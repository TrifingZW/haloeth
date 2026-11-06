//! 会话：代理运行时、运行状态，以及事件到界面的桥接。
//!
//! 运行在专用的 **Tokio** 运行时上执行：**HTTP** 客户端需要该运行时驱动 **I/O**，
//! 而界面执行器不提供。运行期间代理从会话移入运行任务，只允许串行运行；
//! 事件经无界通道回送到界面执行器上的任务，再写入对话区。

use agent::default_tools::default_tools;
use agent::error::AgentError;
use agent::memory::Step;
use agent::metrics::TokenUsage;
use agent::openai::{HttpModel, HttpModelOptions, list_models};
use agent::{Agent, Event, Interrupt, RunResult, RunState};
use gpui_kit::*;
use serde_json::Value;
use tokio::runtime::Runtime;

use crate::config::{Config, Provider};
use crate::conversation::{ChatRole, Conversation};

/// 会话运行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// 空闲
    Idle,
    /// 正在运行
    Running,
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
            Self::Running => "运行中",
            Self::Cancelling => "正在取消",
            Self::Unconfigured => "未配置密钥",
            Self::Unselected => "未选择模型",
            Self::Failed => "运行失败",
        }
    }

    /// 是否处于运行中或正在取消
    pub fn is_busy(self) -> bool {
        matches!(self, Self::Running | Self::Cancelling)
    }
}

/// 会话：持有代理与运行状态
pub struct Session {
    config: Config,
    conversation: Entity<Conversation>,
    /// 空闲时持有代理；运行期间为 [`None`]
    agent: Option<Agent<HttpModel>>,
    /// 运行时代理所在运行时，首次运行前为 [`None`]
    runtime: Option<Runtime>,
    /// 运行期间的中断标记
    interrupt: Option<Interrupt>,
    /// 会话累计 **Token** 用量
    usage: TokenUsage,
    status: Status,
}

impl Session {
    /// 构造会话；此时不创建运行时与代理
    pub fn new(config: Config, conversation: Entity<Conversation>) -> Self {
        let mut config = config;
        config.normalise();
        Self {
            config,
            conversation,
            agent: None,
            runtime: None,
            interrupt: None,
            usage: TokenUsage::default(),
            status: Status::Idle,
        }
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

    /// 应用配置，并同步到已构建的代理
    pub fn apply_config(&mut self, config: Config, cx: &mut Context<Self>) {
        let mut config = config;
        config.normalise();
        self.config = config;
        self.sync_model();
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

    /// 切换模型：更新配置并同步到已构建的代理
    pub fn select_model(&mut self, provider: &str, model: &str, cx: &mut Context<Self>) {
        if self.config.select_model(provider, model).is_err() {
            return;
        }
        self.sync_model();
        // 未配置密钥时不落盘，避免把半成品配置写入配置文件；保存失败也不影响本次使用
        if self.config.provider().is_some_and(Provider::is_configured) {
            let _ = self.config.save();
        }
        cx.notify();
    }

    /// 请求停止当前运行；中断在步骤边界生效
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        let Some(interrupt) = &self.interrupt else {
            return;
        };
        interrupt.cancel();
        self.status = Status::Cancelling;
        cx.notify();
    }

    /// 清空代理记忆，开始新的会话上下文
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        if let Some(agent) = &mut self.agent {
            agent.reset();
        }
        self.usage = TokenUsage::default();
        self.status = Status::Idle;
        cx.notify();
    }

    /// 提交任务：空闲且具备运行前提时启动一次运行，否则只更新状态
    pub fn submit(&mut self, task: String, cx: &mut Context<Self>) {
        if self.status.is_busy() {
            return;
        }
        if let Some(status) = blocking_reason(&self.config) {
            self.status = status;
            cx.notify();
            return;
        }
        let Some(agent) = self.take_agent() else {
            self.status = Status::Failed;
            cx.notify();
            return;
        };
        self.status = Status::Running;
        cx.notify();
        self.spawn(agent, task, cx);
    }

    /// 取出代理；首次使用时构建运行时与代理，并把当前模型同步到代理
    fn take_agent(&mut self) -> Option<Agent<HttpModel>> {
        let options = self.model_options()?;
        if self.runtime.is_none() {
            self.runtime = Some(Runtime::new().ok()?);
        }
        let agent = match self.agent.take() {
            Some(mut agent) => {
                *agent.model_mut() = HttpModel::new(options);
                agent
            }
            None => Agent::new(HttpModel::new(options), default_tools()).ok()?,
        };
        Some(agent)
    }

    /// 由当前配置构造模型选项；缺少提供商或模型时返回 [`None`]
    fn model_options(&self) -> Option<HttpModelOptions> {
        let provider = self.config.provider()?;
        let model = self.config.model()?;
        Some(options_of(provider, model))
    }

    /// 把当前配置同步到已构建的代理
    fn sync_model(&mut self) {
        let Some(options) = self.model_options() else {
            return;
        };
        if let Some(agent) = &mut self.agent {
            *agent.model_mut() = HttpModel::new(options);
        }
    }

