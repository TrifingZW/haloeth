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
use agent::openai::{HttpModel, HttpModelOptions, list_models};
use agent::persist::ContextSnapshot;
use agent::skill::{self, LoadSkillTool, Skill};
use agent::tools::{BuiltinOptions, SearchOptions, builtin_tools};
use agent::{Agent, Event, Interrupt, RunResult, RunState};
use gpui_kit::*;
use serde_json::Value;
use tokio::runtime::Runtime;

use crate::config::{Config, Provider};
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
    /// 会话累计 **Token** 用量
    usage: TokenUsage,
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
            usage: TokenUsage::default(),
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

    /// 会话当前使用的提供商与模型；优先会话级覆盖。
    pub fn selection(&self) -> Option<(&str, &str)> {
        if let (Some(provider), Some(model)) = (&self.provider, &self.model)
            && self.config.provider_named(provider).is_some()
            && !model.trim().is_empty()
        {
            return Some((provider.as_str(), model.as_str()));
        }
        self.config.selection()
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
        self.usage = file.usage;
        self.context = file.context.clone();
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
        self.status = Status::Idle;
        cx.notify();
    }

    /// 设置是否把配置写回磁盘。
    pub fn set_config_persistence(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.persist_config = enabled;
        cx.notify();
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

    /// 当前配置
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// 会话累计 **Token** 用量
    pub fn usage(&self) -> TokenUsage {
        self.usage
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
            && blocking_reason(&self.config).is_none()
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
    pub fn select_model(&mut self, provider: &str, model: &str, cx: &mut Context<Self>) {
        if self.config.select_model(provider, model).is_err() {
            return;
        }
        self.provider = Some(provider.to_owned());
        self.model = Some(model.to_owned());
        // 未配置密钥时不落盘，避免把半成品配置写入配置文件；保存失败也不影响本次使用
        if self.persist_config && self.config.provider().is_some_and(Provider::is_configured) {
            let _ = self.config.save();
        }
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
        if let Some(status) = blocking_reason(&self.config) {
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
            gate = gate.with_reviewer(Box::new(HttpModel::new(reviewer_options)));
        }
        let gate = Arc::new(gate);

        let interrupt = Interrupt::default();
        let model_builder = self.model_builder.clone();
        let options = self.builtin_options();
        let servers = self.config.mcp_servers.clone();
        let skills =
            skill::select(skill::discover(&self.config.skill_dirs()), &self.config.skills.enabled);
        let context = self.context.clone();
        self.interrupt = Some(interrupt.clone());
        self.approval = Some(handle);
        self.pending.clear();
        self.status = Status::Preparing;
        cx.notify();

        let Some(runtime) = &self.runtime else {
            self.status = Status::Failed;
            cx.notify();
            return;
        };
        let run = runtime.handle().spawn(async move {
            let model: Box<dyn Model> = match model_builder {
                Some(builder) => builder(),
                None => Box::new(HttpModel::new(model_options)),
            };
            let tools = prepare_tools(&options, &servers, &skills).await;
            let mut agent = match Agent::new(model, tools) {
                Ok(agent) => agent.with_skills(skills).with_approval_gate(gate),
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
                let effect = apply(&conversation, &event, cx);
                this.update(cx, |session, cx| session.handle_effect(effect, &event, cx)).ok();
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
    fn handle_effect(&mut self, effect: Effect, event: &Event, cx: &mut Context<Self>) {
        if let Some(usage) = effect.usage {
            self.usage = self.usage + usage;
        }
        if self.status == Status::Preparing {
            self.status = Status::Running;
        }
        if let Event::Activity(activity) = event {
            match activity {
                agent::Activity::ApprovalRequested(request) => {
                    self.pending.insert(request.id.clone(), request.clone());
                    if request.manual {
                        self.status = Status::AwaitingApproval;
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
                }
                _ => {}
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
        let provider = self.config.provider_named(reviewer.provider.trim())?;
        Some(options_of(provider, reviewer.model.trim()))
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
        BuiltinOptions {
            search,
            disabled: self.config.tools.disabled.clone(),
        }
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
        self.context = snapshot;
        self.interrupt = None;
        self.approval = None;
        self.pending.clear();
        self.status = Status::Idle;
        self.conversation.update(cx, |conversation, cx| conversation.end_reasoning(cx));
        match result {
            Ok(result) => {
                self.usage = result.usage;
                if result.state == RunState::Cancelled {
                    self.conversation.update(cx, |conversation, cx| {
                        conversation.push(ChatRole::Assistant, "（已取消）", cx);
                    });
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

/// 事件应用后需要会话层知道的结果。
#[derive(Debug, Default, Clone, Copy)]
struct Effect {
    /// 本步骤的 **Token** 用量。
    usage: Option<TokenUsage>,
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
fn blocking_reason(config: &Config) -> Option<Status> {
    if !config.provider().is_some_and(Provider::is_configured) {
        return Some(Status::Unconfigured);
    }
    if config.model().is_none() {
        return Some(Status::Unselected);
    }
    None
}

/// 由服务商与模型标识构造模型选项。
fn options_of(provider: &Provider, model: &str) -> HttpModelOptions {
    HttpModelOptions::new(model.to_owned(), provider.api_key.clone())
        .base_url(provider.endpoint())
        .tool_choice(provider.tool_choice)
}

/// 把运行事件应用到对话区，并返回步骤携带的 **Token** 用量
fn apply<C: AppContext>(conversation: &Entity<Conversation>, event: &Event, cx: &mut C) -> Effect {
    let mut effect = Effect::default();
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
            effect.usage = step.usage();
        }
        Event::Activity(activity) => {
            apply_activity(conversation, activity, cx);
        }
    }
    effect
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
            {
                // 最终答案是模型给出的 Markdown，与标题分行后按 Markdown 渲染
                lines.push(format!("最终答案：{LINE_BREAK}{}", render_value(output)));
            }
            (!lines.is_empty()).then(|| lines.join(LINE_BREAK))
        }
    }
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
    use super::{LINE_BREAK, Status, blocking_reason, options_of, render_step};

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
        provider.models = models.iter().map(|model| (*model).to_owned()).collect();
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
        assert_eq!(blocking_reason(&config), Some(Status::Unconfigured));

        config.add_provider(provider("空目录", &[], "secret")).expect("新增应当成功");
        assert_eq!(blocking_reason(&config), Some(Status::Unselected));

        config.add_provider(provider("无密钥", &["m1"], "  ")).expect("新增应当成功");
        config.select_model("无密钥", "m1").expect("选中应当成功");
        assert_eq!(blocking_reason(&config), Some(Status::Unconfigured));

        config.add_provider(provider("就绪", &["m1"], "secret")).expect("新增应当成功");
        config.select_model("就绪", "m1").expect("选中应当成功");
        assert_eq!(blocking_reason(&config), None);
    }

    #[test]
    fn model_options_follow_the_provider() {
        let options = options_of(&provider("local", &[], "secret"), "m1");

        assert_eq!(options.model_id, "m1");
        assert_eq!(options.api_key, "secret");
        assert_eq!(options.base_url, "https://example.com/v1");
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
            result: Ok(json!("12 results")),
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
