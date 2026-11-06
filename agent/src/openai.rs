//! **OpenAI** 兼容后端的模型实现，实现 [`Model`] 契约。
//!
//! 该后端以 `chat/completions` 接口为契约，始终请求流式响应（**SSE**），
//! 并将增量帧转换为 [`StreamDelta`]。不使用 `reqwest` 的 `json` 特性，
//! 请求体与响应体均经 `serde_json` 自行编解码。

use std::fmt;
use std::future::Future;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::error::ModelError;
use crate::message::{Message, StreamDelta, ToolCallDelta, assemble, normalize};
use crate::metrics::TokenUsage;
use crate::model::{GenerateRequest, Model, ToolChoice};
use crate::text::truncate;

/// 服务端错误正文保留的最大长度。
const MAX_ERROR_BODY: usize = 500;

/// 拉取服务商可用的模型标识列表，请求 `GET {base_url}/models`。
///
/// 解析 **OpenAI** 兼容响应 `{"data": [{"id": "..."}]}`；结果去重并按字典序排列，
/// 以保证界面顺序稳定。服务商未返回任何模型时视为错误。
pub async fn list_models(options: &HttpModelOptions) -> Result<Vec<String>, ModelError> {
    let url = format!("{}/models", options.base_url.trim_end_matches('/'));
    let mut builder = reqwest::Client::new().get(url);
    if !options.api_key.is_empty() {
        builder = builder.bearer_auth(&options.api_key);
    }
    let response = builder.send().await.map_err(|error| ModelError::Transport {
        reason: error.to_string(),
    })?;
    let status = response.status();
    let body = response.text().await.map_err(|error| ModelError::Transport {
        reason: error.to_string(),
    })?;
    if !status.is_success() {
        return Err(ModelError::Rejected {
            status: status.as_u16(),
            body: truncate(&body, MAX_ERROR_BODY),
        });
    }
    parse_models(&body)
}

/// 解析模型列表响应。
fn parse_models(body: &str) -> Result<Vec<String>, ModelError> {
    #[derive(Deserialize)]
    struct ModelsResponse {
        #[serde(default)]
        data: Vec<ModelEntry>,
    }

    #[derive(Deserialize)]
    struct ModelEntry {
        #[serde(default)]
        id: Option<String>,
    }

    let response: ModelsResponse =
        serde_json::from_str(body).map_err(|error| ModelError::Malformed {
            reason: format!("invalid models response: {error}"),
        })?;
    let mut models: Vec<String> = response.data.into_iter().filter_map(|entry| entry.id).collect();
    models.sort();
    models.dedup();
    if models.is_empty() {
        return Err(ModelError::Malformed {
            reason: "the models response contained no model identifiers".to_owned(),
        });
    }
    Ok(models)
}

/// 后端配置。
#[derive(Clone)]
pub struct HttpModelOptions {
    /// 模型标识。
    pub model_id: String,
    /// **API** 密钥；为空时不发送鉴权头，供本地服务使用。
    pub api_key: String,
    /// 接口基地址，`chat/completions` 会追加其后。
    pub base_url: String,
    /// 附加请求参数，最后合并，可覆盖默认字段。
    pub extras: Map<String, Value>,
    /// 是否发送停止序列。
    pub send_stop_sequences: bool,
    /// 是否请求流式 **Token** 用量。
    pub stream_usage: bool,
}

impl HttpModelOptions {
    /// 以模型标识与 **API** 密钥构造配置；接口地址默认指向 **OpenAI** 官方接口。
    pub fn new(model_id: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            api_key: api_key.into(),
            base_url: "https://api.openai.com/v1".to_owned(),
            extras: Map::new(),
            send_stop_sequences: true,
            stream_usage: true,
        }
    }

    /// 设置接口基地址。
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// 添加附加请求参数。
    pub fn extra(mut self, key: impl Into<String>, value: Value) -> Self {
        self.extras.insert(key.into(), value);
        self
    }

    /// 设置是否发送停止序列。
    pub fn send_stop_sequences(mut self, enabled: bool) -> Self {
        self.send_stop_sequences = enabled;
        self
    }

    /// 设置是否请求流式 **Token** 用量。
    pub fn stream_usage(mut self, enabled: bool) -> Self {
        self.stream_usage = enabled;
        self
    }
}

