//! **Anthropic Messages** 协议后端。
//!
//! 与 **OpenAI Chat** 的差异集中在三处，本模块各自处理：
//! 1) 请求体：`system` 在顶层，`tools[].input_schema`，`tool_choice` 用 `auto` / `any` / `none`；
//! 2) 消息投影：user / assistant 交替，工具结果放进 user 的 `tool_result` 内容块；
//! 3) 流式事件：`message_start` / `content_block_*` / `message_delta` / `message_stop`。
//!
//! 传输与鉴权复用 [`HttpModelOptions`]：`x-api-key` 与 `anthropic-version` 是默认头，
//! 自定义头最后应用，因此可以覆盖版本或改用网关的鉴权方式。

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::error::ModelError;
use crate::message::{
    ContentBlock, ImageSource, Message, Role, StreamDelta, ToolCallDelta, assemble,
    retain_modalities,
};
use crate::metrics::TokenUsage;
use crate::model::{GenerateRequest, Model, ToolChoice};
use crate::openai::{HttpModelOptions, ToolChoicePolicy};
use crate::text::truncate;

/// 服务端错误正文保留的最大长度。
const MAX_ERROR_BODY: usize = 500;

/// 未配置 `max_tokens` 时的默认上限。
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// **Anthropic Messages** 后端。
pub struct AnthropicModel {
    client: reqwest::Client,
    options: HttpModelOptions,
}

impl AnthropicModel {
    /// 以后端配置构造模型。
    pub fn new(options: HttpModelOptions) -> Self {
        Self {
            client: reqwest::Client::new(),
            options,
        }
    }

    /// 返回后端配置。
    pub fn options(&self) -> &HttpModelOptions {
        &self.options
    }

    /// 设置模型标识。
    pub fn set_model_id(&mut self, model_id: impl Into<String>) {
        self.options.model_id = model_id.into();
    }

    /// 接口地址。
    fn endpoint(&self) -> String {
        format!("{}/messages", self.options.base_url.trim_end_matches('/'))
    }

    /// 构造请求体。
    fn request_body(&self, request: &GenerateRequest) -> Result<Value, ModelError> {
        let encode = |error: serde_json::Error| ModelError::Encode {
            reason: error.to_string(),
        };

        let messages = retain_modalities(&request.messages, &self.options.input_modalities);
        let (system, messages) = project_messages(&messages);

        let mut body = Map::new();
        body.insert("model".into(), Value::String(self.options.model_id.clone()));
        body.insert(
            "max_tokens".into(),
            json!(self.options.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)),
        );
        body.insert("stream".into(), Value::Bool(true));
        if let Some(system) = system {
            body.insert("system".into(), Value::String(system));
        }
        body.insert("messages".into(), Value::Array(messages));

        if !request.tools.is_empty() {
            let tools: Vec<Value> = request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.function.name,
                        "description": tool.function.description,
                        "input_schema": tool.function.parameters,
                    })
                })
                .collect();
            body.insert("tools".into(), Value::Array(tools));

            let choice = match request.tool_choice {
                ToolChoice::None => Some(json!({ "type": "none" })),
                _ => match self.options.tool_choice {
                    ToolChoicePolicy::Auto => Some(json!({ "type": "auto" })),
                    ToolChoicePolicy::Required => Some(json!({ "type": "any" })),
                    ToolChoicePolicy::Omit => None,
                },
            };
            if let Some(choice) = choice {
                body.insert("tool_choice".into(), choice);
            }
        }

        if self.options.send_stop_sequences && !request.stop_sequences.is_empty() {
            body.insert("stop_sequences".into(), json!(request.stop_sequences));
        }
        for (key, value) in &self.options.extras {
            body.insert(key.clone(), value.clone());
        }
        serde_json::to_value(Value::Object(body)).map_err(encode)
    }
}

#[async_trait::async_trait]
impl Model for AnthropicModel {
    fn model_id(&self) -> &str {
        &self.options.model_id
    }

