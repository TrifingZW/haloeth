//! 模型抽象：屏蔽提供方差异，向编排循环提供统一的生成接口。
//!
//! 与 smolagents 的差异有两点：
//! 1) 不实现从文本中解析工具调用的回退路径，仅采用提供方原生的工具调用字段。
//! 2) 停止序列与流式用量的发送由后端配置显式控制，取代按模型名判断的兼容性开关。

use std::future::Future;

use crate::error::ModelError;
use crate::message::{Message, StreamDelta};
use crate::tool::wire::WireTool;

/// 工具选择策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolChoice {
    /// 必须调用工具。
    #[default]
    Required,
    /// 由模型决定是否调用工具。
    Auto,
    /// 禁止调用工具。
    None,
}

impl ToolChoice {
    /// 返回协议中的取值。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Auto => "auto",
            Self::None => "none",
        }
    }
}

/// 一次生成请求。
#[derive(Debug, Clone, PartialEq)]
pub struct GenerateRequest {
    /// 上下文消息。
    pub messages: Vec<Message>,
    /// 可调用的工具声明。
    pub tools: Vec<WireTool>,
    /// 工具选择策略。
    pub tool_choice: ToolChoice,
    /// 停止序列。
    pub stop_sequences: Vec<String>,
}

impl GenerateRequest {
    /// 以上下文消息构造请求；默认不携带工具，策略为 [`ToolChoice::Required`]。
    pub fn new(messages: Vec<Message>) -> Self {
        Self {
            messages,
            tools: Vec::new(),
            tool_choice: ToolChoice::Required,
            stop_sequences: Vec::new(),
        }
    }

    /// 设置可调用的工具声明。
    pub fn tools(mut self, tools: Vec<WireTool>) -> Self {
        self.tools = tools;
        self
    }

    /// 设置工具选择策略。
    pub fn tool_choice(mut self, choice: ToolChoice) -> Self {
        self.tool_choice = choice;
        self
    }

    /// 设置停止序列。
    pub fn stop_sequences(mut self, sequences: Vec<String>) -> Self {
        self.stop_sequences = sequences;
        self
    }
}

/// 模型契约。
///
/// 要求 [`Send`] 与 [`Sync`]：模型由代理持有，其生成过程可能在任意线程上执行。
pub trait Model: Send + Sync {
    /// 返回模型标识。
    fn model_id(&self) -> &str;

    /// 生成回复。
    ///
    /// 流式是唯一路径：非流式响应等价于单帧流式响应，因此实现统一以增量回调交付输出，
    /// 并返回聚合后的完整消息。`on_delta` 在增量到达时被调用。
    fn generate<F>(
        &self,
        request: GenerateRequest,
        on_delta: F,
    ) -> impl Future<Output = Result<Message, ModelError>> + Send
    where
        F: FnMut(StreamDelta) + Send;
}
