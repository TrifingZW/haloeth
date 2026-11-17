//! 会话上下文的持久化表示。
//!
//! 代理本身不读写文件：它只把记忆中的步骤导出为 [`ContextSnapshot`]，由前端决定存到哪里。
//! 系统提示不入快照，每次构建代理时按当前工具与 Skill 重新生成。

use serde::{Deserialize, Serialize};

use crate::memory::{Memory, Step};

/// 当前快照格式版本。
pub const CONTEXT_VERSION: u32 = 1;

/// 一次运行的上下文快照：已完成的步骤序列。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextSnapshot {
    /// 格式版本。
    pub version: u32,
    /// 步骤序列。
    pub steps: Vec<Step>,
}

impl Default for ContextSnapshot {
    fn default() -> Self {
        Self {
            version: CONTEXT_VERSION,
            steps: Vec::new(),
        }
    }
}

impl ContextSnapshot {
    /// 以步骤构造快照。
    pub fn new(steps: Vec<Step>) -> Self {
        Self {
            version: CONTEXT_VERSION,
            steps,
        }
    }

    /// 快照是否为空。
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

impl Memory {
    /// 导出上下文快照。
    pub fn snapshot(&self) -> ContextSnapshot {
        ContextSnapshot::new(self.steps.clone())
    }

    /// 从快照恢复步骤；系统提示保持不变。
    pub fn restore(&mut self, snapshot: ContextSnapshot) {
        self.steps = snapshot.steps;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{ActionStep, TaskStep, ToolOutcome};
    use crate::message::ToolCall;
    use crate::metrics::Timing;
    use serde_json::json;

    fn sample() -> Memory {
        let mut memory = Memory::new("system");
        memory.push(Step::Task(TaskStep::from("任务")));
        memory.push(Step::Action(ActionStep {
            number: 1,
            timing: Timing::from_duration(std::time::Duration::from_millis(12)),
            model_output: None,
            outcomes: vec![ToolOutcome {
                call: ToolCall {
                    id: "call_1".into(),
                    name: "echo".into(),
                    arguments: json!({ "text": "hi" }),
                },
                result: Ok(json!("hi").into()),
            }],
            error: None,
            output: None,
            is_final_answer: false,
        }));
        memory
    }

    #[test]
    fn snapshots_round_trip_through_json() {
        let memory = sample();
        let snapshot = memory.snapshot();

        let json = serde_json::to_string(&snapshot).expect("snapshot should serialize");
        let restored: ContextSnapshot =
            serde_json::from_str(&json).expect("snapshot should deserialize");

        assert_eq!(restored.version, CONTEXT_VERSION);
        assert_eq!(restored.steps.len(), 2);
        // 计时以毫秒保存，比较序列化结果而不是 `Instant` 数值。
        let round_trip = serde_json::to_string(&restored).expect("snapshot should serialize");
        assert_eq!(round_trip, json);
        let replayed = serde_json::to_value(&restored).expect("snapshot should convert");
        assert_eq!(replayed["steps"][1]["Action"]["outcomes"][0]["call"]["name"], "echo");
    }

    #[test]
    fn restoring_replaces_steps_and_keeps_the_system_prompt() {
        let mut memory = Memory::new("system");
        memory.push(Step::Task("old".into()));

        memory.restore(ContextSnapshot::new(vec![Step::Task("new".into())]));

        assert_eq!(memory.steps.len(), 1);
        assert_eq!(memory.system_prompt, "system");
        assert_eq!(memory.steps[0], Step::Task(TaskStep::from("new")));
    }

    #[test]
    fn timings_serialize_as_millis() {
        let timing = Timing::from_duration(std::time::Duration::from_millis(42));

        assert_eq!(serde_json::to_string(&timing).expect("timing should serialize"), "42");
    }
}