    /// 在运行时上执行一次运行，把事件与结果回送到界面
    fn spawn(&mut self, agent: Agent<HttpModel>, task: String, cx: &mut Context<Self>) {
        let Some(runtime) = &self.runtime else {
            self.status = Status::Failed;
            cx.notify();
            return;
        };

        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let interrupt = agent.interrupt();
        let run = runtime.handle().spawn(async move {
            let mut agent = agent;
            let result = agent
                .run(task, move |event| {
                    let _ = sender.send(event);
                })
                .await;
            (agent, result)
        });
        self.interrupt = Some(interrupt);

        let conversation = self.conversation.clone();
        cx.spawn(async move |this, cx| {
            while let Some(event) = receiver.recv().await {
                if let Some(usage) = apply(&conversation, &event, cx) {
                    this.update(cx, |session, cx| session.add_usage(usage, cx)).ok();
                }
            }
            match run.await {
                Ok((agent, result)) => {
                    this.update(cx, |session, cx| session.finish(agent, result, cx)).ok();
                }
                Err(_) => {
                    // 运行任务未能归还代理，此会话不再可运行（进程启用 panic=abort，实际难以触发）
                    this.update(cx, |session, cx| session.fail(cx)).ok();
                }
            }
        })
        .detach();
    }

    /// 累加步骤用量；运行结束时由运行结果覆盖
    fn add_usage(&mut self, usage: TokenUsage, cx: &mut Context<Self>) {
        self.usage = self.usage + usage;
        cx.notify();
    }

    /// 运行结束：归还代理、更新状态，失败时把原因写入对话区
    fn finish(
        &mut self,
        agent: Agent<HttpModel>,
        result: Result<RunResult, AgentError>,
        cx: &mut Context<Self>,
    ) {
        self.agent = Some(agent);
        self.interrupt = None;
        self.status = Status::Idle;
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
        self.agent = None;
        self.interrupt = None;
        self.status = Status::Failed;
        cx.notify();
    }
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
    HttpModelOptions::new(model.to_owned(), provider.api_key.clone()).base_url(provider.endpoint())
}

/// 把运行事件应用到对话区，并返回步骤携带的 **Token** 用量
fn apply<C: AppContext>(
    conversation: &Entity<Conversation>,
    event: &Event,
    cx: &mut C,
) -> Option<TokenUsage> {
    match event {
        Event::Delta(text) => {
            conversation
                .update(cx, |conversation, cx| conversation.stream(ChatRole::Assistant, text, cx));
            None
        }
        Event::Step(step) => {
            let text = render_step(step);
            conversation.update(cx, |conversation, cx| {
                conversation.end_stream();
                if let Some(text) = text {
                    conversation.push(ChatRole::Assistant, text, cx);
                }
            });
            step.usage()
        }
    }
}

/// 步骤文本的行分隔：Markdown 硬换行，逐行保留，避免被解析器折叠为一段
const LINE_BREAK: &str = "  \n";

/// 把一个记忆步骤渲染为对话文本；无需展示的步骤返回 [`None`]
fn render_step(step: &Step) -> Option<String> {
    match step {
        // 任务文本已由输入区写入对话
        Step::Task(_) => None,
        // 最终答案由产出它的动作步骤展示
        Step::FinalAnswer(_) => None,
        // 计划是模型给出的 Markdown，与标题分行后按 Markdown 渲染
        Step::Planning(step) => Some(format!("计划：{LINE_BREAK}{}", step.plan)),
        Step::Action(step) => {
            let mut lines = Vec::new();
            for outcome in &step.outcomes {
                // 前缀与值同行：值中的 Markdown 标记不会被当作块级语法
                lines.push(format!("调用 {}({})", outcome.call.name, outcome.call.arguments));
                match &outcome.result {
                    Ok(value) => lines.push(format!("观察 {}", render_value(value))),
                    Err(error) => lines.push(format!("错误 {error}")),
                }
            }
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
        other => other.to_string(),
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
        assert_eq!(Status::Running.label(), "运行中");
        assert_eq!(Status::Cancelling.label(), "正在取消");
        assert_eq!(Status::Unconfigured.label(), "未配置密钥");
        assert_eq!(Status::Unselected.label(), "未选择模型");
        assert_eq!(Status::Failed.label(), "运行失败");
    }

    #[test]
    fn busy_statuses_cover_running_and_cancelling() {
        assert!(Status::Running.is_busy());
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
    fn action_step_renders_calls_and_observations() {
        let step = Step::Action(action(vec![ToolOutcome {
            call: call("search", json!({ "query": "rust" })),
            result: Ok(json!("12 results")),
        }]));

        assert_eq!(
            render_step(&step),
            Some(lines(&["调用 search({\"query\":\"rust\"})", "观察 12 results"]))
        );
    }

    #[test]
    fn action_step_renders_failures() {
        let step = Step::Action(action(vec![ToolOutcome {
            call: call("search", json!({})),
            result: Err(ToolError::UnknownTool {
                name: "search".into(),
            }),
        }]));

        assert_eq!(
            render_step(&step),
            Some(lines(&["调用 search({})", "错误 unknown tool: 'search'"]))
        );
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
    fn planning_step_renders_the_plan() {
        let step = Step::Planning(PlanningStep {
            plan: "1. 检索".into(),
            timing: Timing::start(),
            usage: None,
        });

        assert_eq!(render_step(&step), Some(lines(&["计划：", "1. 检索"])));
    }
}