impl fmt::Debug for HttpModelOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpModelOptions")
            .field("model_id", &self.model_id)
            .field("api_key", &"<redacted>")
            .field("base_url", &self.base_url)
            .field("extras", &self.extras)
            .field("send_stop_sequences", &self.send_stop_sequences)
            .field("stream_usage", &self.stream_usage)
            .finish()
    }
}

/// **OpenAI** 兼容后端。
pub struct HttpModel {
    client: reqwest::Client,
    options: HttpModelOptions,
}

impl HttpModel {
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

    /// 设置模型标识，用于会话内切换模型。
    pub fn set_model_id(&mut self, model_id: impl Into<String>) {
        self.options.model_id = model_id.into();
    }

    /// 返回接口地址。
    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.options.base_url.trim_end_matches('/'))
    }

    /// 构造请求体。
    ///
    /// 合并顺序为：默认字段 → 特定字段（停止序列、工具清单与选择策略）→ 附加参数；
    /// 附加参数优先级最高，对应 smolagents 的模型默认参数覆盖规则。
    fn request_body(&self, request: &GenerateRequest) -> Result<Value, ModelError> {
        let encode = |error: serde_json::Error| ModelError::Encode {
            reason: error.to_string(),
        };

        let mut body = Map::new();
        body.insert("model".into(), Value::String(self.options.model_id.clone()));
        body.insert(
            "messages".into(),
            serde_json::to_value(normalize(&request.messages)).map_err(encode)?,
        );
        body.insert("stream".into(), Value::Bool(true));
        if self.options.stream_usage {
            body.insert("stream_options".into(), json!({ "include_usage": true }));
        }
        if !request.tools.is_empty() {
            body.insert("tools".into(), serde_json::to_value(&request.tools).map_err(encode)?);
            if request.tool_choice != ToolChoice::None {
                body.insert(
                    "tool_choice".into(),
                    Value::String(request.tool_choice.as_str().to_owned()),
                );
            }
        }
        if self.options.send_stop_sequences && !request.stop_sequences.is_empty() {
            body.insert("stop".into(), json!(request.stop_sequences));
        }
        for (key, value) in &self.options.extras {
            body.insert(key.clone(), value.clone());
        }
        Ok(Value::Object(body))
    }
}

impl Model for HttpModel {
    fn model_id(&self) -> &str {
        &self.options.model_id
    }

    fn generate<F>(
        &self,
        request: GenerateRequest,
        mut on_delta: F,
    ) -> impl Future<Output = Result<Message, ModelError>> + Send
    where
        F: FnMut(StreamDelta) + Send,
    {
        async move {
            let body = self.request_body(&request)?;
            let payload = serde_json::to_vec(&body).map_err(|error| ModelError::Encode {
                reason: error.to_string(),
            })?;

            let mut builder = self
                .client
                .post(self.endpoint())
                .header(reqwest::header::CONTENT_TYPE, "application/json");
            if !self.options.api_key.is_empty() {
                builder = builder.bearer_auth(&self.options.api_key);
            }
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

            let mut decoder = FrameDecoder::default();
            let mut deltas = Vec::new();
            while let Some(chunk) =
                response.chunk().await.map_err(|error| ModelError::Transport {
                    reason: error.to_string(),
                })?
            {
                for delta in decoder.push(&chunk)? {
                    on_delta(delta.clone());
                    deltas.push(delta);
                }
            }

            Ok(assemble(deltas))
        }
    }
}

/// **SSE** 帧解码器。
///
/// 把字节流按行切分并转换为增量：仅处理 `data:` 行，注释、事件行与结束标记被忽略；
/// 跨越数据块的分行由内部缓冲拼接。
#[derive(Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    /// 追加一块数据，返回其中已完整的增量。
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<StreamDelta>, ModelError> {
        self.buffer.extend_from_slice(chunk);
        let mut deltas = Vec::new();
        while let Some(position) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=position).collect();
            let line = String::from_utf8_lossy(&line[..line.len() - 1]);
            if let Some(delta) = decode_line(line.trim_end_matches('\r'))? {
                deltas.push(delta);
            }
        }
        Ok(deltas)
    }
}

