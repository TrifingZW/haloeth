//! 会话记忆：步骤类型族与上下文构建。
//!
//! 该模块同时承担会话状态容器的职责：Haloeth 为单会话设计，记忆即会话状态的单点来源。
//!
//! 与 smolagents 的差异有两点：
//! 1) 系统提示是记忆字段而非独立步骤类型：会话内不变，无需一个无行为的类型。
//! 2) 动作步骤不保存模型输入消息：避免记忆随步数平方增长；审计需求由事件流承担。

use serde_json::Value;

use crate::error::AgentError;
use crate::error::ToolError;
use crate::message::{ContentBlock, ImageSource, Message, Role, ToolCall};
use crate::metrics::{Timing, TokenUsage};

/// 错误回灌时附带的重试提示。
const RETRY_HINT: &str = "Now let's retry: take care not to repeat previous errors! If you have retried several times, try a completely different approach.";

/// 计划步骤之后用于制造角色切换的提示文本。
const PLAN_CONTINUATION: &str = "Now proceed and carry out this plan.";

/// 记忆中的一个步骤。
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// 任务。
    Task(TaskStep),
    /// 一次动作。
    Action(ActionStep),
    /// 一次计划。
    Planning(PlanningStep),
    /// 最终答案。
    FinalAnswer(FinalAnswerStep),
}

impl Step {
    /// 渲染为对话消息；摘要模式丢弃系统提示与计划步骤，并丢弃助手文本。
    pub fn to_messages(&self, summary_mode: bool) -> Vec<Message> {
        match self {
            Self::Task(step) => step.to_messages(summary_mode),
            Self::Action(step) => step.to_messages(summary_mode),
            Self::Planning(step) => step.to_messages(summary_mode),
            Self::FinalAnswer(step) => step.to_messages(summary_mode),
        }
    }

    /// 返回该步骤消耗的 **Token** 用量。
    pub fn usage(&self) -> Option<TokenUsage> {
        match self {
            Self::Action(step) => step.model_output.as_ref().and_then(|output| output.usage),
            Self::Planning(step) => step.usage,
            Self::Task(_) | Self::FinalAnswer(_) => None,
        }
    }
}

/// 任务步骤。
#[derive(Debug, Clone, PartialEq)]
pub struct TaskStep {
    /// 任务文本。
    pub task: String,
    /// 任务附带的图像。
    pub images: Vec<ImageSource>,
}

impl From<&str> for TaskStep {
    fn from(task: &str) -> Self {
        Self {
            task: task.to_owned(),
            images: Vec::new(),
        }
    }
}

impl From<String> for TaskStep {
    fn from(task: String) -> Self {
        Self {
            task,
            images: Vec::new(),
        }
    }
}

impl TaskStep {
    /// 渲染为用户消息，前缀标记为任务，便于模型区分多轮输入。
    fn to_messages(&self, _summary_mode: bool) -> Vec<Message> {
        let mut content = vec![ContentBlock::text(format!("New task:\n{}", self.task))];
        content.extend(self.images.iter().cloned().map(ContentBlock::Image));
        vec![Message::new(Role::User, content)]
    }
}

/// 一次动作步骤。
#[derive(Debug, Clone, PartialEq)]
pub struct ActionStep {
    /// 步号。
    pub number: u32,
    /// 计时。
    pub timing: Timing,
    /// 模型输出消息，保留助手文本与原始工具调用。
    pub model_output: Option<Message>,
    /// 工具调用及其执行结果，按调用顺序排列。
    pub outcomes: Vec<ToolOutcome>,
    /// 步骤级错误，仅用于无关联调用的失败。
    pub error: Option<AgentError>,
    /// 步骤产出的值。
    pub output: Option<Value>,
    /// 该步骤是否产出最终答案。
    pub is_final_answer: bool,
}