    async fn generate(
        &self,
        request: GenerateRequest,
        on_delta: &mut (dyn FnMut(StreamDelta) + Send),
    ) -> Result<Message, ModelError> {
        let body = self.request_body(&request)?;
        let payload = serde_json::to_vec(&body).map_err(|error| ModelError::Encode {
            reason: error.to_string(),
        })?;

        let mut builder = self
            .client
            .post(self.endpoint())
            .header(reqwest::header::CONTENT_TYPE, "application/json");
        if !self.options.api_key.is_empty() && !self.options.custom_has("x-api-key") {
            builder = builder.header("x-api-key", self.options.api_key.as_str());
        }
        // 版本头与鉴权无关：用自定义头走网关时同样必须发送。
        if !self.options.custom_has("anthropic-version") {
            builder = builder.header("anthropic-version", self.options.anthropic_version.as_str());
        }
        builder = self.options.apply_headers(builder);

        let mut response =
            builder.body(payload).send().await.map_err(|error| ModelError::Transport {
                reason: error.to_string(),
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ModelError::Rejected {
                status: status.as_u16(),
                body: truncate(&body, MAX_ERROR_BODY),
            });
        }

        let mut decoder = AnthropicDecoder::default();
        let mut deltas = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| ModelError::Transport {
            reason: error.to_string(),
        })? {
            for delta in decoder.push(&chunk)? {
                on_delta(delta.clone());
                deltas.push(delta);
            }
        }

        Ok(assemble(deltas))
    }
}

/// 把内部消息投影为 Anthropic 的 `system` 与 `messages`。
///
/// 相邻同角色消息合并；`tool_result` 放进 user 消息；首条不是 user 时补一条
/// 继续指令，避免请求被协议拒绝。
pub(crate) fn project_messages(messages: &[Message]) -> (Option<String>, Vec<Value>) {
    let mut system: Vec<String> = Vec::new();
    let mut projected: Vec<(String, Vec<Value>)> = Vec::new();

    for message in messages {
        match message.role {
            Role::System => {
                let text = text_of(&message.content);
                if !text.is_empty() {
                    system.push(text);
                }
            }
            Role::User => push_role(&mut projected, "user", project_blocks(&message.content)),
            Role::Assistant | Role::ToolCall => {
                let mut blocks = project_blocks(&message.content);
                for call in &message.tool_calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": call.arguments,
                    }));
                }
                push_role(&mut projected, "assistant", blocks);
            }
            Role::ToolResponse => {
                let id = message.tool_call_id.clone().unwrap_or_default();
                let mut content = project_blocks(&message.content);
                if content.is_empty() {
                    content.push(json!({ "type": "text", "text": "(no output)" }));
                }
                push_role(
                    &mut projected,
                    "user",
                    vec![json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": content,
                    })],
                );
            }
        }
    }

    if projected.first().is_some_and(|(role, _)| role == "assistant") {
        projected.insert(
            0,
            (
                "user".to_owned(),
                vec![json!({ "type": "text", "text": "Continue the conversation." })],
            ),
        );
    }

    let messages = projected
        .into_iter()
        .map(|(role, content)| json!({ "role": role, "content": content }))
        .collect();
    let system = (!system.is_empty()).then(|| system.join("\n\n"));
    (system, messages)
}

/// 追加一条消息；与上一条角色相同时合并内容。
fn push_role(projected: &mut Vec<(String, Vec<Value>)>, role: &str, mut blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    match projected.last_mut() {
        Some((last_role, content)) if last_role == role => content.append(&mut blocks),
        _ => projected.push((role.to_owned(), blocks)),
    }
}

/// 投影内容块序列。
fn project_blocks(content: &[ContentBlock]) -> Vec<Value> {
    content.iter().map(project_block).collect()
}

/// 投影单个内容块。
fn project_block(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text(text) => json!({ "type": "text", "text": text }),
        ContentBlock::Image(source) => match source {
            ImageSource::Url(url) => {
                json!({ "type": "image", "source": { "type": "url", "url": url } })
            }
            ImageSource::Base64 {
                media_type,
                data,
            } => json!({
                "type": "image",
                "source": { "type": "base64", "media_type": media_type, "data": data },
            }),
        },
        ContentBlock::Audio(_) => json!({
            "type": "text",
            "text": "[audio omitted: the Anthropic Messages protocol does not accept audio input]",
        }),
    }
}