/// 解析单行 **SSE** 数据。
fn decode_line(line: &str) -> Result<Option<StreamDelta>, ModelError> {
    let Some(data) = line.strip_prefix("data:") else {
        return Ok(None);
    };
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return Ok(None);
    }
    let response: Response = serde_json::from_str(data).map_err(|error| ModelError::Malformed {
        reason: format!("invalid response frame: {error}"),
    })?;
    Ok(Some(response.into_delta()))
}

/// 服务端返回的响应体；流式帧与非流式响应共用结构。
#[derive(Debug, Default, Deserialize)]
struct Response {
    #[serde(default)]
    choices: Vec<ResponseChoice>,
    #[serde(default)]
    usage: Option<ResponseUsage>,
}

/// 一个候选结果；流式帧使用 `delta`，非流式响应使用 `message`。
#[derive(Debug, Default, Deserialize)]
struct ResponseChoice {
    #[serde(default)]
    message: Option<ResponsePayload>,
    #[serde(default)]
    delta: Option<ResponsePayload>,
}

/// 增量或消息载荷。
#[derive(Debug, Default, Deserialize)]
struct ResponsePayload {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ResponseCall>>,
}

/// 工具调用。
#[derive(Debug, Default, Deserialize)]
struct ResponseCall {
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<ResponseFunction>,
}

/// 工具调用的函数载荷。
#[derive(Debug, Default, Deserialize)]
struct ResponseFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// **Token** 用量。
#[derive(Debug, Default, Deserialize)]
struct ResponseUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
}

impl Response {
    /// 转换为增量；无候选结果时返回仅含用量的增量。
    fn into_delta(self) -> StreamDelta {
        let payload =
            self.choices.into_iter().next().and_then(|choice| choice.message.or(choice.delta));
        let payload = payload.unwrap_or_default();
        StreamDelta {
            content: payload.content.filter(|text| !text.is_empty()),
            tool_calls: payload
                .tool_calls
                .unwrap_or_default()
                .into_iter()
                .enumerate()
                .map(convert_call)
                .collect(),
            usage: self
                .usage
                .map(|usage| TokenUsage::new(usage.prompt_tokens, usage.completion_tokens)),
        }
    }
}

