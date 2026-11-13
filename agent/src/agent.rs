//! 编排循环：驱动「动作—观测」循环直至产出最终答案。
//!
//! 循环骨架固定，行为差异由构造配置决定：最大步数、计划间隔与附加指令。
//! 计划步骤默认关闭，设置间隔后在第 1 步及之后每间隔步生成一次。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde_json::{Map, Value};

use crate::approval::{AllowAll, ApprovalGate, ApprovalRequest, DirectoryGrant};
use crate::error::{AgentError, ToolError};
use crate::memory::{
    ActionStep, FinalAnswerStep, Memory, PlanningStep, Step, TaskStep, ToolOutcome,
};
use crate::message::{ContentBlock, Message, StreamDelta, ToolCall};
use crate::metrics::{Timing, TokenUsage};
use crate::model::{GenerateRequest, Model, ToolChoice};
use crate::prompt;
use crate::tool::{
    ApprovalRequirement, FunctionTool, InputSpec, InputType, Tool, ToolSet, ToolSpec,
};

/// 最终答案工具的名称。
pub const FINAL_ANSWER: &str = "final_answer";

/// 默认最大步数。
pub const DEFAULT_MAX_STEPS: u32 = 20;

/// 运行期事件。
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// 模型输出的文本增量。
    Delta(String),
    /// 一个步骤完成并入记忆。
    Step(Step),
    /// 运行过程中的细粒度活动，供界面实时展示。
    Activity(Activity),
}

/// 运行过程中的细粒度活动。
#[derive(Debug, Clone, PartialEq)]
pub enum Activity {
    /// 开始一次模型调用。
    Thinking {
        /// 步号。
        step: u32,
    },
    /// 推理过程增量。
    ReasoningDelta(String),
    /// 开始执行一次工具调用。
    ToolStarted {
        /// 步号。
        step: u32,
        /// 被执行的调用。
        call: ToolCall,
    },
    /// 工具调用结束。
    ToolFinished {
        /// 步号。
        step: u32,
        /// 调用标识符。
        call_id: String,
        /// 是否成功。
        ok: bool,
        /// 输出或错误的短摘要。
        preview: String,
        /// 耗时毫秒数。
        elapsed_ms: u64,
    },
    /// 请求人工审批。
    ApprovalRequested(ApprovalRequest),
    /// 审批完成。
    ApprovalResolved {
        /// 审批标识符。
        id: String,
        /// 决策。
        decision: crate::approval::ApprovalDecision,
        /// 原因。
        reason: String,
    },
    /// 加载了一个 Skill。
    SkillLoaded {
        /// Skill 名称。
        name: String,
    },
    /// 开始生成计划。
    PlanStarted {
        /// 步号。
        step: u32,
    },
    /// 计划生成完成。
    PlanFinished {
        /// 步号。
        step: u32,
        /// 计划正文。
        plan: String,
    },
    /// 工具产出了可以在对话中展示的本地图片。
    Images(Vec<String>),
    /// 工具调用自身消耗的 **Token**（例如生成类模型）。
    ToolUsage {
        /// 模型标识。
        model: String,
        /// 本次消耗。
        usage: TokenUsage,
    },
}

/// 运行结束状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// 由最终答案工具正常结束。
    Success,
    /// 步数耗尽，输出为兜底答案。
    MaxSteps,
    /// 被调用方请求停止，输出为空值。
    Cancelled,
}

/// 一次运行的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct RunResult {
    /// 最终答案。
    pub output: Value,
    /// 结束状态。
    pub state: RunState,
    /// 本次运行的 **Token** 用量；记忆在运行前已清空时等于会话累计量。
    pub usage: TokenUsage,
    /// 总耗时。
    pub timing: Timing,
}

/// 中断标记：由调用方持有，用于请求停止运行。
///
/// 标记在每次运行开始时清除，因此同一个标记可跨多次运行复用。
/// 中断在步骤边界生效：正在进行的模型调用与工具调用会先执行完毕。
#[derive(Clone, Default)]
pub struct Interrupt {
    cancelled: Arc<AtomicBool>,
}

impl Interrupt {
    /// 请求停止运行。
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// 是否已请求停止。
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// 清除标记。
    fn reset(&self) {
        self.cancelled.store(false, Ordering::Relaxed);
    }
}

/// 代理：以工具调用为动作空间的编排循环。
pub struct Agent<M> {
    model: M,
    tools: ToolSet,
    memory: Memory,
    max_steps: u32,
    planning_interval: Option<u32>,
    instructions: Option<String>,
    skills: Vec<crate::skill::Skill>,
    /// 当前会话授权的目录；写进系统提示，让模型知道工作区边界。
    workspace: Vec<DirectoryGrant>,
    interrupt: Interrupt,
    gate: Arc<dyn ApprovalGate>,
}