/// 拼接内容中的文本块。
fn text_of(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// **Anthropic** 的 **SSE** 解码器。
#[derive(Default)]
pub struct AnthropicDecoder {
    buffer: Vec<u8>,
    /// 已收到的输入用量，与输出用量合并后作为末帧用量。
    input_tokens: u32,
}

impl AnthropicDecoder {
    /// 追加一块数据，返回其中已完整的增量。
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<StreamDelta>, ModelError> {
        self.buffer.extend_from_slice(chunk);
        let mut deltas = Vec::new();
        let mut consumed = 0;

        while let Some(offset) = self.buffer[consumed..].iter().position(|byte| *byte == b'\n') {
            let end = consumed + offset;
            let line = String::from_utf8_lossy(&self.buffer[consumed..end]).to_string();
            consumed = end + 1;
            let Some(data) = line.trim_end_matches('\r').strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() {
                continue;
            }
            if let Some(delta) = self.decode(data)? {
                deltas.push(delta);
            }
        }

        self.buffer.drain(..consumed);
        Ok(deltas)
    }

    /// 解码一帧。
    fn decode(&mut self, data: &str) -> Result<Option<StreamDelta>, ModelError> {
        let event: Event = serde_json::from_str(data).map_err(|error| ModelError::Malformed {
            reason: format!("invalid Anthropic stream frame: {error}"),
        })?;

        match event.kind.as_str() {
            "message_start" => {
                if let Some(usage) = event.message.and_then(|message| message.usage) {
                    self.input_tokens = usage.input_tokens;
                    return Ok(Some(StreamDelta {
                        usage: Some(TokenUsage::new(usage.input_tokens, usage.output_tokens)),
                        ..StreamDelta::default()
                    }));
                }
                Ok(None)
            }
            "content_block_start" => {
                let Some(block) = event.content_block else {
                    return Ok(None);
                };
                if block.kind == "tool_use" {
                    return Ok(Some(StreamDelta {
                        tool_calls: vec![ToolCallDelta {
                            index: event.index.unwrap_or_default(),
                            id: block.id,
                            name: block.name,
                            arguments: None,
                        }],
                        ..StreamDelta::default()
                    }));
                }
                if block.kind == "text"
                    && let Some(text) = block.text
                    && !text.is_empty()
                {
                    return Ok(Some(StreamDelta {
                        content: Some(text),
                        ..StreamDelta::default()
                    }));
                }
                Ok(None)
            }
            "content_block_delta" => {
                let Some(delta) = event.delta else {
                    return Ok(None);
                };
                match delta.kind.as_str() {
                    "text_delta" => Ok(Some(StreamDelta {
                        content: delta.text,
                        ..StreamDelta::default()
                    })),
                    "thinking_delta" => Ok(Some(StreamDelta {
                        reasoning: delta.thinking,
                        ..StreamDelta::default()
                    })),
                    "input_json_delta" => Ok(delta.partial_json.map(|partial| StreamDelta {
                        tool_calls: vec![ToolCallDelta {
                            index: event.index.unwrap_or_default(),
                            id: None,
                            name: None,
                            arguments: Some(partial),
                        }],
                        ..StreamDelta::default()
                    })),
                    _ => Ok(None),
                }
            }
            "message_delta" => {
                let usage = event.usage.unwrap_or_default();
                Ok(Some(StreamDelta {
                    usage: Some(TokenUsage::new(self.input_tokens, usage.output_tokens)),
                    ..StreamDelta::default()
                }))
            }
            "error" => {
                let message = event
                    .error
                    .map(|error| format!("{}: {}", error.kind, error.message))
                    .unwrap_or_else(|| data.to_owned());
                Err(ModelError::Malformed {
                    reason: format!("Anthropic stream error: {message}"),
                })
            }
            _ => Ok(None),
        }
    }
}

/// 一条流式事件。
#[derive(Deserialize)]
struct Event {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    delta: Option<Delta>,
    #[serde(default)]
    content_block: Option<BlockStart>,
    #[serde(default)]
    message: Option<MessageStart>,
    #[serde(default)]
    usage: Option<Usage>,
    #[serde(default)]
    error: Option<ErrorBody>,
}

/// 增量载荷。
#[derive(Deserialize)]
struct Delta {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    partial_json: Option<String>,
}

/// 内容块起始载荷。
#[derive(Deserialize)]
struct BlockStart {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

/// 消息起始载荷。
#[derive(Deserialize)]
struct MessageStart {
    #[serde(default)]
    usage: Option<Usage>,
}

/// 用量载荷。
#[derive(Deserialize, Default)]
struct Usage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

/// 错误载荷。
#[derive(Deserialize)]
struct ErrorBody {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::ToolCall;
    use crate::tool::wire::WireTool;

    fn options() -> HttpModelOptions {
        HttpModelOptions::new("claude-sonnet-5", "sk-test")
            .base_url("https://api.anthropic.com/v1")
            .protocol(crate::model::Protocol::AnthropicMessages)
            .max_tokens(512)
    }

    fn model() -> AnthropicModel {
        AnthropicModel::new(options())
    }

    #[test]
    fn request_body_uses_top_level_system_and_input_schema() {
        let request =
            GenerateRequest::new(vec![Message::system("You are helpful."), Message::user("hi")])
                .tools(vec![WireTool::from_spec(
                    &crate::tool::ToolSpec::new("echo", "Echo.", crate::tool::InputType::String)
                        .input(
                            "text",
                            crate::tool::InputSpec::new(crate::tool::InputType::String, "Text."),
                        ),
                )])
                .tool_choice(ToolChoice::Required);

        let body = model().request_body(&request).expect("body");

        assert_eq!(body["system"], json!("You are helpful."));
        assert_eq!(body["max_tokens"], json!(512));
        assert_eq!(body["stream"], json!(true));
        assert_eq!(body["tools"][0]["name"], json!("echo"));
        assert!(body["tools"][0].get("input_schema").is_some());
        assert_eq!(body["tool_choice"]["type"], json!("auto"));
        assert_eq!(body["messages"][0]["role"], json!("user"));
    }

    #[test]
    fn tool_results_are_projected_into_user_content() {
        let request = GenerateRequest::new(vec![
            Message::user("read it"),
            Message::tool_calls(vec![ToolCall {
                id: "call_1".into(),
                name: "read_file".into(),
                arguments: json!({ "path": "a.txt" }),
            }]),
            Message::tool_response("call_1", "hello"),
        ]);

        let body = model().request_body(&request).expect("body");
        let messages = body["messages"].as_array().expect("messages");

        assert_eq!(messages[1]["role"], json!("assistant"));
        assert_eq!(messages[1]["content"][0]["type"], json!("tool_use"));
        assert_eq!(messages[2]["role"], json!("user"));
        assert_eq!(messages[2]["content"][0]["type"], json!("tool_result"));
        assert_eq!(messages[2]["content"][0]["tool_use_id"], json!("call_1"));
    }

    #[test]
    fn assistant_first_history_is_prefixed_with_a_user_turn() {
        let request = GenerateRequest::new(vec![
            Message::assistant("previous answer"),
            Message::user("next"),
        ]);

        let body = model().request_body(&request).expect("body");
        let messages = body["messages"].as_array().expect("messages");

        assert_eq!(messages[0]["role"], json!("user"));
        assert_eq!(messages[1]["role"], json!("assistant"));
    }

    #[test]
    fn decodes_text_tool_and_usage_events() {
        let mut decoder = AnthropicDecoder::default();
        let frames = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12,\"output_tokens\":1}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"echo\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"text\\\":\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"hi\\\"}\"}}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":7}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );

        let deltas = decoder.push(frames.as_bytes()).expect("decode");
        let message = assemble(deltas);

        assert_eq!(message.content, vec![ContentBlock::text("Hello")]);
        assert_eq!(message.tool_calls.len(), 1);
        assert_eq!(message.tool_calls[0].id, "toolu_1");
        assert_eq!(message.tool_calls[0].name, "echo");
        assert_eq!(message.tool_calls[0].arguments, json!({ "text": "hi" }));
        assert_eq!(message.usage.expect("usage").total(), 19);
    }

    #[test]
    fn thinking_deltas_become_reasoning() {
        let mut decoder = AnthropicDecoder::default();
        let frame = "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hmm\"}}\n";

        let deltas = decoder.push(frame.as_bytes()).expect("decode");

        assert_eq!(deltas[0].reasoning.as_deref(), Some("hmm"));
        assert!(deltas[0].content.is_none());
    }

    #[test]
    fn stream_errors_are_reported() {
        let mut decoder = AnthropicDecoder::default();
        let frame = "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"busy\"}}\n";

        let error = decoder.push(frame.as_bytes()).expect_err("error frame");

        assert!(error.to_string().contains("overloaded_error"));
    }

    #[test]
    fn buffers_frames_split_across_chunks() {
        let mut decoder = AnthropicDecoder::default();
        let frame = "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n";

        assert!(decoder.push(&frame.as_bytes()[..20]).expect("partial").is_empty());
        let deltas = decoder.push(&frame.as_bytes()[20..]).expect("rest");

        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].content.as_deref(), Some("Hi"));
    }
}
