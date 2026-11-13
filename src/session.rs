//! 会话：代理运行时、运行状态，以及事件到界面的桥接。
//!
//! 运行在专用的 **Tokio** 运行时上执行：**HTTP** 客户端需要该运行时驱动 **I/O**，
//! 而界面执行器不提供。运行期间代理从会话移入运行任务，只允许串行运行；
//! 事件经无界通道回送到界面执行器上的任务，再写入对话区。
//!
//! 审核：每次运行创建独立的审批门与挂起表，人工决策由界面通过
//! [`Session::resolve_approval`] 回传；取消时把所有挂起审批解析为拒绝。

use std::collections::HashMap;
use std::sync::Arc;

use agent::approval::{
    AllowRule, ApprovalDecision, ApprovalHandle, ApprovalPolicy, ApprovalRequest, DirectoryGrant,
    PolicyGate, allow_rule_for,
};
use agent::error::AgentError;
use agent::mcp::McpServerConfig;
use agent::memory::Step;
use agent::metrics::Timing;
use agent::metrics::TokenUsage;
use agent::model::{Model, ModelBuilder};
use agent::openai::{HttpModelOptions, list_models};
use agent::persist::ContextSnapshot;
use agent::skill::{self, LoadSkillTool, Skill};
use agent::tools::{BuiltinOptions, ImageOptions, JobOptions, SearchOptions, builtin_tools};
use agent::{Agent, Event, Interrupt, RunResult, RunState};
use gpui_kit::*;
use serde_json::Value;
use tokio::runtime::Runtime;

use crate::config::{Config, FeatureKind, Provider};
use crate::conversation::{ChatRole, Conversation};
use crate::session_store::SessionFile;

/// 会话运行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// 空闲
    Idle,
    /// 正在准备运行（装配工具、连接外部能力）
    Preparing,
    /// 正在运行
    Running,
    /// 等待用户审批
    AwaitingApproval,
    /// 已请求停止，等待当前步骤结束
    Cancelling,
    /// 缺少提供商密钥
    Unconfigured,
    /// 尚未选择模型
    Unselected,
    /// 最近一次运行失败
    Failed,
}

impl Status {
    /// 状态栏展示文本
    pub fn label(self) -> &'static str {
        match self {
            Self::Idle => "就绪",
            Self::Preparing => "准备中",
            Self::Running => "运行中",
            Self::AwaitingApproval => "等待确认",
            Self::Cancelling => "正在取消",
            Self::Unconfigured => "未配置密钥",
            Self::Unselected => "未选择模型",
            Self::Failed => "运行失败",
        }
    }

    /// 是否处于运行中或正在取消
    pub fn is_busy(self) -> bool {
        matches!(self, Self::Preparing | Self::Running | Self::AwaitingApproval | Self::Cancelling)
    }
}

/// 会话：持有代理与运行状态
pub struct Session {
    config: Config,
    conversation: Entity<Conversation>,
    /// 运行时代理所在运行时，首次运行前为 [`None`]
    runtime: Option<Runtime>,
    /// 运行期间的中断标记
    interrupt: Option<Interrupt>,
    /// 运行期间的审批句柄
    approval: Option<ApprovalHandle>,
    /// 挂起中的审批请求
    pending: HashMap<String, ApprovalRequest>,
    /// 当前会话的目录授权（后续随会话持久化）
    grants: Vec<DirectoryGrant>,
    /// 当前会话的「始终允许」规则
    always_allow: Vec<AllowRule>,
    /// 当前会话的功能状态；键为 `tool:name` 等。
    features: std::collections::BTreeMap<String, bool>,
    /// 会话累计 **Token** 用量
    usage: TokenUsage,
    /// 按模型拆分的 **Token** 用量
    usage_by_model: std::collections::BTreeMap<String, TokenUsage>,
    /// 最近一次运行是否正常完成（用于语音播报等收尾动作）。
    last_run_ok: bool,
    /// 本次运行使用的模型，用于用量归属。
    run_model: Option<String>,
    /// 当前细粒度活动描述（思考、调用工具等）。
    activity: Option<String>,
    /// 工具自身消耗的 **Token**（生成类模型），跨运行累计。
    extra_usage: TokenUsage,
    status: Status,
    /// 当前会话标识符
    session_id: String,
    /// 会话级提供商覆盖；为空时跟随全局配置
    provider: Option<String>,
    /// 会话级模型覆盖；为空时跟随全局配置
    model: Option<String>,
    /// 已完成的执行上下文
    context: ContextSnapshot,
    /// 是否把配置写回磁盘（测试禁用，避免污染本机配置）
    persist_config: bool,
    /// 测试注入的模型构造器
    model_builder: Option<ModelBuilder>,
}

impl Session {
    /// 构造会话；此时不创建运行时与代理
    pub fn new(config: Config, conversation: Entity<Conversation>) -> Self {
        let mut config = config;
        config.normalise();
        Self {
            config,
            conversation,
            runtime: None,
            interrupt: None,
            approval: None,
            pending: HashMap::new(),
            grants: Vec::new(),
            always_allow: Vec::new(),
            features: std::collections::BTreeMap::new(),
            usage: TokenUsage::default(),
            usage_by_model: std::collections::BTreeMap::new(),
            last_run_ok: false,
            run_model: None,
            activity: None,
            extra_usage: TokenUsage::default(),
            status: Status::Idle,
            session_id: String::new(),
            provider: None,
            model: None,
            context: ContextSnapshot::default(),
            persist_config: true,
            model_builder: None,
        }
    }

    /// 当前会话标识符。
    #[allow(dead_code)]
    pub fn id(&self) -> &str {
        &self.session_id
    }

    /// 会话当前使用的提供商与模型；优先会话级覆盖，不可用时退回全局选择。
    pub fn selection(&self) -> Option<(&str, &str)> {
        if let (Some(provider), Some(model)) = (&self.provider, &self.model)
            && let Some(entry) = self.config.provider_named(provider)
            && entry.models.iter().any(|candidate| candidate.name == *model && candidate.enabled)
        {
            return Some((provider.as_str(), model.as_str()));
        }
        self.config.selection()
    }

    /// 运行前提缺失时的状态：缺少密钥或未选择模型。
    ///
    /// 以 [`Self::selection`] 的结果为准，避免预检与真正使用的模型不是同一个；
    /// 会话级提供商被删除或模型被停用时，selection 已经退回全局选择。
    fn blocking_reason(&self) -> Option<Status> {
        let Some((provider, _model)) = self.selection() else {
            return Some(config_blocking_reason(&self.config).unwrap_or(Status::Unselected));
        };
        match self.config.provider_named(provider) {
            Some(entry) if entry.is_configured() => None,
            _ => Some(Status::Unconfigured),
        }
    }

