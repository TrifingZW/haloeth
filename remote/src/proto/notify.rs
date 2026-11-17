//! 服务端推送通知与通知信封。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::dto::{
    AppStateDto, ApprovalDecisionDto, ChatRoleDto, ImageItemDto, RiskLevelDto, SessionStatusDto,
    SessionSummaryDto, StreamingDto, TranscriptEntryDto, UsagePairDto,
};

/// 连接后推送的会话快照。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptReset {
    /// 会话标识符。
    pub session_id: String,
    /// 已完成的显示记录；不含打开中的流式消息。
    pub entries: Vec<TranscriptEntryDto>,
    /// 打开中的流式消息。
    pub streaming: Option<StreamingDto>,
}

/// 追加一条完整文本行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageAppended {
    /// 角色。
    pub role: ChatRoleDto,
    /// 原文。
    pub text: String,
}

/// 助手流式增量。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageDelta {
    /// 增量文本。
    pub text: String,
}

/// 一个记忆步骤完成。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageCompleted {
    /// 步骤的展示文本；为空表示只收束流式消息。
    pub text: Option<String>,
}

/// 思考过程增量。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningDelta {
    /// 增量文本。
    pub text: String,
}

/// 工具调用开始。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolStarted {
    /// 调用标识。
    pub call_id: String,
    /// 工具名称。
    pub name: String,
    /// 参数摘要。
    pub arguments: String,
}

/// 工具调用结束。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolFinished {
    /// 调用标识。
    pub call_id: String,
    /// 是否成功。
    pub ok: bool,
    /// 结果预览。
    pub preview: String,
    /// 耗时（毫秒）。
    pub elapsed_ms: u64,
}

/// 审批请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequested {
    /// 请求标识。
    pub id: String,
    /// 工具名称。
    pub tool: String,
    /// 摘要。
    pub summary: String,
    /// 风险等级。
    pub risk: RiskLevelDto,
    /// 是否需要人工确认；否则由审核模型判定。
    pub manual: bool,
    /// 影响种类，例如 `read` / `write` / `command` / `network` / `unknown`。
    pub effects: Vec<String>,
}

/// 审批出结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalResolved {
    /// 请求标识。
    pub id: String,
    /// 决定。
    pub decision: ApprovalDecisionDto,
    /// 说明。
    pub reason: String,
}

/// 图片产出。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImagesAppended {
    /// 条目。
    pub items: Vec<ImageItemDto>,
}

/// 计划文本。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanAppended {
    /// 计划原文。
    pub text: String,
}

/// 会话状态或活动文案变化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusChanged {
    /// 运行状态。
    pub status: SessionStatusDto,
    /// 细粒度活动文案。
    pub activity: Option<String>,
}

/// 用量累计或校准。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageChanged {
    /// 总量。
    pub total: UsagePairDto,
    /// 按模型拆分。
    pub by_model: BTreeMap<String, UsagePairDto>,
}

/// 会话列表变化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsChanged {
    /// 摘要列表。
    pub summaries: Vec<SessionSummaryDto>,
}

/// 全局状态变化。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateChanged {
    /// 状态快照。
    pub state: AppStateDto,
}

/// 服务端主动停止。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shutdown {
    /// 原因。
    pub reason: String,
}

/// 服务端推送。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", content = "params", rename_all = "snake_case")]
pub enum Notification {
    /// 会话快照。
    #[serde(rename = "transcript.reset")]
    TranscriptReset(TranscriptReset),
    /// 追加完整文本行。
    #[serde(rename = "message.appended")]
    MessageAppended(MessageAppended),
    /// 助手流式增量。
    #[serde(rename = "message.delta")]
    MessageDelta(MessageDelta),
    /// 步骤完成。
    #[serde(rename = "message.completed")]
    MessageCompleted(MessageCompleted),
    /// 思考过程增量。
    #[serde(rename = "reasoning.delta")]
    ReasoningDelta(ReasoningDelta),
    /// 工具调用开始。
    #[serde(rename = "tool.started")]
    ToolStarted(ToolStarted),
    /// 工具调用结束。
    #[serde(rename = "tool.finished")]
    ToolFinished(ToolFinished),
    /// 审批请求。
    #[serde(rename = "approval.requested")]
    ApprovalRequested(ApprovalRequested),
    /// 审批结果。
    #[serde(rename = "approval.resolved")]
    ApprovalResolved(ApprovalResolved),
    /// 图片产出。
    #[serde(rename = "images.appended")]
    ImagesAppended(ImagesAppended),
    /// 计划文本。
    #[serde(rename = "plan.appended")]
    PlanAppended(PlanAppended),
    /// 状态变化。
    #[serde(rename = "status.changed")]
    StatusChanged(StatusChanged),
    /// 用量变化。
    #[serde(rename = "usage.changed")]
    UsageChanged(UsageChanged),
    /// 会话列表变化。
    #[serde(rename = "sessions.changed")]
    SessionsChanged(SessionsChanged),
    /// 全局状态变化。
    #[serde(rename = "state.changed")]
    StateChanged(StateChanged),
    /// 服务端停止。
    #[serde(rename = "shutdown")]
    Shutdown(Shutdown),
}

impl Notification {
    /// 拆出信封字段：操作名与参数。
    pub fn envelope(&self) -> (String, Value) {
        let value = serde_json::to_value(self).unwrap_or(Value::Null);
        let op = value.get("op").and_then(Value::as_str).unwrap_or_default().to_owned();
        let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
        (op, params)
    }

    /// 从信封字段还原。
    pub fn from_envelope(op: &str, params: Value) -> Option<Self> {
        serde_json::from_value(json!({ "op": op, "params": params })).ok()
    }
}

/// 通知信封：服务端 → 客户端。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationFrame {
    /// 协议版本。
    pub v: u8,
    /// 通知序号；连接内单调递增。
    pub seq: u64,
    /// 操作名。
    pub op: String,
    /// 参数。
    #[serde(default)]
    pub params: Value,
}

impl NotificationFrame {
    /// 构造一帧通知。
    pub fn new(seq: u64, notification: &Notification) -> Self {
        let (op, params) = notification.envelope();
        Self {
            v: crate::proto::PROTOCOL_VERSION,
            seq,
            op,
            params,
        }
    }

    /// 还原为序号与通知。
    pub fn parse(&self) -> Option<(u64, Notification)> {
        Notification::from_envelope(&self.op, self.params.clone())
            .map(|notification| (self.seq, notification))
    }
}
