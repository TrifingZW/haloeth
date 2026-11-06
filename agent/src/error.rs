//! 代理错误类型。
//!
//! 错误消息面向模型与用户，统一使用英文，以便直接作为观测文本回灌上下文。

use std::error::Error;
use std::fmt;

/// 工具契约相关的错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// 工具元数据不合法。
    InvalidSpec {
        /// 工具名称。
        name: String,
        /// 不合法原因。
        reason: String,
    },
    /// 注册表中已存在同名工具。
    DuplicateName {
        /// 冲突的名称。
        name: String,
    },
    /// 引用了未注册的工具。
    UnknownTool {
        /// 被引用的名称。
        name: String,
    },
    /// 调用参数不合法。
    InvalidArguments {
        /// 工具名称。
        name: String,
        /// 不合法原因。
        reason: String,
    },
    /// 工具执行失败。
    ExecutionFailed {
        /// 工具名称。
        name: String,
        /// 失败原因。
        reason: String,
    },
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSpec {
                name,
                reason,
            } => write!(f, "invalid spec for tool '{name}': {reason}"),
            Self::DuplicateName {
                name,
            } => write!(f, "duplicate tool name: '{name}'"),
            Self::UnknownTool {
                name,
            } => write!(f, "unknown tool: '{name}'"),
            Self::InvalidArguments {
                name,
                reason,
            } => write!(f, "invalid arguments for tool '{name}': {reason}"),
            Self::ExecutionFailed {
                name,
                reason,
            } => write!(f, "tool '{name}' failed: {reason}"),
        }
    }
}

impl Error for ToolError {}

/// 模型契约相关的错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    /// 请求序列化失败。
    Encode {
        /// 失败原因。
        reason: String,
    },
    /// 网络传输失败。
    Transport {
        /// 失败原因。
        reason: String,
    },
    /// 服务返回非成功状态。
    Rejected {
        /// **HTTP** 状态码。
        status: u16,
        /// 服务端返回的正文，已截断。
        body: String,
    },
    /// 响应解析失败。
    Malformed {
        /// 失败原因。
        reason: String,
    },
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode {
                reason,
            } => write!(f, "failed to encode model request: {reason}"),
            Self::Transport {
                reason,
            } => write!(f, "model request failed: {reason}"),
            Self::Rejected {
                status,
                body,
            } => write!(f, "model request was rejected with status {status}: {body}"),
            Self::Malformed {
                reason,
            } => write!(f, "failed to parse model response: {reason}"),
        }
    }
}

impl Error for ModelError {}

/// 代理循环错误。
///
/// 可恢复性即错误分类：仅模型生成失败为致命错误，其余错误均以文本形式回灌上下文并进入下一轮。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentError {
    /// 模型输出无法解析为动作。
    Parsing {
        /// 失败原因。
        reason: String,
    },
    /// 工具调用失败。
    Tool(ToolError),
    /// 模型生成失败；该错误不可恢复。
    Generation(ModelError),
    /// 达到最大步数。
    MaxSteps,
}

impl AgentError {
    /// 判断错误是否致命。
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::Generation(_))
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parsing {
                reason,
            } => write!(f, "failed to parse the model output: {reason}"),
            Self::Tool(error) => error.fmt(f),
            Self::Generation(error) => error.fmt(f),
            Self::MaxSteps => f.write_str("reached the maximum number of steps"),
        }
    }
}

impl Error for AgentError {}

impl From<ToolError> for AgentError {
    fn from(error: ToolError) -> Self {
        Self::Tool(error)
    }
}

impl From<ModelError> for AgentError {
    fn from(error: ModelError) -> Self {
        Self::Generation(error)
    }
}