    /// 导出执行上下文，供会话文件保存。
    pub fn context(&self) -> &ContextSnapshot {
        &self.context
    }

    /// 用会话文件中的状态替换当前会话；代理在下一次运行前重建。
    pub fn load_state(&mut self, file: &SessionFile, cx: &mut Context<Self>) {
        self.session_id = file.id.clone();
        self.provider = file.provider.clone();
        self.model = file.model.clone();
        self.grants = file.grants.clone();
        self.always_allow = file.rules.clone();
        self.features = file.features.clone();
        self.context = file.context.clone();
        // 会话文件只存总量；工具用量 = 总量 − 上下文里模型步骤的用量。
        self.extra_usage = file.usage.saturating_sub(steps_usage(&self.context.steps));
        self.usage = file.usage;
        self.usage_by_model = file.usage_by_model.clone();
        self.interrupt = None;
        self.approval = None;
        self.pending.clear();
        self.status = Status::Idle;
        cx.notify();
    }

    /// 记住当前会话，供下次启动恢复。
    pub fn remember_session(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.config.storage.last_session == id {
            return;
        }
        self.config.storage.last_session = id.to_owned();
        if self.persist_config {
            let _ = self.config.save();
        }
        cx.notify();
    }

    /// 清空执行上下文、对话以外的会话状态保持不变。
    pub fn clear_context(&mut self, cx: &mut Context<Self>) {
        self.context = ContextSnapshot::default();
        self.usage = TokenUsage::default();
        self.usage_by_model.clear();
        self.extra_usage = TokenUsage::default();
        self.status = Status::Idle;
        cx.notify();
    }

    /// 设置是否把配置写回磁盘。
    pub fn set_config_persistence(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.persist_config = enabled;
        cx.notify();
    }

    /// 是否把配置写回磁盘；测试注入的会话不会污染真实配置。
    pub fn persists_config(&self) -> bool {
        self.persist_config
    }

    /// 替换模型构造器；供测试注入脚本模型。
    #[cfg(test)]
    pub fn set_model_builder(&mut self, builder: ModelBuilder, cx: &mut Context<Self>) {
        self.model_builder = Some(builder);
        cx.notify();
    }

    /// 当前运行状态
    pub fn status(&self) -> Status {
        self.status
    }

    /// 仅供测试：直接设置状态，用来构造「运行中」的界面场景。
    #[cfg(test)]
    pub(crate) fn set_status_for_test(&mut self, status: Status, cx: &mut Context<Self>) {
        self.status = status;
        cx.notify();
    }

    /// 当前配置
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// 会话累计 **Token** 用量
    pub fn usage(&self) -> TokenUsage {
        self.usage
    }

    /// 按模型拆分的 **Token** 用量。
    pub fn usage_by_model(&self) -> &std::collections::BTreeMap<String, TokenUsage> {
        &self.usage_by_model
    }

    /// 最近一次运行是否正常完成。
    pub fn last_run_ok(&self) -> bool {
        self.last_run_ok
    }

    /// 状态栏展示文本：运行中优先显示当前活动。
    pub fn status_text(&self) -> String {
        match self.status {
            Status::Preparing | Status::Running => {
                self.activity.clone().unwrap_or_else(|| self.status.label().to_owned())
            }
            _ => self.status.label().to_owned(),
        }
    }

    /// 当前会话的目录授权
    pub fn grants(&self) -> &[DirectoryGrant] {
        &self.grants
    }

    /// 当前会话的「始终允许」规则
    pub fn always_allow(&self) -> &[AllowRule] {
        &self.always_allow
    }

    /// 替换目录授权；下一轮运行生效。
    pub fn set_grants(&mut self, grants: Vec<DirectoryGrant>, cx: &mut Context<Self>) {
        self.grants = grants;
        cx.notify();
    }

    /// 当前会话的功能状态集合。
    pub fn feature_overrides(&self) -> &std::collections::BTreeMap<String, bool> {
        &self.features
    }

    /// 功能是否可见（全局「显示」开关）。
    pub fn feature_visible(&self, kind: crate::config::FeatureKind, name: &str) -> bool {
        self.config.feature_prefs.is_visible(kind, name)
    }

    /// 解析功能当前是否启用：隐藏 → 禁用；会话记录优先；否则用全局默认值。
    pub fn feature_state(&self, kind: crate::config::FeatureKind, name: &str) -> bool {
        if !self.feature_visible(kind, name) {
            return false;
        }
        if kind == crate::config::FeatureKind::Tool
            && let Some(tool) = crate::config::MediaTool::from_tool_name(name)
            && !self.config.media.ready(&self.config, tool)
        {
            return false;
        }
        let key = format!("{}:{name}", kind.prefix());
        self.features
            .get(&key)
            .copied()
            .unwrap_or_else(|| self.config.feature_prefs.default_state(kind, name))
    }

    /// 清除会话级覆盖，让全局默认值重新生效（用于重新显示功能）。
    pub fn clear_feature_override(
        &mut self,
        kind: crate::config::FeatureKind,
        name: &str,
        cx: &mut Context<Self>,
    ) {
        let key = format!("{}:{name}", kind.prefix());
        if self.features.remove(&key).is_some() {
            cx.notify();
        }
    }

    /// 设置会话级功能状态；与全局默认值相同时删除记录，让后续修改默认值立即生效。
    pub fn set_feature_state(
        &mut self,
        kind: crate::config::FeatureKind,
        name: &str,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        let key = format!("{}:{name}", kind.prefix());
        if enabled == self.config.feature_prefs.default_state(kind, name) {
            self.features.remove(&key);
        } else {
            self.features.insert(key, enabled);
        }
        cx.notify();
    }

    /// 替换「始终允许」规则；下一轮运行生效。
    #[allow(dead_code)]
    pub fn set_always_allow(&mut self, rules: Vec<AllowRule>, cx: &mut Context<Self>) {
        self.always_allow = rules;
        cx.notify();
    }