impl<M: Model> Agent<M> {
    /// 以模型与工具集构造代理；工具集缺失最终答案工具时自动注入。
    pub fn new(model: M, mut tools: ToolSet) -> Result<Self, AgentError> {
        if tools.get(FINAL_ANSWER).is_none() {
            tools.insert(final_answer_tool())?;
        }
        let system_prompt = prompt::system_prompt(&tools, None);
        Ok(Self {
            model,
            tools,
            memory: Memory::new(system_prompt),
            max_steps: DEFAULT_MAX_STEPS,
            planning_interval: None,
            instructions: None,
            skills: Vec::new(),
            workspace: Vec::new(),
            interrupt: Interrupt::default(),
            gate: Arc::new(AllowAll),
        })
    }

    /// 设置审核门；不设置时全部放行。
    pub fn with_approval_gate(mut self, gate: Arc<dyn ApprovalGate>) -> Self {
        self.gate = gate;
        self
    }

    /// 运行结束：释放审核门持有的事件发送端。
    pub fn close_events(&mut self) {
        self.gate.close();
    }

    /// 设置最大步数。
    pub fn with_max_steps(mut self, max_steps: u32) -> Self {
        self.max_steps = max_steps;
        self
    }

    /// 设置计划间隔；零与 [`None`] 均表示不生成计划。
    pub fn with_planning_interval(mut self, interval: Option<u32>) -> Self {
        self.planning_interval = interval.filter(|interval| *interval > 0);
        self
    }

    /// 设置已启用的 Skill；系统提示会列出它们的元数据。
    pub fn with_skills(mut self, skills: Vec<crate::skill::Skill>) -> Self {
        self.skills = skills;
        self.refresh_system_prompt();
        self
    }

    /// 设置当前会话授权的目录；系统提示会说明工作区与相对路径基准。
    pub fn with_workspace(mut self, workspace: Vec<DirectoryGrant>) -> Self {
        self.workspace = workspace;
        self.refresh_system_prompt();
        self
    }

