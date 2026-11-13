//! 模型抽象：屏蔽提供方差异，向编排循环提供统一的生成接口。
//!
//! 与 smolagents 的差异有两点：
//! 1) 不实现从文本中解析工具调用的回退路径，仅采用提供方原生的工具调用字段。
//! 2) 停止序列与流式用量的发送由后端配置显式控制，取代按模型名判断的兼容性开关。
//!
//! 生成接口以 `&mut dyn FnMut` 交付增量并返回装箱 future：这样 [`Model`] 保持对象安全，
//! 会话可以用 `Box<dyn Model>` 持有生产模型，在测试中换成脚本模型。

use std::sync::Arc;

use async_trait::async_trait;

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
/// 流式是唯一路径：非流式响应等价于单帧流式响应，因此实现统一以增量回调交付输出，
/// 并返回聚合后的完整消息。
#[async_trait]
pub trait Model: Send + Sync {
    /// 返回模型标识。
    fn model_id(&self) -> &str;

    /// 生成回复；`on_delta` 在增量到达时被调用。
    async fn generate(
        &self,
        request: GenerateRequest,
        on_delta: &mut (dyn FnMut(StreamDelta) + Send),
    ) -> Result<Message, ModelError>;
}

#[async_trait]
impl Model for Box<dyn Model> {
    fn model_id(&self) -> &str {
        self.as_ref().model_id()
    }

    async fn generate(
        &self,
        request: GenerateRequest,
        on_delta: &mut (dyn FnMut(StreamDelta) + Send),
    ) -> Result<Message, ModelError> {
        self.as_ref().generate(request, on_delta).await
    }
}

/// 模型工厂：会话在每次运行前用它构建模型，测试可注入脚本模型。
pub type ModelBuilder = Arc<dyn Fn() -> Box<dyn Model> + Send + Sync>;

#[cfg(test)]
mod tests {
    use super::*;

    /// 记录调用的模型，用于验证 `dyn Model` 的对象安全性。
    struct Echo;

    #[async_trait]
    impl Model for Echo {
        fn model_id(&self) -> &str {
            "echo"
        }

        async fn generate(
            &self,
            request: GenerateRequest,
            on_delta: &mut (dyn FnMut(StreamDelta) + Send),
        ) -> Result<Message, ModelError> {
            on_delta(StreamDelta {
                content: Some(format!("{} messages", request.messages.len())),
                ..Default::default()
            });
            Ok(Message::assistant("ok"))
        }
    }

    #[tokio::test]
    async fn boxed_models_dispatch_through_the_trait_object() {
        let model: Box<dyn Model> = Box::new(Echo);
        assert_eq!(model.model_id(), "echo");

        let mut streamed = String::new();
        let mut sink = |delta: StreamDelta| {
            if let Some(text) = delta.content {
                streamed.push_str(&text);
            }
        };
        let message = model
            .generate(GenerateRequest::new(vec![Message::user("hi")]), &mut sink)
            .await
            .expect("generation should succeed");

        assert_eq!(message.content, vec![crate::message::ContentBlock::text("ok")]);
        assert_eq!(streamed, "1 messages");
    }
}