    /// 修改配置并立即生效；会按持久化开关写回配置文件。
    pub fn update_config(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Config)) {
        let mut config = self.config.clone();
        f(&mut config);
        config.normalise();
        if self.persist_config {
            let _ = config.save();
        }
        self.apply_config(config, cx);
    }

    /// 应用配置，并同步到已构建的代理
    pub fn apply_config(&mut self, config: Config, cx: &mut Context<Self>) {
        let mut config = config;
        config.normalise();
        self.config = config;
        if !self.status.is_busy()
            && self.blocking_reason().is_none()
            && matches!(self.status, Status::Unconfigured | Status::Unselected)
        {
            self.status = Status::Idle;
        }
        cx.notify();
    }

    /// 拉取指定提供商的模型列表；结果由调用方决定去向
    pub fn fetch_models(
        &mut self,
        provider: Provider,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<String>, String>> {
        if self.runtime.is_none() {
            match Runtime::new() {
                Ok(runtime) => self.runtime = Some(runtime),
                Err(error) => {
                    let reason = error.to_string();
                    return cx.spawn(async move |_, _| Err(reason));
                }
            }
        }
        // 列表接口不使用模型标识
        let options = options_of(&provider, "");
        let (sender, receiver) = tokio::sync::oneshot::channel();
        if let Some(runtime) = self.runtime.as_ref() {
            runtime.handle().spawn(async move {
                let _ = sender.send(list_models(&options).await);
            });
        }

        cx.spawn(async move |_, _| match receiver.await {
            Ok(result) => result.map_err(|error| error.to_string()),
            Err(_) => Err("拉取任务被中断".to_owned()),
        })
    }

    /// 切换模型：更新配置；下一轮运行生效。
    /// 只切换当前会话使用的模型；全局默认模型在偏好里修改。
    pub fn select_model(&mut self, provider: &str, model: &str, cx: &mut Context<Self>) {
        let Some(entry) = self.config.provider_named(provider) else {
            return;
        };
        if !entry.models.iter().any(|candidate| candidate.name == model && candidate.enabled) {
            return;
        }
        self.provider = Some(provider.to_owned());
        self.model = Some(model.to_owned());
        cx.notify();
    }

    /// 请求停止当前运行；中断在步骤边界生效，挂起的审批立即拒绝。
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        let Some(interrupt) = &self.interrupt else {
            return;
        };
        interrupt.cancel();
        if let Some(approval) = &self.approval {
            approval.cancel();
            approval.deny_all();
        }
        self.status = Status::Cancelling;
        cx.notify();
    }

    /// 回传一次审批决定。
    pub fn resolve_approval(
        &mut self,
        id: &str,
        decision: ApprovalDecision,
        cx: &mut Context<Self>,
    ) {
        if decision == ApprovalDecision::AllowAlways
            && let Some(request) = self.pending.get(id)
            && let Some(rule) = allow_rule_for(request)
            && !self.always_allow.iter().any(|existing| existing == &rule)
        {
            self.always_allow.push(rule);
        }
        if let Some(approval) = &self.approval {
            approval.resolve(id, decision);
        }
        cx.notify();
    }

    /// 清空代理记忆，开始新的会话上下文
    #[allow(dead_code)]
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.context = ContextSnapshot::default();
        self.usage = TokenUsage::default();
        self.usage_by_model.clear();
        self.status = Status::Idle;
        cx.notify();
    }

    /// 提交任务：空闲且具备运行前提时启动一次运行，否则只更新状态。
    ///
    /// 运行准备（构建模型、连接 MCP、装配工具）在运行时上异步完成，界面不会阻塞。
    pub fn submit(&mut self, task: String, cx: &mut Context<Self>) {
        if self.status.is_busy() {
            return;
        }
        if let Some(status) = self.blocking_reason() {
            self.status = status;
            cx.notify();
            return;
        }
        let Some(model_options) = self.model_options() else {
            self.status = Status::Failed;
            cx.notify();
            return;
        };
        if self.runtime.is_none() {
            match Runtime::new() {
                Ok(runtime) => self.runtime = Some(runtime),
                Err(_) => {
                    self.status = Status::Failed;
                    cx.notify();
                    return;
                }
            }
        }

        // 审批通道：门与运行任务分持发送端，界面侧从接收端读取。
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let handle = ApprovalHandle::new();
        let mut gate =
            PolicyGate::new(self.approval_policy(), handle.clone(), Some(sender.clone()));
        if let Some(reviewer_options) = self.reviewer_options() {
            gate = gate.with_reviewer(agent::openai::build_model(reviewer_options));
        }
        let gate = Arc::new(gate);

        let interrupt = Interrupt::default();
        let model_builder = self.model_builder.clone();
        let options = self.builtin_options();
        // 会话级启停：隐藏的功能不加载，其余按「会话记录 → 全局默认值」解析。
        let servers: Vec<McpServerConfig> = self
            .config
            .mcp_servers
            .iter()
            .filter(|server| self.feature_state(FeatureKind::Mcp, &server.name))
            .cloned()
            .map(|mut server| {
                // 会话级状态已经过滤过；`enabled` 只是旧配置字段，连接时不再参与判断。
                server.enabled = true;
                server
            })
            .collect();
        let skills: Vec<Skill> = skill::discover(&self.config.skill_dirs())
            .into_iter()
            .filter(|skill| self.feature_state(FeatureKind::Skill, &skill.name))
            .collect();
        // 通用提示词：跟随系统提示进入上下文，每次运行重建一次。
        let instructions = self.config.ui.system_prompt.trim().to_owned();
        // 工作区是会话级授权：每次运行重建代理时告诉模型当前目录边界。
        let workspace = self.grants.clone();
        let context = self.context.clone();
        self.interrupt = Some(interrupt.clone());
        self.approval = Some(handle);
        self.pending.clear();
        self.status = Status::Preparing;
        self.last_run_ok = false;
        self.run_model = Some(model_options.model_id.clone());
        self.activity = Some("连接中…".to_owned());
        // 工具用量跨运行累计，这里不能清零；否则每次运行结束都会少算之前的工具消耗。
        cx.notify();

        let Some(runtime) = &self.runtime else {
            self.status = Status::Failed;
            cx.notify();
            return;
        };
        let run = runtime.handle().spawn(async move {
            let model: Box<dyn Model> = match model_builder {
                Some(builder) => builder(),
                None => agent::openai::build_model(model_options),
            };
            let tools = prepare_tools(&options, &servers, &skills).await;
            let mut agent = match Agent::new(model, tools) {
                Ok(agent) => {
                    let agent = agent
                        .with_skills(skills)
                        .with_workspace(workspace)
                        .with_approval_gate(gate);
                    if instructions.is_empty() {
                        agent
                    } else {
                        agent.with_instructions(instructions)
                    }
                }
                Err(error) => return (context, Err(error)),
            };
            agent.restore_context(context.clone());
            if interrupt.is_cancelled() {
                agent.close_events();
                return (
                    context,
                    Ok(RunResult {
                        output: Value::Null,
                        state: RunState::Cancelled,
                        usage: TokenUsage::default(),
                        timing: Timing::start(),
                    }),
                );
            }
            let result = agent
                .run(task, move |event| {
                    let _ = sender.send(event);
                })
                .await;
            // 审核门也持有发送端：运行结束即释放，消费侧才能看到事件流终止。
            agent.close_events();
            (agent.snapshot(), result)
        });

        let conversation = self.conversation.clone();
        cx.spawn(async move |this, cx| {
            while let Some(event) = receiver.recv().await {
                apply(&conversation, &event, cx);
                this.update(cx, |session, cx| session.handle_effect(&event, cx)).ok();
            }
            match run.await {
                Ok((snapshot, result)) => {
                    this.update(cx, |session, cx| session.finish(snapshot, result, cx)).ok();
                }
                Err(_) => {
                    // 运行任务未能归还结果，此会话不再可运行（进程启用 panic=abort，实际难以触发）
                    this.update(cx, |session, cx| session.fail(cx)).ok();
                }
            }
        })
        .detach();
    }

    /// 根据事件更新会话状态与挂起表。
    fn handle_effect(&mut self, event: &Event, cx: &mut Context<Self>) {
        // 步骤完成时即时更新用量；运行结束时再用上下文总量校准。
        if let Event::Step(step) = event
            && let Some(usage) = step.usage()
        {
            self.usage = self.usage + usage;
        }
        if self.status == Status::Preparing {
            self.status = Status::Running;
            if self.activity.as_deref().is_none_or(|value| value == "连接中…") {
                self.activity = Some("思考中…".to_owned());
            }
        }
        if let Event::Step(_) = event {
            self.activity = Some("思考中…".to_owned());
        }
        if let Event::Activity(activity) = event {
            match activity {
                agent::Activity::Thinking {
                    ..
                }
                | agent::Activity::ReasoningDelta(_) => {
                    self.activity = Some("思考中…".to_owned());
                }
                agent::Activity::ToolStarted {
                    call,
                    ..
                } => {
                    self.activity = Some(format!("调用 {} 中…", call.name));
                }
                agent::Activity::ToolFinished {
                    ..
                } => {
                    self.activity = Some("整理结果…".to_owned());
                }
                agent::Activity::ApprovalRequested(request) => {
                    self.pending.insert(request.id.clone(), request.clone());
                    if request.manual {
                        self.status = Status::AwaitingApproval;
                    } else {
                        self.activity = Some("AI 审核中…".to_owned());
                    }
                }
                agent::Activity::ApprovalResolved {
                    id,
                    ..
                } => {
                    self.pending.remove(id);
                    if self.status == Status::AwaitingApproval && self.pending.is_empty() {
                        self.status = Status::Running;
                    }
                    self.activity = Some("继续运行…".to_owned());
                }
                agent::Activity::SkillLoaded {
                    name,
                } => {
                    self.activity = Some(format!("加载技能 {name}…"));
                }
                agent::Activity::PlanStarted {
                    ..
                } => {
                    self.activity = Some("规划中…".to_owned());
                }
                agent::Activity::PlanFinished {
                    ..
                } => {
                    self.activity = Some("执行计划…".to_owned());
                }
                agent::Activity::Images(_) => {}
                agent::Activity::ToolUsage {
                    model,
                    usage,
                } => {
                    self.usage = self.usage + *usage;
                    let entry = self.usage_by_model.entry(model.clone()).or_default();
                    *entry = *entry + *usage;
                    self.extra_usage = self.extra_usage + *usage;
                }
            }
        }
        cx.notify();
    }

    /// 审核模型选项：复用当前模型，或单独指定。
    fn reviewer_options(&self) -> Option<HttpModelOptions> {
        let reviewer = &self.config.approval.reviewer;
        if reviewer.use_current || reviewer.provider.trim().is_empty() {
            return self.model_options();
        }
        if reviewer.model.trim().is_empty() {
            return self.model_options();
        }
        match self.config.provider_named(reviewer.provider.trim()) {
            Some(provider) => Some(options_of(provider, reviewer.model.trim())),
            None => {
                // 单独指定的提供商被删掉时退回当前模型，而不是让审核模型凭空消失。
                tracing::warn!(
                    "审核模型 {} · {} 已不存在，退回当前对话模型",
                    reviewer.provider,
                    reviewer.model
                );
                self.model_options()
            }
        }
    }

    /// 组装本次运行的审核策略。
    fn approval_policy(&self) -> ApprovalPolicy {
        let mut grants = self.grants.clone();
        // Skill 目录始终以只读方式授权，正文与资源不需要逐次审批。
        for dir in self.config.skill_dirs() {
            grants.push(DirectoryGrant::new(dir, agent::tool::Access::Read));
        }
        ApprovalPolicy {
            mode: self.config.approval.mode,
            grants,
            allow_network: self.config.approval.allow_network,
            rules: self.always_allow.clone(),
            base_dir: None,
        }
    }

    /// 组装本次运行的工具选项。
    fn builtin_options(&self) -> BuiltinOptions {
        let search = self.search_options();
        // 内置工具按会话级状态禁用；隐藏的工具也视为禁用。
        let mut names: Vec<String> = builtin_tools(&BuiltinOptions::default())
            .iter()
            .map(|tool| tool.spec().name.clone())
            .collect();
        // 搜索与生成工具只在配置后注册，这里单独补上它们的开关。
        for name in [
            "web_search",
            agent::tools::media::GENERATE_IMAGE,
            agent::tools::media::GENERATE_VIDEO,
            agent::tools::media::GENERATE_MUSIC,
        ] {
            if !names.iter().any(|existing| existing == name) {
                names.push(name.to_owned());
            }
        }
        let disabled: Vec<String> =
            names.into_iter().filter(|name| !self.feature_state(FeatureKind::Tool, name)).collect();
        BuiltinOptions {
            search,
            disabled,
            image: self.image_options(),
            image_job: self.image_job_options(),
            video: self.job_options(crate::config::MediaTool::Video),
            music: self.job_options(crate::config::MediaTool::Music),
        }
    }

    /// 绘图工具配置（OpenAI 兼容同步接口）；百炼与显式轮询配置走异步。
    fn image_options(&self) -> Option<ImageOptions> {
        let media = &self.config.media.image;
        if !media.is_configured() {
            return None;
        }
        let provider = self.config.provider_named(media.provider.trim())?;
        if !provider.is_configured() {
            return None;
        }
        if is_dashscope(provider) || !media.poll_url.trim().is_empty() {
            return None;
        }
        let endpoint = if media.endpoint.trim().is_empty() {
            format!("{}/images/generations", provider.endpoint().trim_end_matches('/'))
        } else {
            media.endpoint.trim().to_owned()
        };
        let mut headers = provider.headers.clone();
        for (key, value) in &media.headers {
            headers.insert(key.clone(), value.clone());
        }
        Some(ImageOptions {
            endpoint,
            api_key: provider.api_key.clone(),
            model: media.model.clone(),
            headers,
        })
    }

    /// 异步绘图配置：百炼原生接口或显式轮询配置。
    fn image_job_options(&self) -> Option<JobOptions> {
        let media = &self.config.media.image;
        if !media.is_configured() {
            return None;
        }
        let provider = self.config.provider_named(media.provider.trim())?;
        let dashscope = is_dashscope(provider);
        if !dashscope && media.poll_url.trim().is_empty() {
            return None;
        }
        let defaults = dashscope.then(|| dashscope_image_defaults(provider, &media.model));
        self.media_job(provider, media, "AI 绘图", defaults)
    }

    /// 视频 / 音乐任务配置。
    fn job_options(&self, kind: crate::config::MediaTool) -> Option<JobOptions> {
        let media = self.config.media.tool(kind);
        if !media.is_configured() {
            return None;
        }
        let provider = self.config.provider_named(media.provider.trim())?;
        let dashscope = is_dashscope(provider);
        if media.endpoint.trim().is_empty() && !dashscope {
            return None;
        }
        let defaults = if dashscope && matches!(kind, crate::config::MediaTool::Video) {
            Some(dashscope_video_defaults(provider, &media.model))
        } else {
            None
        };
        self.media_job(provider, media, kind.label(), defaults)
    }

    /// 由配置与默认适配器组装异步任务选项。
    fn media_job(
        &self,
        provider: &Provider,
        media: &crate::config::MediaToolConfig,
        name: &str,
        defaults: Option<MediaDefaults>,
    ) -> Option<JobOptions> {
        let mut headers = provider.headers.clone();
        for (key, value) in &media.headers {
            headers.insert(key.clone(), value.clone());
        }
        let (endpoint, poll_url, body, id_path, status_path, done_value, result_path) =
            match defaults {
                Some(defaults) => {
                    headers.insert("X-DashScope-Async".to_owned(), "enable".to_owned());
                    (
                        if media.endpoint.trim().is_empty() {
                            defaults.endpoint
                        } else {
                            media.endpoint.trim().to_owned()
                        },
                        if media.poll_url.trim().is_empty() {
                            defaults.poll_url
                        } else {
                            media.poll_url.trim().to_owned()
                        },
                        media.body_template.clone().or(Some(defaults.body)),
                        defaults.id_path,
                        defaults.status_path,
                        defaults.done_value,
                        defaults.result_path,
                    )
                }
                None => (
                    media.endpoint.trim().to_owned(),
                    media.poll_url.clone(),
                    media.body_template.clone(),
                    media.id_path.clone(),
                    media.status_path.clone(),
                    media.done_value.clone(),
                    media.result_path.clone(),
                ),
            };
        Some(JobOptions {
            name: name.to_owned(),
            endpoint,
            api_key: provider.api_key.clone(),
            model: media.model.clone(),
            body_template: body,
            poll_url,
            id_path,
            status_path,
            done_value,
            result_path,
            interval: std::time::Duration::from_millis(media.interval_ms.max(200)),
            timeout: std::time::Duration::from_secs(media.timeout_s.max(5)),
            headers,
        })
    }

    /// 由搜索设置与提供商推导搜索接口配置。
    fn search_options(&self) -> Option<SearchOptions> {
        let search = &self.config.search;
        if !search.enabled {
            return None;
        }
        let provider = if search.provider.trim().is_empty() {
            self.config.provider()?
        } else {
            self.config.provider_named(search.provider.trim())?
        };
        let endpoint = if search.endpoint.trim().is_empty() {
            format!("{}/tools/search", provider.endpoint().trim_end_matches('/'))
        } else {
            search.endpoint.trim().to_owned()
        };
        let api_key = if search.api_key.trim().is_empty() {
            provider.api_key.clone()
        } else {
            search.api_key.trim().to_owned()
        };
        Some(SearchOptions {
            endpoint,
            api_key,
            max_results: search.max_results,
        })
    }

    /// 由当前配置构造模型选项；缺少提供商或模型时返回 [`None`]
    fn model_options(&self) -> Option<HttpModelOptions> {
        let (provider, model) = self.selection()?;
        let provider = self.config.provider_named(provider)?;
        Some(options_of(provider, model))
    }

    /// 运行结束：保存上下文、更新状态，失败时把原因写入对话区
    fn finish(
        &mut self,
        snapshot: ContextSnapshot,
        result: Result<RunResult, AgentError>,
        cx: &mut Context<Self>,
    ) {
        // 上一次上下文里的模型用量；本次新增部分才是本轮运行的用量。
        let previous_context_usage = steps_usage(&self.context.steps);
        self.context = snapshot;
        let context_usage = steps_usage(&self.context.steps);
        // 总量 = 上下文里累积的模型用量 + 累积的工具用量（生成类模型）。
        self.usage = context_usage + self.extra_usage;
        self.interrupt = None;
        self.approval = None;
        self.pending.clear();
        self.status = Status::Idle;
        self.activity = None;
        self.last_run_ok = false;
        self.conversation.update(cx, |conversation, cx| conversation.end_reasoning(cx));
        let delta = context_usage.saturating_sub(previous_context_usage);
        if delta.total() > 0 {
            let model = self
                .run_model
                .take()
                .or_else(|| self.selection().map(|(_, model)| model.to_owned()))
                .unwrap_or_else(|| "未知模型".to_owned());
            let entry = self.usage_by_model.entry(model).or_default();
            *entry = *entry + delta;
        }
        match result {
            Ok(result) => {
                if result.state == RunState::Cancelled {
                    self.conversation.update(cx, |conversation, cx| {
                        conversation.push(ChatRole::Assistant, "（已取消）", cx);
                    });
                } else {
                    self.last_run_ok = true;
                }
            }
            Err(error) => {
                self.status = Status::Failed;
                self.conversation.update(cx, |conversation, cx| {
                    conversation.push(ChatRole::Assistant, format!("运行失败：{error}"), cx);
                });
            }
        }
        cx.notify();
    }

    /// 运行任务异常结束
    fn fail(&mut self, cx: &mut Context<Self>) {
        self.interrupt = None;
        self.approval = None;
        self.pending.clear();
        self.status = Status::Failed;
        cx.notify();
    }
}