    /// 设置附加指令，追加到系统提示末尾。
    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self.refresh_system_prompt();
        self
    }

    /// 返回记忆。
    pub fn memory(&self) -> &Memory {
        &self.memory
    }

    /// 返回工具集。
    pub fn tools(&self) -> &ToolSet {
        &self.tools
    }

    /// 返回模型标识。
    pub fn model_id(&self) -> &str {
        self.model.model_id()
    }

    /// 返回模型。
    pub fn model(&self) -> &M {
        &self.model
    }

    /// 返回模型的可变引用，用于调整模型配置。
    pub fn model_mut(&mut self) -> &mut M {
        &mut self.model
    }

    /// 返回中断标记的副本，供调用方在运行期间请求停止。
    pub fn interrupt(&self) -> Interrupt {
        self.interrupt.clone()
    }

    /// 清空会话步骤，保留系统提示。
    pub fn reset(&mut self) {
        self.memory.reset();
    }

    /// 返回已完成的步骤，供会话持久化。
    pub fn steps(&self) -> &[Step] {
        &self.memory.steps
    }

    /// 取出已完成的步骤；代理随后从空上下文开始。
    pub fn take_steps(&mut self) -> Vec<Step> {
        std::mem::take(&mut self.memory.steps)
    }

    /// 以既有步骤恢复上下文；系统提示在下次运行时按当前工具重建。
    pub fn restore_steps(&mut self, steps: Vec<Step>) {
        self.memory.steps = steps;
    }

    /// 导出上下文快照，供前端持久化。
    pub fn snapshot(&self) -> crate::persist::ContextSnapshot {
        self.memory.snapshot()
    }

    /// 从快照恢复上下文；系统提示保持不变。
    pub fn restore_context(&mut self, snapshot: crate::persist::ContextSnapshot) {
        self.memory.restore(snapshot);
    }

    /// 运行一次任务。
    ///
    /// `on_event` 在运行过程中接收文本增量与完成的步骤，运行结束后返回结果。
    pub async fn run<F>(
        &mut self,
        task: impl Into<TaskStep>,
        mut on_event: F,
    ) -> Result<RunResult, AgentError>
    where
        F: FnMut(Event) + Send,
    {
        self.refresh_system_prompt();
        self.interrupt.reset();
        let mut timing = Timing::start();

        let task = task.into();
        let task_text = task.task.clone();
        self.register(Step::Task(task), &mut on_event);

        let mut answer = None;
        let mut cancelled = false;
        let mut number = 1;
        while number <= self.max_steps {
            if self.interrupt.is_cancelled() {
                cancelled = true;
                break;
            }
            if self.should_plan(number) {
                let step = self.planning_step(&task_text, number, &mut on_event).await?;
                self.register(Step::Planning(step), &mut on_event);
            }

            // 审核模型需要看到用户的要求与之前的步骤，才能判断当前调用是否合理。
            self.gate.set_context(self.review_context());
            let step = self.action_step(number, &mut on_event).await?;
            let finished = step.is_final_answer;
            let output = step.output.clone();
            self.register(Step::Action(step), &mut on_event);
            if finished {
                answer = output;
                break;
            }
            number += 1;
        }

        let state = match (answer.is_some(), cancelled) {
            (true, _) => RunState::Success,
            (false, true) => RunState::Cancelled,
            (false, false) => RunState::MaxSteps,
        };
        let output = match answer {
            Some(output) => output,
            None if cancelled => Value::Null,
            None => {
                let step = self.fallback_step(&task_text, &mut on_event).await?;
                let output = step.output.clone().unwrap_or(Value::Null);
                self.register(Step::Action(step), &mut on_event);
                output
            }
        };

        timing.finish();
        self.register(
            Step::FinalAnswer(FinalAnswerStep {
                output: output.clone(),
            }),
            &mut on_event,
        );

        Ok(RunResult {
            output,
            state,
            usage: self.memory.usage(),
            timing,
        })
    }

    /// 审核模型可见的上下文：最近若干条消息，且只保留文本，避免把图片与音频整段复制。
    fn review_context(&self) -> Vec<crate::message::Message> {
        let mut context = self.memory.history();
        let skip = context.len().saturating_sub(16);
        context.drain(..skip);
        for message in &mut context {
            message.content.retain(|block| matches!(block, crate::message::ContentBlock::Text(_)));
        }
        context
    }

    /// 判断当前步号是否应生成计划。
    fn should_plan(&self, number: u32) -> bool {
        self.planning_interval
            .is_some_and(|interval| number == 1 || (number - 1).is_multiple_of(interval))
    }

    /// 用当前工具集、指令、Skill 与工作区刷新系统提示。
    fn refresh_system_prompt(&mut self) {
        self.memory.system_prompt = prompt::system_prompt_with_context(
            &self.tools,
            self.instructions.as_deref(),
            &self.skills,
            &self.workspace,
        );
    }

    /// 将步骤写入记忆并派发事件。
    fn register<F: FnMut(Event)>(&mut self, step: Step, on_event: &mut F) {
        self.memory.push(step.clone());
        on_event(Event::Step(step));
    }

    /// 执行一次动作步骤。
    ///
    /// 无工具调用、或最终答案工具与其他调用同时出现时，记录可恢复错误并返回，
    /// 由下一轮的上下文把错误回灌给模型。
    async fn action_step<F>(
        &mut self,
        number: u32,
        on_event: &mut F,
    ) -> Result<ActionStep, AgentError>
    where
        F: FnMut(Event) + Send,
    {
        let timing = Timing::start();
        on_event(Event::Activity(Activity::Thinking {
            step: number,
        }));
        let request = GenerateRequest::new(self.memory.context())
            .tools(self.tools.schemas())
            .tool_choice(ToolChoice::Required);
        let mut sink = |delta: StreamDelta| forward(on_event, delta);
        let message =
            self.model.generate(request, &mut sink).await.map_err(AgentError::Generation)?;

        let calls = message.tool_calls.clone();
        let has_final_answer = calls.iter().any(|call| call.name == FINAL_ANSWER);
        let mut step = ActionStep {
            number,
            timing,
            model_output: Some(message),
            outcomes: Vec::new(),
            error: None,
            output: None,
            is_final_answer: false,
        };

        if calls.is_empty() {
            // 并非所有提供商都会遵守 `tool_choice`（思考型模型只接受 `auto`），
            // 此时模型常常直接用文本作答。文本回复本身就是合法的最终答案，
            // 当成解析失败会把同一轮对话重试到最大步数，日志与界面里全是错误。
            let text = step.model_output.as_ref().map(message_text).unwrap_or_default();
            if text.trim().is_empty() {
                step.error = Some(AgentError::Parsing {
                    reason: "the model replied without calling any tool".into(),
                });
            } else {
                step.output = Some(Value::String(text));
                step.is_final_answer = true;
            }
            step.timing.finish();
            return Ok(step);
        }
        if has_final_answer && calls.len() > 1 {
            step.error = Some(AgentError::Parsing {
                reason: format!("'{FINAL_ANSWER}' must be the only tool call of its step"),
            });
            step.timing.finish();
            return Ok(step);
        }

        for call in calls {
            let is_final_answer = call.name == FINAL_ANSWER;
            let result = self.execute_call(number, &call, on_event).await;
            if is_final_answer && let Ok(output) = &result {
                step.output = output.json().cloned();
                step.is_final_answer = true;
            }
            step.outcomes.push(ToolOutcome {
                call,
                result,
            });
        }

        step.timing.finish();
        Ok(step)
    }

    /// 执行一次工具调用：查找 → 校验 → 审核 → 执行，并在两侧派发活动事件。
    async fn execute_call<F>(
        &self,
        number: u32,
        call: &ToolCall,
        on_event: &mut F,
    ) -> Result<crate::tool::ToolOutput, ToolError>
    where
        F: FnMut(Event) + Send,
    {
        let Some(tool) = self.tools.get(&call.name) else {
            return Err(ToolError::UnknownTool {
                name: call.name.clone(),
            });
        };
        let spec = tool.spec();
        // 参数不合法时不产生审批请求。
        let resolved = spec.resolve(&call.arguments)?;

        on_event(Event::Activity(Activity::ToolStarted {
            step: number,
            call: call.clone(),
        }));
        let started = Instant::now();

        let decision = if spec.approval == ApprovalRequirement::Policy {
            self.gate.decide(spec, call).await
        } else {
            crate::approval::ApprovalDecision::AllowOnce
        };

        let result = if decision == crate::approval::ApprovalDecision::Deny {
            if self.interrupt.is_cancelled() {
                Err(ToolError::Cancelled {
                    name: call.name.clone(),
                })
            } else {
                Err(ToolError::Denied {
                    name: call.name.clone(),
                    reason: "the approval policy rejected this call".to_owned(),
                })
            }
        } else {
            tool.invoke(&resolved).await
        };

        let elapsed_ms = started.elapsed().as_millis() as u64;
        let preview = match &result {
            Ok(output) => preview_text(&output.observation()),
            Err(error) => preview_text(&error.to_string()),
        };
        on_event(Event::Activity(Activity::ToolFinished {
            step: number,
            call_id: call.id.clone(),
            ok: result.is_ok(),
            preview,
            elapsed_ms,
        }));
        if let Ok(output) = &result {
            if let Some(path) = displayed_image(&call.name, &call.arguments) {
                on_event(Event::Activity(Activity::Images(vec![path])));
            }
            if let Some((model, usage)) = output_usage(output) {
                on_event(Event::Activity(Activity::ToolUsage {
                    model,
                    usage,
                }));
            }
        }
        result
    }

    /// 生成一次计划步骤。
    async fn planning_step<F>(
        &mut self,
        task: &str,
        number: u32,
        on_event: &mut F,
    ) -> Result<PlanningStep, AgentError>
    where
        F: FnMut(Event) + Send,
    {
        let mut timing = Timing::start();
        on_event(Event::Activity(Activity::PlanStarted {
            step: number,
        }));
        let messages = if number == 1 {
            vec![Message::user(prompt::initial_plan(task, &self.tools))]
        } else {
            let mut messages = vec![Message::system(prompt::plan_update_preamble(task))];
            messages.extend(self.memory.summary());
            let remaining = self.max_steps.saturating_sub(number);
            messages.push(Message::user(prompt::plan_update_request(&self.tools, remaining)));
            messages
        };
        let request = GenerateRequest::new(messages)
            .tool_choice(ToolChoice::None)
            .stop_sequences(vec![prompt::PLAN_STOP_SEQUENCE.to_owned()]);

        let mut sink = |delta: StreamDelta| forward(on_event, delta);
        let message =
            self.model.generate(request, &mut sink).await.map_err(AgentError::Generation)?;
        timing.finish();

        let plan = message_text(&message);
        on_event(Event::Activity(Activity::PlanFinished {
            step: number,
            plan: plan.clone(),
        }));
        Ok(PlanningStep {
            plan,
            timing,
            usage: message.usage,
        })
    }

    /// 步数耗尽后请求一次兜底答案，结果记录为带最大步数错误标签的动作步骤。
    async fn fallback_step<F>(
        &mut self,
        task: &str,
        on_event: &mut F,
    ) -> Result<ActionStep, AgentError>
    where
        F: FnMut(Event) + Send,
    {
        let mut timing = Timing::start();
        let mut messages = vec![Message::system(prompt::final_answer_preamble())];
        messages.extend(self.memory.history());
        messages.push(Message::user(prompt::final_answer_request(task)));
        let request = GenerateRequest::new(messages).tool_choice(ToolChoice::None);

        let mut sink = |delta: StreamDelta| forward(on_event, delta);
        let message =
            self.model.generate(request, &mut sink).await.map_err(AgentError::Generation)?;
        timing.finish();

        Ok(ActionStep {
            number: self.max_steps + 1,
            timing,
            output: Some(Value::String(message_text(&message))),
            model_output: Some(message),
            outcomes: Vec::new(),
            error: Some(AgentError::MaxSteps),
            is_final_answer: true,
        })
    }
}

