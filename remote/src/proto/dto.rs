//! 线上数据类型：与前端内部类型解耦的稳定契约。
//!
//! 内部类型可以随界面演进重构，这里的字段名与枚举值受协议兼容性规则约束。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 对话角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRoleDto {
    /// 用户。
    User,
    /// 助手。
    Assistant,
}

/// 活动卡片状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityStatusDto {
    /// 运行中。
    Running,
    /// 已完成。
    Ok,
    /// 失败。
    Failed,
    /// 等待人工确认。
    Pending,
    /// 已允许。
    Allowed,
    /// 已拒绝。
    Denied,
    /// 运行中断。
    Interrupted,
}

/// 会话运行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatusDto {
    /// 空闲。
    Idle,
    /// 准备中。
    Preparing,
    /// 运行中。
    Running,
    /// 等待确认。
    AwaitingApproval,
    /// 正在取消。
    Cancelling,
    /// 未配置密钥。
    Unconfigured,
    /// 未选择模型。
    Unselected,
    /// 运行失败。
    Failed,
}

/// 审批决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecisionDto {
    /// 本次允许。
    AllowOnce,
    /// 始终允许。
    AllowAlways,
    /// 拒绝。
    Deny,
}

/// 风险等级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevelDto {
    /// 低。
    Low,
    /// 中。
    Medium,
    /// 高。
    High,
}

/// 目录访问级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessDto {
    /// 只读。
    Read,
    /// 读写。
    Write,
}

/// 审核模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalModeDto {
    /// 手动审核。
    Manual,
    /// 规则自动。
    Rules,
    /// 模型自动。
    Model,
    /// 无审核。
    Off,
}

/// 功能类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureKindDto {
    /// 内置工具。
    Tool,
    /// MCP 服务器。
    Mcp,
    /// Skill。
    Skill,
}

/// 活动卡片记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityRecordDto {
    /// 卡片标识。
    pub id: String,
    /// 标题。
    pub title: String,
    /// 详细内容。
    pub detail: String,
    /// 结果预览。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    /// 耗时（毫秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    /// 状态。
    pub status: ActivityStatusDto,
    /// 是否展开。
    pub expanded: bool,
}

/// 一条显示记录。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptEntryDto {
    /// 文本消息。
    Text {
        /// 角色。
        role: ChatRoleDto,
        /// Markdown 原文。
        text: String,
    },
    /// 活动卡片。
    Activity(ActivityRecordDto),
    /// 图片行。
    Image {
        /// 本地路径。
        path: String,
        /// 说明文字。
        caption: String,
    },
}

/// 打开中的流式消息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamingDto {
    /// 角色。
    pub role: ChatRoleDto,
    /// 已累积原文。
    pub text: String,
}

/// 单组 **Token** 用量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UsagePairDto {
    /// 输入。
    pub input: u32,
    /// 输出。
    pub output: u32,
}

/// **Token** 用量；总量与按模型拆分。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UsageDto {
    /// 总量。
    pub total: UsagePairDto,
    /// 按模型拆分。
    #[serde(default)]
    pub by_model: BTreeMap<String, UsagePairDto>,
}

/// 会话摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummaryDto {
    /// 会话标识符。
    pub id: String,
    /// 标题。
    pub title: String,
    /// 最后更新时间（Unix 毫秒）。
    pub updated_at: u64,
    /// 文本消息数量。
    pub message_count: usize,
}

/// 模型引用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRefDto {
    /// 提供商名称。
    pub provider: String,
    /// 模型标识。
    pub model: String,
}

/// 功能开关状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureDto {
    /// 会话级键，形如 `tool:name`。
    pub key: String,
    /// 类别。
    pub kind: FeatureKindDto,
    /// 名称。
    pub name: String,
    /// 是否启用。
    pub enabled: bool,
}

/// 目录授权。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantDto {
    /// 目录路径。
    pub path: String,
    /// 访问级别。
    pub access: AccessDto,
}

/// 应用状态快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppStateDto {
    /// 当前会话标识符。
    pub active_session_id: String,
    /// 当前会话标题。
    pub title: String,
    /// 运行状态。
    pub status: SessionStatusDto,
    /// 细粒度活动文案。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    /// 当前模型。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<ModelRefDto>,
    /// 可选模型（文本→文本）。
    pub models: Vec<ModelRefDto>,
    /// 功能开关。
    pub features: Vec<FeatureDto>,
    /// 目录授权。
    pub grants: Vec<GrantDto>,
    /// 审核模式。
    pub approval_mode: ApprovalModeDto,
    /// 用量。
    pub usage: UsageDto,
}

/// 服务端信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerInfoDto {
    /// 服务名。
    pub name: String,
    /// 版本。
    pub version: String,
    /// 平台。
    pub platform: String,
}

/// 握手结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WelcomeDto {
    /// 协议版本。
    pub protocol: u8,
    /// 服务端信息。
    pub server: ServerInfoDto,
    /// 初始状态。
    pub state: AppStateDto,
    /// 会话列表。
    pub sessions: Vec<SessionSummaryDto>,
}

/// 图片条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageItemDto {
    /// 本地路径。
    pub path: String,
    /// 说明文字。
    pub caption: String,
}

/// 连接建立时的快照；`watermark` 为通知计数器高水位，供客户端初始化游标。
///
/// 仅在宿主与服务端之间传递，不属于线上信封。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotDto {
    /// 会话标识符。
    pub session_id: String,
    /// 已完成的显示记录；不含打开中的流式消息。
    pub entries: Vec<TranscriptEntryDto>,
    /// 打开中的流式消息。
    pub streaming: Option<StreamingDto>,
    /// 通知计数器高水位。
    pub watermark: u64,
}