/// 装配本次运行的工具：内置工具 + 已连接的 MCP 工具。
async fn prepare_tools(
    options: &BuiltinOptions,
    servers: &[McpServerConfig],
    skills: &[Skill],
) -> agent::tool::ToolSet {
    let mut tools = builtin_tools(options);
    if !skills.is_empty() {
        let _ = tools.insert(LoadSkillTool::new(skills.to_vec()));
    }
    let connected = agent::mcp::connect(servers).await;
    for tool in connected.tools {
        let _ = tools.insert(tool);
    }
    for status in connected.statuses {
        if !status.connected {
            tracing::warn!("MCP 服务器 {} 不可用：{:?}", status.name, status.error);
        }
    }
    tools
}

/// 运行前提缺失时的状态：缺少密钥或未选择模型。
fn config_blocking_reason(config: &Config) -> Option<Status> {
    if !config.provider().is_some_and(Provider::is_configured) {
        return Some(Status::Unconfigured);
    }
    if config.model().is_none() {
        return Some(Status::Unselected);
    }
    None
}

/// 百炼原生异步任务的默认字段。
struct MediaDefaults {
    endpoint: String,
    poll_url: String,
    body: serde_json::Value,
    id_path: String,
    status_path: String,
    done_value: String,
    result_path: String,
}