/// 构造最终答案工具，原样返回参数中的答案。
fn final_answer_tool() -> impl Tool + 'static {
    let spec = ToolSpec::new(
        FINAL_ANSWER,
        "Provides the final answer to the task. It must be the only tool call of its step.",
        InputType::Any,
    )
    .input("answer", InputSpec::new(InputType::Any, "The final answer to the task."))
    .approval(ApprovalRequirement::Never);
    FunctionTool::new(spec, |arguments: &Map<String, Value>| {
        Ok(arguments.get("answer").cloned().unwrap_or(Value::Null))
    })
}

/// 转发流式增量：文本与推理过程分别派发。
fn forward<F: FnMut(Event)>(on_event: &mut F, delta: StreamDelta) {
    if let Some(text) = delta.content {
        on_event(Event::Delta(text));
    }
    if let Some(reasoning) = delta.reasoning {
        on_event(Event::Activity(Activity::ReasoningDelta(reasoning)));
    }
}

/// 从工具输出里读取提供商报告的用量：`{"model": "...", "usage": {...}}`。
fn output_usage(output: &crate::tool::ToolOutput) -> Option<(String, TokenUsage)> {
    // 内容块输出把用量放在以 `__usage__` 开头的文本块里。
    let embedded = match output {
        crate::tool::ToolOutput::Blocks(blocks) => blocks.iter().find_map(|block| {
            let crate::message::ContentBlock::Text(text) = block else {
                return None;
            };
            let rest = text.strip_prefix("__usage__")?;
            serde_json::from_str::<Value>(rest).ok()
        }),
        crate::tool::ToolOutput::Json(_) => None,
    };
    let value = output.json().cloned().or(embedded)?;
    let model = value.get("model")?.as_str()?.to_owned();
    let usage = value.get("usage")?;
    let input = usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0) as u32;
    let output_tokens = usage.get("output_tokens").and_then(Value::as_u64).unwrap_or(0) as u32;
    let total = usage.get("total_tokens").and_then(Value::as_u64).unwrap_or(0) as u32;
    let usage = if input == 0 && output_tokens == 0 && total > 0 {
        TokenUsage::new(total, 0)
    } else {
        TokenUsage::new(input, output_tokens)
    };
    (usage.total() > 0).then_some((model, usage))
}

