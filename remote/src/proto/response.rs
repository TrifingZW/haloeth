//! 响应信封与结构化错误。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// 响应信封：服务端 → 客户端。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseFrame {
    /// 协议版本。
    pub v: u8,
    /// 与请求配对的序号。
    pub id: u64,
    /// 是否成功。
    pub ok: bool,
    /// 成功结果。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// 失败原因。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RemoteError>,
}

impl ResponseFrame {
    /// 成功响应。
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            v: crate::proto::PROTOCOL_VERSION,
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    /// 失败响应。
    pub fn err(id: u64, error: RemoteError) -> Self {
        Self {
            v: crate::proto::PROTOCOL_VERSION,
            id,
            ok: false,
            result: None,
            error: Some(error),
        }
    }
}

/// 请求错误码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// 未鉴权即发请求。
    Unauthorized,
    /// 运行中拒绝执行。
    Busy,
    /// 目标不存在。
    NotFound,
    /// 模型组合不可用。
    UnknownModel,
    /// 功能键不存在。
    UnknownFeature,
    /// 操作名无法识别。
    UnknownOp,
    /// 参数缺失或类型错误。
    InvalidParams,
    /// 触发限速。
    RateLimited,
    /// 宿主内部错误。
    Internal,
}

impl ErrorCode {
    /// 线上字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::Busy => "busy",
            Self::NotFound => "not_found",
            Self::UnknownModel => "unknown_model",
            Self::UnknownFeature => "unknown_feature",
            Self::UnknownOp => "unknown_op",
            Self::InvalidParams => "invalid_params",
            Self::RateLimited => "rate_limited",
            Self::Internal => "internal",
        }
    }
}

/// 结构化错误。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteError {
    /// 错误码。
    pub code: ErrorCode,
    /// 面向用户的说明。
    pub message: String,
    /// 结构化补充。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RemoteError {
    /// 以错误码与说明构造。
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// 运行中拒绝，并回显用户文本供客户端恢复输入。
    pub fn busy(text: impl Into<String>) -> Self {
        Self::new(ErrorCode::Busy, "运行中").with_data(json!({ "text": text.into() }))
    }

    /// 目标不存在。
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    /// 参数不合法。
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidParams, message)
    }

    /// 未知操作。
    pub fn unknown_op(op: &str) -> Self {
        Self::new(ErrorCode::UnknownOp, format!("未知操作：{op}"))
    }

    /// 模型不可用。
    pub fn unknown_model(name: impl Into<String>) -> Self {
        Self::new(ErrorCode::UnknownModel, format!("模型不可用：{}", name.into()))
    }

    /// 功能键不存在。
    pub fn unknown_feature(key: impl Into<String>) -> Self {
        Self::new(ErrorCode::UnknownFeature, format!("功能不存在：{}", key.into()))
    }

    /// 宿主内部错误。
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    /// 附加结构化数据。
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}：{}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for RemoteError {}