/// 是否百炼（DashScope）提供商。
fn is_dashscope(provider: &Provider) -> bool {
    provider.endpoint().contains("dashscope.aliyuncs.com")
}

/// 接口的服务根地址（scheme + host）。
fn service_root(endpoint: &str) -> String {
    let trimmed = endpoint.trim_end_matches('/');
    let Some(scheme) = trimmed.find("://") else {
        return trimmed.to_owned();
    };
    let rest = &trimmed[scheme + 3..];
    match rest.find('/') {
        Some(path) => trimmed[..scheme + 3 + path].to_owned(),
        None => trimmed.to_owned(),
    }
}

/// 百炼文生图默认适配。
fn dashscope_image_defaults(provider: &Provider, model: &str) -> MediaDefaults {
    let root = service_root(&provider.endpoint());
    MediaDefaults {
        endpoint: format!("{root}/api/v1/services/aigc/text2image/image-synthesis"),
        poll_url: format!("{root}/api/v1/tasks/{{{{id}}}}"),
        body: serde_json::json!({
            "model": model,
            "input": { "prompt": "{{prompt}}" },
            "parameters": { "size": "1024*1024", "n": 1 },
        }),
        id_path: "output.task_id".to_owned(),
        status_path: "output.task_status".to_owned(),
        done_value: "SUCCEEDED".to_owned(),
        result_path: "output.results.0.url".to_owned(),
    }
}