/// 转换工具调用分片；索引缺失时以出现位置代替。
fn convert_call((position, call): (usize, ResponseCall)) -> ToolCallDelta {
    let ResponseCall {
        index,
        id,
        function,
    } = call;
    let (name, arguments) = match function {
        Some(function) => (function.name, function.arguments),
        None => (None, None),
    };
    ToolCallDelta {
        index: index.unwrap_or(position),
        id,
        name,
        arguments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::wire::WireTool;
    use crate::tool::{InputSpec, InputType, ToolSpec};

    fn model() -> HttpModel {
        HttpModel::new(
            HttpModelOptions::new("gpt-4o-mini", "secret").base_url("https://api.example.com/v1"),
        )
    }

    fn search_tool() -> WireTool {
        WireTool::from_spec(
            &ToolSpec::new("search", "Performs a web search.", InputType::String)
                .input("query", InputSpec::new(InputType::String, "The search query.")),
        )
    }

    #[test]
    fn builds_request_body_with_tools_and_stream_options() {
        let request = GenerateRequest::new(vec![Message::user("hi")])
            .tools(vec![search_tool()])
            .stop_sequences(vec!["Observation:".into()]);

        let body = model().request_body(&request).expect("body should be built");

        assert_eq!(body["model"], "gpt-4o-mini");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"], json!({ "include_usage": true }));
        assert_eq!(body["messages"], json!([{ "role": "user", "content": "hi" }]));
        assert_eq!(body["tools"][0]["function"]["name"], "search");
        assert_eq!(body["tool_choice"], "required");
        assert_eq!(body["stop"], json!(["Observation:"]));
    }

    #[test]
    fn omits_tools_and_stop_when_disabled() {
        let request = GenerateRequest::new(vec![Message::user("hi")]).tool_choice(ToolChoice::None);
        let model =
            HttpModel::new(HttpModelOptions::new("gpt-4o-mini", "").send_stop_sequences(false));

        let body = model.request_body(&request).expect("body should be built");

        assert_eq!(body.get("tools"), None);
        assert_eq!(body.get("tool_choice"), None);
        assert_eq!(body.get("stop"), None);
    }

    #[test]
    fn extras_override_default_fields() {
        let model = HttpModel::new(
            HttpModelOptions::new("gpt-4o-mini", "secret").extra("model", json!("custom-model")),
        );

        let body = model
            .request_body(&GenerateRequest::new(vec![Message::user("hi")]))
            .expect("body should be built");

        assert_eq!(body["model"], "custom-model");
    }

    #[test]
    fn decodes_stream_frames_and_skips_terminator() {
        let mut decoder = FrameDecoder::default();

        let deltas = decoder
            .push(b"data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\ndata: [DONE]\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n")
            .expect("frames should decode");

        let contents: Vec<Option<String>> =
            deltas.iter().map(|delta| delta.content.clone()).collect();
        assert_eq!(contents, [Some("Hel".into()), Some("lo".into())]);
    }

    #[test]
    fn buffers_partial_frames_across_chunks() {
        let mut decoder = FrameDecoder::default();

        let incomplete = decoder
            .push(b"data: {\"choices\":[{\"delta\":{\"cont")
            .expect("partial frame should decode");
        let complete = decoder.push(b"ent\":\"hi\"}}]}\r\n").expect("frame should decode");

        assert!(incomplete.is_empty());
        assert_eq!(complete.len(), 1);
        assert_eq!(complete[0].content.as_deref(), Some("hi"));
    }

    #[test]
    fn decodes_tool_call_fragments() {
        let mut decoder = FrameDecoder::default();
        let frame = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"search","arguments":"{\"query\":\""}}]}}]}"#;

        let deltas = decoder.push(format!("{frame}\n").as_bytes()).expect("frame should decode");

        let call = &deltas[0].tool_calls[0];
        assert_eq!(call.index, 0);
        assert_eq!(call.id.as_deref(), Some("call_1"));
        assert_eq!(call.name.as_deref(), Some("search"));
        assert_eq!(call.arguments.as_deref(), Some("{\"query\":\""));
    }

    #[test]
    fn parses_usage_frames() {
        let mut decoder = FrameDecoder::default();
        let frame =
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":3}}\n";

        let deltas = decoder.push(frame.as_bytes()).expect("frame should decode");

        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].usage, Some(TokenUsage::new(12, 3)));
        assert!(deltas[0].content.is_none());
    }

    #[test]
    fn reports_malformed_frames() {
        let mut decoder = FrameDecoder::default();

        let error = decoder.push(b"data: not json\n").expect_err("frame should be rejected");

        assert!(matches!(error, ModelError::Malformed { .. }));
    }

    #[test]
    fn ignores_non_data_lines() {
        let mut decoder = FrameDecoder::default();

        let deltas =
            decoder.push(b": keep-alive\n\nevent: ping\n\n").expect("lines should be ignored");

        assert!(deltas.is_empty());
    }

    #[test]
    fn debug_redacts_api_key() {
        let options = HttpModelOptions::new("gpt-4o-mini", "top-secret");

        let rendered = format!("{options:?}");

        assert!(!rendered.contains("top-secret"));
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn parses_and_normalises_model_lists() {
        let body = r#"{"data":[{"id":"gpt-5-codex"},{"id":"claude-sonnet-4-5"},{"id":"gpt-5-codex"},{"object":"model"}]}"#;

        let models = parse_models(body).expect("models should parse");

        assert_eq!(models, ["claude-sonnet-4-5", "gpt-5-codex"]);
    }

    #[test]
    fn rejects_model_responses_without_identifiers() {
        assert!(matches!(parse_models(r#"{"data":[]}"#), Err(ModelError::Malformed { .. })));
        assert!(matches!(
            parse_models(r#"{"data":[{"object":"model"}]}"#),
            Err(ModelError::Malformed { .. })
        ));
        assert!(matches!(parse_models("not json"), Err(ModelError::Malformed { .. })));
    }
}