impl ActionStep {
    /// 渲染为对话消息。
    ///
    /// 渲染顺序即上下文构建算法：助手文本 → 工具调用 → 逐条观测 → 错误与重试提示。
    /// 工具调用取自已执行的调用集合，保证每个调用都有对应的结果消息。
    fn to_messages(&self, summary_mode: bool) -> Vec<Message> {
        let mut messages = Vec::new();

        if let Some(output) = &self.model_output
            && !summary_mode
            && !output.content.is_empty()
        {
            messages.push(Message::new(Role::Assistant, output.content.clone()));
        }

        let calls: Vec<ToolCall> =
            self.outcomes.iter().map(|outcome| outcome.call.clone()).collect();
        if !calls.is_empty() {
            messages.push(Message::tool_calls(calls));
        }

        for outcome in &self.outcomes {
            let observation = match &outcome.result {
                Ok(value) => render_value(value),
                Err(error) => render_failure(error),
            };
            messages.push(Message::tool_response(outcome.call.id.clone(), observation));
        }

        if let Some(error) = &self.error {
            messages.push(Message::user(format!("Error: {error}\n{RETRY_HINT}")));
        }

        messages
    }
}

/// 一次工具调用的执行结果。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    /// 被执行的调用。
    pub call: ToolCall,
    /// 执行结果；失败时为工具错误。
    pub result: Result<Value, ToolError>,
}

/// 计划步骤。
#[derive(Debug, Clone, PartialEq)]
pub struct PlanningStep {
    /// 计划正文。
    pub plan: String,
    /// 计时。
    pub timing: Timing,
    /// **Token** 用量。
    pub usage: Option<TokenUsage>,
}

impl PlanningStep {
    /// 渲染为「助手计划 + 用户继续执行」两条消息；后一条消息制造角色切换，
    /// 防止模型把计划视为未完成的输出而继续续写。
    fn to_messages(&self, summary_mode: bool) -> Vec<Message> {
        if summary_mode {
            return Vec::new();
        }
        vec![Message::assistant(self.plan.clone()), Message::user(PLAN_CONTINUATION)]
    }
}

/// 最终答案步骤。
#[derive(Debug, Clone, PartialEq)]
pub struct FinalAnswerStep {
    /// 输出值。
    pub output: Value,
}

impl FinalAnswerStep {
    /// 不产生消息。
    fn to_messages(&self, _summary_mode: bool) -> Vec<Message> {
        Vec::new()
    }
}

/// 会话记忆：系统提示与全部步骤。
#[derive(Debug, Clone, PartialEq)]
pub struct Memory {
    /// 系统提示。
    pub system_prompt: String,
    /// 步骤序列。
    pub steps: Vec<Step>,
}

impl Memory {
    /// 以系统提示构造空记忆。
    pub fn new(system_prompt: impl Into<String>) -> Self {
        Self {
            system_prompt: system_prompt.into(),
            steps: Vec::new(),
        }
    }

    /// 追加步骤。
    pub fn push(&mut self, step: Step) {
        self.steps.push(step);
    }

    /// 清空步骤，保留系统提示。
    pub fn reset(&mut self) {
        self.steps.clear();
    }

    /// 渲染完整上下文，供动作步骤使用。
    pub fn context(&self) -> Vec<Message> {
        self.render(false)
    }

    /// 渲染摘要上下文，供计划更新与汇报使用：丢弃系统提示、计划步骤与助手文本。
    pub fn summary(&self) -> Vec<Message> {
        self.render(true)
    }

    /// 渲染全部步骤，不含系统提示。
    pub fn history(&self) -> Vec<Message> {
        self.steps.iter().flat_map(|step| step.to_messages(false)).collect()
    }

    /// 返回累计 **Token** 用量。
    pub fn usage(&self) -> TokenUsage {
        self.steps
            .iter()
            .filter_map(Step::usage)
            .fold(TokenUsage::default(), |total, usage| total + usage)
    }

    /// 按模式渲染全部消息。
    fn render(&self, summary_mode: bool) -> Vec<Message> {
        let mut messages = Vec::new();
        if !summary_mode {
            messages.push(Message::system(self.system_prompt.clone()));
        }
        messages.extend(self.steps.iter().flat_map(|step| step.to_messages(summary_mode)));
        messages
    }
}