/// 百炼文生视频默认适配。
fn dashscope_video_defaults(provider: &Provider, model: &str) -> MediaDefaults {
    let root = service_root(&provider.endpoint());
    MediaDefaults {
        endpoint: format!("{root}/api/v1/services/aigc/video-generation/video-synthesis"),
        poll_url: format!("{root}/api/v1/tasks/{{{{id}}}}"),
        body: serde_json::json!({
            "model": model,
            "input": { "prompt": "{{prompt}}" },
            "parameters": { "size": "1280*720" },
        }),
        id_path: "output.task_id".to_owned(),
        status_path: "output.task_status".to_owned(),
        done_value: "SUCCEEDED".to_owned(),
        result_path: "output.video_url".to_owned(),
    }
}

/// 上下文里各步骤消耗的模型用量之和。
fn steps_usage(steps: &[Step]) -> TokenUsage {
    steps.iter().filter_map(Step::usage).fold(TokenUsage::default(), |total, usage| total + usage)
}

/// 由服务商与模型标识构造模型选项。
fn options_of(provider: &Provider, model: &str) -> HttpModelOptions {
    let entry = provider.models.iter().find(|entry| entry.name == model);
    let mut options = HttpModelOptions::new(model.to_owned(), provider.api_key.clone())
        .base_url(provider.endpoint())
        .tool_choice(provider.tool_choice)
        .protocol(provider.protocol)
        .headers(provider.headers.clone());
    for (key, value) in &provider.extra_body {
        options = options.extra(key.clone(), value.clone());
    }
    if let Some(max_tokens) = provider.max_tokens {
        options = options.max_tokens(max_tokens);
    }
    // 按模型声明的输入模态裁剪消息，纯文本模型不会被塞进图片或音频块。
    if let Some(entry) = entry {
        options = options.input_modalities(entry.input.clone());
    }
    options
}

/// 把运行事件应用到对话区。
fn apply<C: AppContext>(conversation: &Entity<Conversation>, event: &Event, cx: &mut C) {
    match event {
        Event::Delta(text) => {
            conversation
                .update(cx, |conversation, cx| conversation.stream(ChatRole::Assistant, text, cx));
        }
        Event::Step(step) => {
            let text = render_step(step);
            conversation.update(cx, |conversation, cx| {
                conversation.end_stream();
                if let Some(text) = text {
                    conversation.push(ChatRole::Assistant, text, cx);
                }
            });
        }
        Event::Activity(activity) => {
            apply_activity(conversation, activity, cx);
        }
    }
}

/// 把一条细粒度活动应用到对话区。
fn apply_activity<C: AppContext>(
    conversation: &Entity<Conversation>,
    activity: &agent::Activity,
    cx: &mut C,
) {
    use agent::Activity;
    match activity {
        Activity::ReasoningDelta(text) => {
            conversation.update(cx, |conversation, cx| conversation.reasoning(text, cx));
        }
        Activity::ToolStarted {
            call,
            ..
        } => {
            conversation.update(cx, |conversation, cx| {
                conversation.end_reasoning(cx);
                conversation.end_stream();
                conversation.tool_activity(
                    call.id.clone(),
                    call.name.clone(),
                    compact(&call.arguments),
                    cx,
                );
            });
        }
        Activity::ToolFinished {
            call_id,
            ok,
            preview,
            elapsed_ms,
            ..
        } => {
            conversation.update(cx, |conversation, cx| {
                conversation.finish_activity(call_id, *ok, preview.clone(), *elapsed_ms, cx);
            });
        }
        Activity::ApprovalRequested(request) => {
            conversation.update(cx, |conversation, cx| {
                conversation.approval_requested(request, cx);
            });
        }
        Activity::ApprovalResolved {
            id,
            decision,
            reason,
        } => {
            conversation.update(cx, |conversation, cx| {
                conversation.approval_resolved(id, *decision, reason.clone(), cx);
            });
        }
        Activity::ToolUsage {
            ..
        } => {}
        Activity::Images(paths) => {
            conversation.update(cx, |conversation, cx| {
                conversation.push_images(paths, cx);
            });
        }
        Activity::PlanFinished {
            plan,
            ..
        } => {
            conversation.update(cx, |conversation, cx| conversation.plan(plan.clone(), cx));
        }
        Activity::Thinking {
            ..
        }
        | Activity::PlanStarted {
            ..
        }
        | Activity::SkillLoaded {
            ..
        } => {}
    }
}

/// 把 JSON 参数压成一行摘要。
fn compact(value: &Value) -> String {
    let text = value.to_string();
    agent::text::truncate(&text, 400)
}

/// 步骤文本的行分隔：Markdown 硬换行，逐行保留，避免被解析器折叠为一段
const LINE_BREAK: &str = "  \n";

/// 把一个记忆步骤渲染为对话文本；无需展示的步骤返回 [`None`]
///
/// 工具调用与计划由活动卡片实时展示，这里只负责兜底答案与步骤级错误。
fn render_step(step: &Step) -> Option<String> {
    match step {
        // 任务文本已由输入区写入对话
        Step::Task(_) => None,
        // 最终答案由产出它的动作步骤展示
        Step::FinalAnswer(_) => None,
        // 计划由活动卡片展示
        Step::Planning(_) => None,
        Step::Action(step) => {
            let mut lines = Vec::new();
            if let Some(error) = &step.error {
                lines.push(format!("错误 {error}"));
            }
            if step.is_final_answer
                && let Some(output) = &step.output
                && !answer_was_streamed(step)
            {
                // 最终答案是模型给出的 Markdown，与标题分行后按 Markdown 渲染
                lines.push(format!("最终答案：{LINE_BREAK}{}", render_value(output)));
            }
            (!lines.is_empty()).then(|| lines.join(LINE_BREAK))
        }
    }
}