/// 返回工具调用可以展示的本地图片路径。
fn displayed_image(tool: &str, arguments: &Value) -> Option<String> {
    if !matches!(tool, "generate_image" | "show_image") {
        return None;
    }
    let argument = if tool == "show_image" {
        "path"
    } else {
        "output_path"
    };
    let path = arguments.get(argument)?.as_str()?;
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(|value| value.to_str())?
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp") {
        return None;
    }
    std::path::Path::new(path).is_file().then(|| path.to_owned())
}

/// 活动预览的最大字符数。
const MAX_PREVIEW: usize = 240;

/// 截断活动预览，保持单行。
fn preview_text(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    let compact: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::text::truncate(&compact, MAX_PREVIEW)
}

/// 提取消息中的文本内容。
fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.as_str()),
            ContentBlock::Image(_) | ContentBlock::Audio(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::error::{ModelError, ToolError};
    use crate::message::Role;
    use crate::tool::{FunctionTool, InputSpec, InputType, ToolSpec};

    /// 按脚本依次返回回复的模型，并记录收到的请求。
    struct ScriptedModel {
        replies: Mutex<Vec<Result<Message, ModelError>>>,
        requests: Mutex<Vec<GenerateRequest>>,
        /// 非空时在每次生成后请求中断，用于验证取消路径
        interrupt: Option<Interrupt>,
    }

    impl ScriptedModel {
        fn new(replies: Vec<Message>) -> Self {
            Self {
                replies: Mutex::new(replies.into_iter().map(Ok).collect()),
                requests: Mutex::new(Vec::new()),
                interrupt: None,
            }
        }

        fn failing(error: ModelError) -> Self {
            Self {
                replies: Mutex::new(vec![Err(error)]),
                requests: Mutex::new(Vec::new()),
                interrupt: None,
            }
        }

        fn requests(&self) -> Vec<GenerateRequest> {
            self.requests.lock().expect("lock should not be poisoned").clone()
        }
    }

    #[async_trait::async_trait]
    impl Model for ScriptedModel {
        fn model_id(&self) -> &str {
            "scripted"
        }

        async fn generate(
            &self,
            request: GenerateRequest,
            on_delta: &mut (dyn FnMut(StreamDelta) + Send),
        ) -> Result<Message, ModelError> {
            self.requests.lock().expect("lock should not be poisoned").push(request);
            let reply = self.replies.lock().expect("lock should not be poisoned").remove(0);
            if let Some(interrupt) = &self.interrupt {
                interrupt.cancel();
            }
            if let Ok(message) = &reply {
                let text = message_text(message);
                if !text.is_empty() {
                    on_delta(StreamDelta {
                        content: Some(text),
                        ..Default::default()
                    });
                }
            }
            reply
        }
    }

    fn call(name: &str, arguments: Value) -> crate::message::ToolCall {
        crate::message::ToolCall {
            id: format!("call_{name}"),
            name: name.to_owned(),
            arguments,
        }
    }

    #[allow(clippy::type_complexity)]
    fn echo_tool() -> FunctionTool<fn(&Map<String, Value>) -> Result<Value, ToolError>> {
        fn echo(arguments: &Map<String, Value>) -> Result<Value, ToolError> {
            Ok(arguments.get("text").cloned().unwrap_or(Value::Null))
        }
        FunctionTool::new(
            ToolSpec::new("echo", "Echoes the given text.", InputType::String)
                .input("text", InputSpec::new(InputType::String, "Text to echo.")),
            echo,
        )
    }

    fn tools() -> ToolSet {
        let mut tools = ToolSet::new();
        tools.insert(echo_tool()).expect("insertion should succeed");
        tools
    }

    fn assistant_calling(name: &str, arguments: Value) -> Message {
        Message {
            role: Role::Assistant,
            content: Vec::new(),
            tool_calls: vec![call(name, arguments)],
            tool_call_id: None,
            usage: None,
        }
    }

    fn final_answer(value: Value) -> Message {
        assistant_calling(FINAL_ANSWER, json!({ "answer": value }))
    }

    #[tokio::test]
    async fn run_executes_tools_and_returns_the_final_answer() {
        let model = ScriptedModel::new(vec![
            assistant_calling("echo", json!({ "text": "hello" })),
            final_answer(json!("hello")),
        ]);
        let mut agent = Agent::new(model, tools()).expect("agent should be constructed");
        let mut events = Vec::new();

        let result =
            agent.run("Echo hello.", |event| events.push(event)).await.expect("run should succeed");

        assert_eq!(result.output, json!("hello"));
        assert_eq!(result.state, RunState::Success);
        assert_eq!(agent.memory().steps.len(), 4);
        let steps: Vec<&Step> = events
            .iter()
            .filter_map(|event| match event {
                Event::Step(step) => Some(step),
                Event::Delta(_) | Event::Activity(_) => None,
            })
            .collect();
        assert_eq!(steps.len(), 4);
    }

    #[tokio::test]
    async fn a_text_reply_ends_the_run_as_the_final_answer() {
        // 思考型模型在 `tool_choice: auto` 下常常直接用文本作答；
        // 这必须是最终答案，而不是反复重试的解析错误。
        let model = ScriptedModel::new(vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text("这里是最终回答".into())],
            tool_calls: Vec::new(),
            tool_call_id: None,
            usage: None,
        }]);
        let mut agent = Agent::new(model, tools()).expect("agent should be constructed");

        let result = agent.run("Answer.", |_| {}).await.expect("run should succeed");

        assert_eq!(result.output, json!("这里是最终回答"));
        assert_eq!(result.state, RunState::Success);
        assert_eq!(agent.model().requests().len(), 1);
        let steps = agent.memory().steps.clone();
        let action = steps.iter().find_map(|step| match step {
            Step::Action(action) => Some(action),
            _ => None,
        });
        let action = action.expect("the reply should be recorded as an action step");
        assert!(action.error.is_none());
        assert!(action.is_final_answer);
    }

    #[tokio::test]
    async fn requests_carry_the_system_prompt_and_tool_schemas() {
        let model = ScriptedModel::new(vec![final_answer(json!(1))]);
        let mut agent = Agent::new(model, tools()).expect("agent should be constructed");

        agent.run("Answer.", |_| {}).await.expect("run should succeed");

        let requests = agent.model().requests();
        let first = &requests[0];
        assert_eq!(first.messages[0].role, Role::System);
        let names: Vec<&str> = first.tools.iter().map(|tool| tool.function.name.as_str()).collect();
        assert_eq!(names, ["echo", FINAL_ANSWER]);
        assert_eq!(first.tool_choice, ToolChoice::Required);
    }

    #[test]
    fn displayed_images_require_existing_image_files() {
        let dir = std::env::temp_dir().join(format!("haloeth-image-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("a.png");
        std::fs::write(&path, b"png").expect("file");

        let arguments = json!({ "output_path": path.display().to_string() });
        assert!(displayed_image("generate_image", &arguments).is_some());
        let arguments = json!({ "path": path.display().to_string() });
        assert!(displayed_image("show_image", &arguments).is_some());
        assert!(displayed_image("shell", &arguments).is_none());
        let arguments = json!({ "path": dir.join("a.txt").display().to_string() });
        assert!(displayed_image("show_image", &arguments).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unknown_tool_is_recovered_from() {
        let model = ScriptedModel::new(vec![
            assistant_calling("missing", json!({})),
            final_answer(json!("done")),
        ]);
        let mut agent = Agent::new(model, tools()).expect("agent should be constructed");

        let result = agent.run("Use a missing tool.", |_| {}).await.expect("run should succeed");

        assert_eq!(result.state, RunState::Success);
        let Step::Action(first) = &agent.memory().steps[1] else {
            panic!("expected an action step");
        };
        assert!(matches!(
            &first.outcomes[0].result,
            Err(crate::error::ToolError::UnknownTool { .. })
        ));

        let second_request = &agent.model().requests()[1];
        let observation = crate::message::normalize(&second_request.messages)
            .into_iter()
            .find(|message| message.role == crate::message::wire::WireRole::Tool)
            .expect("the failure should be sent back as a tool message");
        assert!(observation.content.is_some());
    }

    #[tokio::test]
    async fn empty_reply_without_tool_calls_is_a_parsing_error() {
        let model = ScriptedModel::new(vec![
            Message {
                role: Role::Assistant,
                content: Vec::new(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                usage: None,
            },
            final_answer(json!("ok")),
        ]);
        let mut agent = Agent::new(model, tools()).expect("agent should be constructed");

        let result = agent.run("Answer.", |_| {}).await.expect("run should succeed");

        assert_eq!(result.state, RunState::Success);
        let Step::Action(first) = &agent.memory().steps[1] else {
            panic!("expected an action step");
        };
        assert!(matches!(first.error, Some(AgentError::Parsing { .. })));
    }

    #[tokio::test]
    async fn final_answer_alongside_other_calls_is_rejected() {
        let model = ScriptedModel::new(vec![
            Message {
                role: Role::Assistant,
                content: Vec::new(),
                tool_calls: vec![
                    call("echo", json!({ "text": "x" })),
                    call(FINAL_ANSWER, json!({ "answer": 1 })),
                ],
                tool_call_id: None,
                usage: None,
            },
            final_answer(json!(1)),
        ]);
        let mut agent = Agent::new(model, tools()).expect("agent should be constructed");

        let result = agent.run("Answer.", |_| {}).await.expect("run should succeed");

        assert_eq!(result.state, RunState::Success);
        let Step::Action(first) = &agent.memory().steps[1] else {
            panic!("expected an action step");
        };
        assert!(matches!(first.error, Some(AgentError::Parsing { .. })));
        assert!(first.outcomes.is_empty());
    }

    #[tokio::test]
    async fn max_steps_produces_a_fallback_answer() {
        let model = ScriptedModel::new(vec![
            assistant_calling("echo", json!({ "text": "once" })),
            Message::assistant("Final answer from memory."),
        ]);
        let mut agent =
            Agent::new(model, tools()).expect("agent should be constructed").with_max_steps(1);
        let mut events = Vec::new();

        let result =
            agent.run("Answer.", |event| events.push(event)).await.expect("run should succeed");

        assert_eq!(result.state, RunState::MaxSteps);
        assert_eq!(result.output, json!("Final answer from memory."));
        assert!(events.contains(&Event::Delta("Final answer from memory.".into())));
        let Step::Action(last) = &agent.memory().steps[2] else {
            panic!("expected an action step");
        };
        assert_eq!(last.error, Some(AgentError::MaxSteps));
        assert!(last.is_final_answer);
    }

    #[tokio::test]
    async fn cancellation_stops_the_run_without_a_fallback_call() {
        let model = ScriptedModel::new(vec![assistant_calling("echo", json!({ "text": "one" }))]);
        let mut agent = Agent::new(model, tools()).expect("agent should be constructed");
        let interrupt = agent.interrupt();
        agent.model_mut().interrupt = Some(interrupt);

        let result = agent.run("Answer.", |_| {}).await.expect("run should succeed");

        assert_eq!(result.state, RunState::Cancelled);
        assert_eq!(result.output, Value::Null);
        // 取消不触发兜底调用：仅第一次生成发生
        assert_eq!(agent.model().requests().len(), 1);
    }

    #[tokio::test]
    async fn cancellation_flag_resets_for_the_next_run() {
        let model = ScriptedModel::new(vec![
            assistant_calling("echo", json!({ "text": "one" })),
            final_answer(json!("done")),
        ]);
        let mut agent = Agent::new(model, tools()).expect("agent should be constructed");
        let interrupt = agent.interrupt();
        agent.model_mut().interrupt = Some(interrupt.clone());

        let cancelled = agent.run("First.", |_| {}).await.expect("run should succeed");
        assert_eq!(cancelled.state, RunState::Cancelled);

        // 移除模型的取消行为并预置一个陈旧标记：运行开始时应被清除
        agent.model_mut().interrupt = None;
        interrupt.cancel();
        let result = agent.run("Second.", |_| {}).await.expect("run should succeed");

        assert_eq!(result.state, RunState::Success);
        assert_eq!(result.output, json!("done"));
    }

    #[tokio::test]
    async fn generation_failure_aborts_the_run() {
        let model = ScriptedModel::failing(ModelError::Transport {
            reason: "offline".into(),
        });
        let mut agent = Agent::new(model, tools()).expect("agent should be constructed");

        let error = agent.run("Answer.", |_| {}).await.expect_err("run should fail");

        assert!(matches!(error, AgentError::Generation(_)));
        assert!(error.is_fatal());
    }

    #[tokio::test]
    async fn planning_runs_at_the_configured_interval() {
        let model = ScriptedModel::new(vec![
            Message::assistant("Initial plan <end_plan>"),
            assistant_calling("echo", json!({ "text": "one" })),
            assistant_calling("echo", json!({ "text": "two" })),
            Message::assistant("Updated plan <end_plan>"),
            assistant_calling("echo", json!({ "text": "three" })),
            final_answer(json!("done")),
        ]);
        let mut agent = Agent::new(model, tools())
            .expect("agent should be constructed")
            .with_planning_interval(Some(2))
            .with_max_steps(5);

        let result = agent.run("Plan twice.", |_| {}).await.expect("run should succeed");

        assert_eq!(result.state, RunState::Success);
        let planning: Vec<&PlanningStep> = agent
            .memory()
            .steps
            .iter()
            .filter_map(|step| match step {
                Step::Planning(planning) => Some(planning),
                _ => None,
            })
            .collect();
        assert_eq!(planning.len(), 2);
        assert_eq!(planning[0].plan, "Initial plan <end_plan>");
        assert_eq!(planning[1].plan, "Updated plan <end_plan>");

        let requests = agent.model().requests();
        let update = &requests[3];
        assert_eq!(update.stop_sequences, vec![prompt::PLAN_STOP_SEQUENCE.to_owned()]);
        assert_eq!(update.messages[0].role, Role::System);
        assert!(update.tools.is_empty());
    }

    #[tokio::test]
    async fn planning_can_be_disabled() {
        let model = ScriptedModel::new(vec![final_answer(json!(1))]);
        let mut agent = Agent::new(model, tools())
            .expect("agent should be constructed")
            .with_planning_interval(Some(0));

        agent.run("Answer.", |_| {}).await.expect("run should succeed");

        assert!(agent.memory().steps.iter().all(|step| !matches!(step, Step::Planning(_))));
    }

    #[tokio::test]
    async fn final_answer_tool_is_injected_only_when_absent() {
        let mut tools = tools();
        tools
            .insert(FunctionTool::new(
                ToolSpec::new(FINAL_ANSWER, "Custom final answer tool.", InputType::Any)
                    .input("answer", InputSpec::new(InputType::Any, "The answer.")),
                |arguments: &Map<String, Value>| {
                    Ok(arguments.get("answer").cloned().unwrap_or(Value::Null))
                },
            ))
            .expect("insertion should succeed");
        let model = ScriptedModel::new(vec![final_answer(json!("custom"))]);
        let agent = Agent::new(model, tools).expect("agent should be constructed");

        let spec = agent.tools().get(FINAL_ANSWER).expect("tool should exist").spec();
        assert_eq!(spec.description, "Custom final answer tool.");
    }
}