/// 渲染工具输出为观测文本；字符串直接使用其内容，其余值使用 **JSON** 文本。
fn render_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// 渲染失败的工具调用为观测文本。
fn render_failure(error: &ToolError) -> String {
    format!("Error: {error}\n{RETRY_HINT}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::normalize;
    use crate::message::wire::WireRole;
    use serde_json::json;

    fn search_call() -> ToolCall {
        ToolCall {
            id: "call_1".into(),
            name: "search".into(),
            arguments: json!({ "query": "rust" }),
        }
    }

    fn trivial_action(number: u32) -> ActionStep {
        ActionStep {
            number,
            timing: Timing::start(),
            model_output: Some(Message::assistant("thinking")),
            outcomes: Vec::new(),
            error: None,
            output: None,
            is_final_answer: false,
        }
    }

    #[test]
    fn task_step_renders_user_message_with_images() {
        let step = Step::Task(TaskStep {
            task: "Find the answer.".into(),
            images: vec![ImageSource::Url("https://example.com/a.png".into())],
        });

        let messages = step.to_messages(false);

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(messages[0].content[0], ContentBlock::text("New task:\nFind the answer."));
        assert_eq!(messages[0].content[1], ContentBlock::image_url("https://example.com/a.png"));
    }

    #[test]
    fn task_step_accepts_string_into() {
        let step: TaskStep = "Answer this.".into();

        assert_eq!(step.task, "Answer this.");
        assert!(step.images.is_empty());
    }

    #[test]
    fn action_step_renders_output_calls_and_observations() {
        let call = search_call();
        let step = Step::Action(ActionStep {
            number: 1,
            timing: Timing::start(),
            model_output: Some(Message::assistant("I will search.")),
            outcomes: vec![ToolOutcome {
                call: call.clone(),
                result: Ok(json!("42 results")),
            }],
            error: None,
            output: None,
            is_final_answer: false,
        });

        let messages = step.to_messages(false);

        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0].role, Role::Assistant);
        assert_eq!(messages[0].content, vec![ContentBlock::text("I will search.")]);
        assert_eq!(messages[1].role, Role::ToolCall);
        assert_eq!(messages[1].tool_calls, vec![call]);
        assert_eq!(messages[2].role, Role::ToolResponse);
        assert_eq!(messages[2].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(messages[2].content, vec![ContentBlock::text("42 results")]);
    }

    #[test]
    fn action_step_renders_object_output_as_json_text() {
        let call = search_call();
        let step = Step::Action(ActionStep {
            number: 1,
            timing: Timing::start(),
            model_output: None,
            outcomes: vec![ToolOutcome {
                call,
                result: Ok(json!({ "hits": 2 })),
            }],
            error: None,
            output: None,
            is_final_answer: false,
        });

        let messages = step.to_messages(false);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::ToolCall);
        assert_eq!(messages[1].content, vec![ContentBlock::text(r#"{"hits":2}"#)]);
    }

    #[test]
    fn action_step_renders_tool_failures_with_retry_hint() {
        let call = search_call();
        let step = Step::Action(ActionStep {
            number: 1,
            timing: Timing::start(),
            model_output: None,
            outcomes: vec![ToolOutcome {
                call,
                result: Err(ToolError::UnknownTool {
                    name: "search".into(),
                }),
            }],
            error: None,
            output: None,
            is_final_answer: false,
        });

        let messages = step.to_messages(false);
        let observation = match &messages[1].content[0] {
            ContentBlock::Text(text) => text,
            other => panic!("expected text content, got {other:?}"),
        };

        assert!(observation.starts_with("Error: unknown tool: 'search'"));
        assert!(observation.contains("Now let's retry"));
    }

    #[test]
    fn action_step_renders_step_error_as_user_message() {
        let step = Step::Action(ActionStep {
            number: 1,
            timing: Timing::start(),
            model_output: Some(Message::assistant("no call")),
            outcomes: Vec::new(),
            error: Some(AgentError::Parsing {
                reason: "no tool call was provided".into(),
            }),
            output: None,
            is_final_answer: false,
        });

        let messages = step.to_messages(false);

        assert_eq!(messages[0].role, Role::Assistant);
        assert_eq!(messages[1].role, Role::User);
        let text = match &messages[1].content[0] {
            ContentBlock::Text(text) => text,
            other => panic!("expected text content, got {other:?}"),
        };
        assert!(
            text.starts_with("Error: failed to parse the model output: no tool call was provided")
        );
    }

    #[test]
    fn planning_step_renders_plan_and_continuation() {
        let step = Step::Planning(PlanningStep {
            plan: "1. Search".into(),
            timing: Timing::start(),
            usage: None,
        });

        let messages = step.to_messages(false);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, Role::Assistant);
        assert_eq!(messages[0].content, vec![ContentBlock::text("1. Search")]);
        assert_eq!(messages[1].role, Role::User);
        assert_eq!(messages[1].content, vec![ContentBlock::text(PLAN_CONTINUATION)]);
    }

    #[test]
    fn final_answer_step_renders_nothing() {
        let step = Step::FinalAnswer(FinalAnswerStep {
            output: json!(42),
        });

        assert!(step.to_messages(false).is_empty());
    }

    #[test]
    fn summary_mode_drops_system_prompt_plans_and_assistant_text() {
        let call = search_call();
        let mut memory = Memory::new("system prompt");
        memory.push(Step::Planning(PlanningStep {
            plan: "1. Search".into(),
            timing: Timing::start(),
            usage: None,
        }));
        memory.push(Step::Action(ActionStep {
            number: 1,
            timing: Timing::start(),
            model_output: Some(Message::assistant("thinking")),
            outcomes: vec![ToolOutcome {
                call,
                result: Ok(json!("42 results")),
            }],
            error: None,
            output: None,
            is_final_answer: false,
        }));

        let context = memory.context();
        let summary = memory.summary();

        assert_eq!(context.len(), 6);
        assert_eq!(context[0].role, Role::System);
        assert_eq!(summary.len(), 2);
        assert!(summary.iter().all(|message| message.role != Role::System));
        assert!(summary.iter().all(|message| message.role != Role::Assistant));
        assert!(summary.iter().all(|message| message.role != Role::User));
    }

    #[test]
    fn history_renders_steps_without_system_prompt() {
        let mut memory = Memory::new("system prompt");
        memory.push(Step::Task("task".into()));

        let history = memory.history();

        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, Role::User);
    }

    #[test]
    fn memory_accumulates_usage_across_steps() {
        let mut memory = Memory::new("system prompt");
        memory.push(Step::Action(ActionStep {
            model_output: Some(Message::assistant("a").with_usage(TokenUsage::new(10, 5))),
            ..trivial_action(1)
        }));
        memory.push(Step::Planning(PlanningStep {
            plan: "1. Search".into(),
            timing: Timing::start(),
            usage: Some(TokenUsage::new(3, 2)),
        }));

        assert_eq!(memory.usage(), TokenUsage::new(13, 7));
    }

    #[test]
    fn reset_clears_steps_and_keeps_system_prompt() {
        let mut memory = Memory::new("system prompt");
        memory.push(Step::Task("task".into()));

        memory.reset();

        assert!(memory.steps.is_empty());
        assert_eq!(memory.system_prompt, "system prompt");
    }

    #[test]
    fn rendered_context_projects_to_protocol_messages() {
        let call = search_call();
        let mut memory = Memory::new("You are an agent.");
        memory.push(Step::Task("Find the answer.".into()));
        memory.push(Step::Action(ActionStep {
            number: 1,
            timing: Timing::start(),
            model_output: Some(Message {
                role: Role::Assistant,
                content: vec![ContentBlock::text("Searching.")],
                tool_calls: vec![call.clone()],
                tool_call_id: None,
                usage: None,
            }),
            outcomes: vec![ToolOutcome {
                call,
                result: Ok(json!("ok")),
            }],
            error: None,
            output: None,
            is_final_answer: false,
        }));

        let wire = normalize(&memory.context());

        let roles: Vec<WireRole> = wire.iter().map(|message| message.role).collect();
        assert_eq!(roles, [WireRole::System, WireRole::User, WireRole::Assistant, WireRole::Tool]);
        let calls = wire[2].tool_calls.as_ref().expect("calls should be projected once");
        assert_eq!(calls.len(), 1);
        assert_eq!(wire[3].tool_call_id.as_deref(), Some("call_1"));
    }
}