/// 模型直接用文本作答时，答案已经随流式增量显示在对话里，不再重复一遍。
fn answer_was_streamed(step: &agent::memory::ActionStep) -> bool {
    step.model_output.as_ref().is_some_and(|message| {
        message.content.iter().any(|block| match block {
            agent::message::ContentBlock::Text(text) => !text.trim().is_empty(),
            agent::message::ContentBlock::Image(_) | agent::message::ContentBlock::Audio(_) => {
                false
            }
        })
    })
}

/// 渲染工具输出：字符串直接使用，其余使用 **JSON** 文本
fn render_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LINE_BREAK, Session, Status, config_blocking_reason, dashscope_image_defaults,
        dashscope_video_defaults, is_dashscope, options_of, render_step, service_root,
    };
    use gpui_kit::AppContext as _;

    use crate::config::{Config, Provider};
    use agent::error::{AgentError, ToolError};
    use agent::memory::{ActionStep, FinalAnswerStep, PlanningStep, Step, TaskStep, ToolOutcome};
    use agent::message::ToolCall;
    use agent::metrics::Timing;
    use serde_json::{Value, json};

    /// 按步骤文本的行分隔拼接期望值
    fn lines(lines: &[&str]) -> String {
        lines.join(LINE_BREAK)
    }

    fn provider(name: &str, models: &[&str], api_key: &str) -> Provider {
        let mut provider = Provider::new(name, "https://example.com/v1", api_key);
        provider.models =
            models.iter().map(|model| crate::config::ModelEntry::text(*model)).collect();
        provider
    }

    fn call(name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: format!("call_{name}"),
            name: name.to_owned(),
            arguments,
        }
    }

    fn action(outcomes: Vec<ToolOutcome>) -> ActionStep {
        ActionStep {
            number: 1,
            timing: Timing::start(),
            model_output: None,
            outcomes,
            error: None,
            output: None,
            is_final_answer: false,
        }
    }

    #[test]
    fn status_labels_are_stable() {
        assert_eq!(Status::Idle.label(), "就绪");
        assert_eq!(Status::Preparing.label(), "准备中");
        assert_eq!(Status::Running.label(), "运行中");
        assert_eq!(Status::AwaitingApproval.label(), "等待确认");
        assert_eq!(Status::Cancelling.label(), "正在取消");
        assert_eq!(Status::Unconfigured.label(), "未配置密钥");
        assert_eq!(Status::Unselected.label(), "未选择模型");
        assert_eq!(Status::Failed.label(), "运行失败");
    }

    #[test]
    fn busy_statuses_cover_every_running_state() {
        assert!(Status::Preparing.is_busy());
        assert!(Status::Running.is_busy());
        assert!(Status::AwaitingApproval.is_busy());
        assert!(Status::Cancelling.is_busy());
        assert!(!Status::Idle.is_busy());
        assert!(!Status::Unconfigured.is_busy());
        assert!(!Status::Unselected.is_busy());
        assert!(!Status::Failed.is_busy());
    }

    #[test]
    fn blocking_reasons_distinguish_missing_key_and_missing_model() {
        let mut config = Config::default();
        assert_eq!(config_blocking_reason(&config), Some(Status::Unconfigured));

        config.add_provider(provider("空目录", &[], "secret")).expect("新增应当成功");
        assert_eq!(config_blocking_reason(&config), Some(Status::Unselected));

        config.add_provider(provider("无密钥", &["m1"], "  ")).expect("新增应当成功");
        config.select_model("无密钥", "m1").expect("选中应当成功");
        assert_eq!(config_blocking_reason(&config), Some(Status::Unconfigured));

        config.add_provider(provider("就绪", &["m1"], "secret")).expect("新增应当成功");
        config.select_model("就绪", "m1").expect("选中应当成功");
        assert_eq!(config_blocking_reason(&config), None);
    }

    #[gpui_kit::test]
    fn blocking_reason_follows_the_session_model(cx: &mut gpui_kit::TestAppContext) {
        let conversation = cx.update(|cx| cx.new(crate::conversation::Conversation::new));
        cx.update(|_| {
            let mut config = Config::default();
            config.add_provider(provider("a", &["m-a"], "key")).expect("a");
            config.add_provider(provider("b", &["m-b"], "  ")).expect("b");
            let mut session = Session::new(config, conversation);

            // 会话内切到没有密钥的提供商时，预检必须按会话的选择判断。
            session.provider = Some("b".into());
            session.model = Some("m-b".into());
            assert_eq!(session.blocking_reason(), Some(Status::Unconfigured));

            session.provider = Some("a".into());
            session.model = Some("m-a".into());
            assert_eq!(session.blocking_reason(), None);

            // 会话里的提供商或模型不可用时退回全局选择，而不是把消息挡在门外。
            let global = session
                .config
                .selection()
                .map(|(provider, model)| (provider.to_owned(), model.to_owned()));
            session.provider = Some("deleted".into());
            session.model = Some("m-x".into());
            assert_eq!(session.selection().map(|(p, m)| (p.to_owned(), m.to_owned())), global);
            assert_eq!(session.blocking_reason(), None);

            session.provider = Some("a".into());
            session.model = Some("missing".into());
            assert_eq!(session.selection().map(|(p, m)| (p.to_owned(), m.to_owned())), global);
            assert_eq!(session.blocking_reason(), None);
        });
    }

    #[gpui_kit::test]
    fn reloading_a_session_keeps_tool_usage(cx: &mut gpui_kit::TestAppContext) {
        let conversation = cx.update(|cx| cx.new(crate::conversation::Conversation::new));
        let mut file = crate::session_store::SessionFile::new(None, None);
        // 上下文里模型用了 10 个 Token，总量是 30，其余 20 来自生成类工具。
        file.context.steps.push(Step::Action(ActionStep {
            number: 1,
            timing: Timing::start(),
            model_output: Some(agent::message::Message {
                role: agent::message::Role::Assistant,
                content: vec![agent::message::ContentBlock::Text("ok".into())],
                tool_calls: Vec::new(),
                tool_call_id: None,
                usage: Some(agent::metrics::TokenUsage::new(10, 0)),
            }),
            outcomes: Vec::new(),
            error: None,
            output: Some(json!("ok")),
            is_final_answer: true,
        }));
        file.usage = agent::metrics::TokenUsage::new(30, 0);

        let session = cx.update(|cx| {
            cx.new(|cx| {
                let mut session = Session::new(Config::default(), conversation.clone());
                session.load_state(&file, cx);
                session
            })
        });
        cx.update(|cx| {
            let session = session.read(cx);
            assert_eq!(session.usage().total(), 30);
            assert_eq!(session.extra_usage.total(), 20);
        });
    }

    #[gpui_kit::test]
    fn dashscope_image_uses_the_async_job(cx: &mut gpui_kit::TestAppContext) {
        let conversation = cx.update(|cx| cx.new(crate::conversation::Conversation::new));
        cx.update(|_| {
            let mut config = Config::default();
            let mut provider = Provider::new(
                "bailian",
                "https://dashscope.aliyuncs.com/compatible-mode/v1",
                "key",
            );
            provider.models = vec![crate::config::ModelEntry {
                name: "wanx2.1-t2i-turbo".into(),
                enabled: true,
                input: vec![agent::model::Modality::Text],
                output: vec![agent::model::Modality::Image],
            }];
            config.add_provider(provider).expect("provider");
            config.media.image.provider = "bailian".into();
            config.media.image.model = "wanx2.1-t2i-turbo".into();
            let session = Session::new(config, conversation);
            assert!(session.image_options().is_none(), "百炼不应走同步接口");
            let job = session.image_job_options().expect("async image job");
            assert!(job.endpoint.ends_with("/text2image/image-synthesis"));
            assert_eq!(job.id_path, "output.task_id");
            assert_eq!(job.result_path, "output.results.0.url");
            assert_eq!(job.headers.get("X-DashScope-Async").map(String::as_str), Some("enable"));
        });
    }

    #[test]
    fn dashscope_defaults_use_native_async_endpoints() {
        let provider =
            Provider::new("bailian", "https://dashscope.aliyuncs.com/compatible-mode/v1", "key");
        assert!(is_dashscope(&provider));
        assert_eq!(service_root(&provider.endpoint()), "https://dashscope.aliyuncs.com");

        let image = dashscope_image_defaults(&provider, "wanx2.1-t2i-turbo");
        assert!(image.endpoint.ends_with("/services/aigc/text2image/image-synthesis"));
        assert!(image.poll_url.ends_with("/api/v1/tasks/{{id}}"));
        assert_eq!(image.result_path, "output.results.0.url");
        assert_eq!(image.done_value, "SUCCEEDED");

        let video = dashscope_video_defaults(&provider, "wan2.1-t2v-turbo");
        assert!(video.endpoint.ends_with("/services/aigc/video-generation/video-synthesis"));
        assert_eq!(video.result_path, "output.video_url");
    }

    #[test]
    fn model_options_follow_the_provider() {
        let options = options_of(&provider("local", &[], "secret"), "m1");

        assert_eq!(options.model_id, "m1");
        assert_eq!(options.api_key, "secret");
        assert_eq!(options.base_url, "https://example.com/v1");
    }

    #[test]
    fn model_options_carry_protocol_headers_and_modalities() {
        let mut provider = Provider::new("local", "https://api.example.com/v1", "secret");
        provider.protocol = agent::model::Protocol::AnthropicMessages;
        provider.headers.insert("X-Org".into(), "team".into());
        provider.extra_body.insert("top_p".into(), json!(0.9));
        provider.max_tokens = Some(2048);
        provider.models = vec![crate::config::ModelEntry {
            name: "m1".into(),
            enabled: true,
            input: vec![agent::model::Modality::Text],
            output: vec![agent::model::Modality::Text],
        }];

        let options = options_of(&provider, "m1");

        assert_eq!(options.protocol, agent::model::Protocol::AnthropicMessages);
        assert_eq!(options.headers.get("X-Org").map(String::as_str), Some("team"));
        assert_eq!(options.extras.get("top_p"), Some(&json!(0.9)));
        assert_eq!(options.max_tokens, Some(2048));
        assert_eq!(options.input_modalities, vec![agent::model::Modality::Text]);
    }

    #[test]
    fn model_options_fall_back_to_the_default_endpoint() {
        let provider = Provider::new("local", "  ", "secret");

        assert_eq!(options_of(&provider, "m1").base_url, crate::config::DEFAULT_BASE_URL);
    }

    #[test]
    fn task_and_final_answer_steps_are_not_rendered() {
        assert_eq!(render_step(&Step::Task(TaskStep::from("任务"))), None);
        assert_eq!(
            render_step(&Step::FinalAnswer(FinalAnswerStep {
                output: json!(1)
            })),
            None
        );
    }

    #[test]
    fn tool_calls_are_left_to_the_activity_cards() {
        let step = Step::Action(action(vec![ToolOutcome {
            call: call("search", json!({ "query": "rust" })),
            result: Ok(json!("12 results").into()),
        }]));

        assert_eq!(render_step(&step), None);
    }

    #[test]
    fn tool_failures_are_left_to_the_activity_cards() {
        let step = Step::Action(action(vec![ToolOutcome {
            call: call("search", json!({})),
            result: Err(ToolError::UnknownTool {
                name: "search".into(),
            }),
        }]));

        assert_eq!(render_step(&step), None);
    }

    #[test]
    fn final_answer_is_rendered_within_its_action_step() {
        let mut step = action(Vec::new());
        step.is_final_answer = true;
        step.output = Some(json!("42"));

        assert_eq!(render_step(&Step::Action(step)), Some(lines(&["最终答案：", "42"])));
    }

    #[test]
    fn streamed_text_reply_is_not_repeated_as_the_final_answer() {
        let mut step = action(Vec::new());
        step.is_final_answer = true;
        step.output = Some(json!("你好，我是 Haloeth"));
        step.model_output = Some(agent::message::Message::assistant("你好，我是 Haloeth"));

        // 文本已经随流式增量显示，步骤里不再重复。
        assert_eq!(render_step(&Step::Action(step)), None);
    }

    #[test]
    fn fallback_answer_reports_the_maximum_step_error() {
        let mut step = action(Vec::new());
        step.error = Some(AgentError::MaxSteps);
        step.is_final_answer = true;
        step.output = Some(json!("partial answer"));

        let rendered = render_step(&Step::Action(step)).expect("fallback step should render");

        assert!(rendered.starts_with("错误 reached the maximum number of steps"));
        assert!(rendered.ends_with(&lines(&["最终答案：", "partial answer"])));
    }

    #[test]
    fn steps_without_activity_are_skipped() {
        assert_eq!(render_step(&Step::Action(action(Vec::new()))), None);
    }

    #[test]
    fn planning_step_is_displayed_by_the_activity_card() {
        let step = Step::Planning(PlanningStep {
            plan: "1. 检索".into(),
            timing: Timing::start(),
            usage: None,
        });

        assert_eq!(render_step(&step), None);
    }
}
