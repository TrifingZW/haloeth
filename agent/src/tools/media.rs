//! 生成类工具：AI 绘图、视频与音乐。
//!
//! 绘图走 **OpenAI** 兼容的 `images/generations`；视频与音乐没有统一标准，
//! 使用「提交任务 → 轮询状态 → 下载产物」的通用适配器，地址与字段路径可配置。
//! 产物路径由模型在参数里给出，写入前照常走审核。

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::error::ToolError;
use crate::message::ContentBlock;
use crate::tool::{
    Access, ApprovalRequirement, InputSpec, InputType, Tool, ToolEffect, ToolFuture, ToolOutput,
    ToolSpec,
};

/// 绘图工具名。
pub const GENERATE_IMAGE: &str = "generate_image";
/// 视频工具名。
pub const GENERATE_VIDEO: &str = "generate_video";
/// 音乐工具名。
pub const GENERATE_MUSIC: &str = "generate_music";
/// 展示本地图片的工具名。
pub const SHOW_IMAGE: &str = "show_image";

/// 网络请求超时。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// 绘图配置。
#[derive(Debug, Clone)]
pub struct ImageOptions {
    /// 接口地址（完整 URL）。
    pub endpoint: String,
    /// **API** 密钥。
    pub api_key: String,
    /// 模型标识。
    pub model: String,
    /// 请求头。
    pub headers: BTreeMap<String, String>,
}

/// 异步任务配置。
#[derive(Debug, Clone)]
pub struct JobOptions {
    /// 任务名，用于错误信息。
    pub name: String,
    /// 提交地址。
    pub endpoint: String,
    /// **API** 密钥。
    pub api_key: String,
    /// 模型标识。
    pub model: String,
    /// 请求体模板；字符串里的 `{{prompt}}` 会被替换。
    pub body_template: Option<Value>,
    /// 轮询地址模板，支持 `{{base}}`、`{{id}}`。
    pub poll_url: String,
    /// 任务标识字段路径。
    pub id_path: String,
    /// 状态字段路径。
    pub status_path: String,
    /// 完成状态值。
    pub done_value: String,
    /// 结果地址字段路径。
    pub result_path: String,
    /// 轮询间隔。
    pub interval: Duration,
    /// 超时。
    pub timeout: Duration,
    /// 请求头。
    pub headers: BTreeMap<String, String>,
}

impl JobOptions {
    /// 是否配置完整。
    pub fn is_configured(&self) -> bool {
        !self.endpoint.trim().is_empty() && !self.poll_url.trim().is_empty()
    }
}

/// **AI** 绘图工具。
pub struct ImageTool {
    spec: ToolSpec,
    options: ImageOptions,
    client: reqwest::Client,
}

impl ImageTool {
    /// 构造工具。
    pub fn new(options: ImageOptions) -> Self {
        let spec = ToolSpec::new(
            GENERATE_IMAGE,
            "Generates an image from a text prompt and saves it to output_path. Ask the user for the destination path if they did not provide one.",
            InputType::String,
        )
        .input("prompt", InputSpec::new(InputType::String, "What to draw."))
        .input(
            "output_path",
            InputSpec::new(InputType::String, "Absolute path of the image file to write."),
        )
        .input(
            "size",
            InputSpec::new(InputType::String, "Image size such as 1024x1024.").nullable(),
        )
        .effect(ToolEffect::Path {
            argument: "output_path".to_owned(),
            access: Access::Write,
        })
        .effect(ToolEffect::Network {
            argument: None,
        })
        .approval(ApprovalRequirement::Policy);
        Self {
            spec,
            options,
            client: reqwest::Client::new(),
        }
    }
}

impl Tool for ImageTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke<'a>(&'a self, arguments: &'a Map<String, Value>) -> ToolFuture<'a> {
        Box::pin(async move {
            let prompt = required(arguments, "prompt")?;
            let output = required(arguments, "output_path")?;
            let size = arguments.get("size").and_then(Value::as_str).unwrap_or("1024x1024");

            let body = json!({
                "model": self.options.model,
                "prompt": prompt,
                "n": 1,
                "size": size,
                "response_format": "b64_json",
            });
            let mut request = self.client.post(&self.options.endpoint).timeout(REQUEST_TIMEOUT);
            if !self.options.api_key.is_empty() {
                request = request.bearer_auth(&self.options.api_key);
            }
            for (name, value) in &self.options.headers {
                request = request.header(name.as_str(), value.as_str());
            }
            let response =
                request.json(&body).send().await.map_err(|error| execution(error.to_string()))?;
            let status = response.status();
            let text = response.text().await.map_err(|error| execution(error.to_string()))?;
            if !status.is_success() {
                return Err(execution(format!(
                    "image request failed with status {status}: {text}"
                )));
            }
            let parsed: ImagesResponse = serde_json::from_str(&text)
                .map_err(|error| execution(format!("invalid image response: {error}")))?;
            let usage = parsed.usage.clone();
            let Some(first) = parsed.data.into_iter().next() else {
                return Err(execution("the image response contained no data"));
            };

            let (bytes, media_type) = if let Some(data) = first.b64_json {
                (decode_base64(&data)?, "image/png".to_owned())
            } else if let Some(url) = first.url {
                let response = self
                    .client
                    .get(&url)
                    .timeout(REQUEST_TIMEOUT)
                    .send()
                    .await
                    .map_err(|error| execution(error.to_string()))?;
                let media_type = response
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("image/png")
                    .to_owned();
                let bytes = response.bytes().await.map_err(|error| execution(error.to_string()))?;
                (bytes.to_vec(), media_type)
            } else {
                return Err(execution("the image response contained neither b64_json nor url"));
            };

            save(&output, &bytes).await?;
            let encoded = encode_base64(&bytes);
            let mut blocks = vec![
                ContentBlock::text(format!("Image saved to {output}")),
                ContentBlock::image_base64(media_type, encoded),
            ];
            if let Some(usage) = usage {
                blocks.push(ContentBlock::text(format!(
                    "__usage__{}",
                    json!({ "model": self.options.model, "usage": usage })
                )));
            }
            Ok(ToolOutput::Blocks(blocks))
        })
    }
}

/// 通用异步任务工具（视频 / 音乐）。
pub struct JobTool {
    spec: ToolSpec,
    options: JobOptions,
    /// 产物是图片时附带图像内容块。
    image: bool,
    client: reqwest::Client,
}

impl JobTool {
    /// 构造工具。
    pub fn new(name: &str, description: &str, options: JobOptions) -> Self {
        Self::build(name, description, options, false)
    }

    /// 构造产物为图片的任务工具。
    pub fn new_image(name: &str, description: &str, options: JobOptions) -> Self {
        Self::build(name, description, options, true)
    }

    /// 构造工具。
    fn build(name: &str, description: &str, options: JobOptions, image: bool) -> Self {
        let spec = ToolSpec::new(name, description, InputType::String)
            .input("prompt", InputSpec::new(InputType::String, "What to generate."))
            .input(
                "output_path",
                InputSpec::new(InputType::String, "Absolute path of the file to write."),
            )
            .effect(ToolEffect::Path {
                argument: "output_path".to_owned(),
                access: Access::Write,
            })
            .effect(ToolEffect::Network {
                argument: None,
            })
            .approval(ApprovalRequirement::Policy);
        Self {
            spec,
            options,
            image,
            client: reqwest::Client::new(),
        }
    }

    /// 提交任务，返回任务标识。
    async fn submit(&self, prompt: &str) -> Result<String, ToolError> {
        let mut body = self
            .options
            .body_template
            .clone()
            .unwrap_or_else(|| json!({ "model": self.options.model, "prompt": prompt }));
        if self.options.body_template.is_some() {
            body = fill(&body, prompt);
            if let Value::Object(map) = &mut body {
                map.entry("model").or_insert(json!(self.options.model));
            }
        }

        let mut request = self.client.post(&self.options.endpoint).timeout(REQUEST_TIMEOUT);
        if !self.options.api_key.is_empty() {
            request = request.bearer_auth(&self.options.api_key);
        }
        for (name, value) in &self.options.headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let response =
            request.json(&body).send().await.map_err(|error| execution(error.to_string()))?;
        let status = response.status();
        let text = response.text().await.map_err(|error| execution(error.to_string()))?;
        if !status.is_success() {
            return Err(execution(format!("{status}: {text}")));
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|error| execution(format!("invalid task response: {error}")))?;
        pointer(&value, &self.options.id_path)
            .map(|value| match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            })
            .ok_or_else(|| execution(format!("missing task id at {}", self.options.id_path)))
    }

    /// 轮询直到完成，返回结果地址。
    async fn wait(&self, id: &str) -> Result<String, ToolError> {
        let base = base_of(&self.options.endpoint);
        let url = self.options.poll_url.replace("{{base}}", &base).replace("{{id}}", id);
        let started = std::time::Instant::now();
        loop {
            if started.elapsed() > self.options.timeout {
                return Err(execution("the generation task timed out"));
            }
            tokio::time::sleep(self.options.interval).await;
            let mut request = self.client.get(&url).timeout(REQUEST_TIMEOUT);
            if !self.options.api_key.is_empty() {
                request = request.bearer_auth(&self.options.api_key);
            }
            for (name, value) in &self.options.headers {
                request = request.header(name.as_str(), value.as_str());
            }
            let response = request.send().await.map_err(|error| execution(error.to_string()))?;
            let http = response.status();
            let text = response.text().await.map_err(|error| execution(error.to_string()))?;
            if !http.is_success() {
                return Err(execution(format!(
                    "polling the task failed: {http}: {}",
                    crate::text::truncate(text.trim(), 300)
                )));
            }
            let value: Value = serde_json::from_str(&text)
                .map_err(|error| execution(format!("invalid poll response: {error}")))?;
            let status = pointer(&value, &self.options.status_path)
                .map(|value| match value {
                    Value::String(text) => text.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            if status.eq_ignore_ascii_case(&self.options.done_value) {
                return pointer(&value, &self.options.result_path)
                    .map(|value| match value {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    })
                    .ok_or_else(|| {
                        execution(format!("missing result at {}", self.options.result_path))
                    });
            }
            // 失败状态是终态：继续轮询只会拖到超时，并吞掉真正的原因。
            if failure_status(&status) {
                let detail = failure_detail(&value);
                let detail = if detail.is_empty() {
                    String::new()
                } else {
                    format!(": {detail}")
                };
                return Err(execution(format!("the generation task failed ({status}){detail}")));
            }
        }
    }
}

/// 是否是任务失败的终态。
fn failure_status(status: &str) -> bool {
    let status = status.trim().to_ascii_lowercase();
    matches!(
        status.as_str(),
        "failed"
            | "failure"
            | "error"
            | "canceled"
            | "cancelled"
            | "expired"
            | "unknown"
            | "rejected"
    )
}

/// 从任务响应里找出失败原因。
fn failure_detail(value: &Value) -> String {
    for path in ["output.message", "output.task_status_msg", "message", "error.message", "error"] {
        if let Some(detail) = pointer(value, path) {
            let detail = match detail {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            if !detail.trim().is_empty() {
                return crate::text::truncate(detail.trim(), 300);
            }
        }
    }
    String::new()
}

impl Tool for JobTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke<'a>(&'a self, arguments: &'a Map<String, Value>) -> ToolFuture<'a> {
        Box::pin(async move {
            let prompt = required(arguments, "prompt")?;
            let output = required(arguments, "output_path")?;
            let id = self.submit(&prompt).await?;
            let url = self.wait(&id).await?;
            let response = self
                .client
                .get(&url)
                .timeout(REQUEST_TIMEOUT)
                .send()
                .await
                .map_err(|error| execution(error.to_string()))?;
            if !response.status().is_success() {
                return Err(execution(format!("downloading {url} failed: {}", response.status())));
            }
            let bytes = response.bytes().await.map_err(|error| execution(error.to_string()))?;
            save(&output, &bytes).await?;
            if self.image {
                let media_type = image_media_type(&output, &url);
                Ok(ToolOutput::Blocks(vec![
                    ContentBlock::text(format!("{} saved to {output}", self.options.name)),
                    ContentBlock::image_base64(media_type, encode_base64(&bytes)),
                ]))
            } else {
                Ok(ToolOutput::Json(Value::String(format!(
                    "{} saved to {output}",
                    self.options.name
                ))))
            }
        })
    }
}

/// 在对话中展示一张本地图片。
pub struct ShowImageTool {
    spec: ToolSpec,
}

impl ShowImageTool {
    /// 构造工具。
    pub fn new() -> Self {
        let spec = ToolSpec::new(
            SHOW_IMAGE,
            "Shows a local image file in the conversation for the user. Use it after saving or finding an image on disk.",
            InputType::String,
        )
        .input(
            "path",
            InputSpec::new(InputType::String, "Absolute path of the local image file."),
        )
        .input(
            "caption",
            InputSpec::new(InputType::String, "Optional short caption.").nullable(),
        )
        .effect(ToolEffect::Path {
            argument: "path".to_owned(),
            access: Access::Read,
        })
        // 只把本地图片展示给用户，不向模型发送文件内容，因此不打断用户。
        .approval(ApprovalRequirement::Never);
        Self {
            spec,
        }
    }
}

impl Default for ShowImageTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for ShowImageTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke<'a>(&'a self, arguments: &'a Map<String, Value>) -> ToolFuture<'a> {
        Box::pin(async move {
            let path = required(arguments, "path")?;
            let path = Path::new(&path);
            if !path.is_absolute() {
                return Err(invalid("path must be an absolute path"));
            }
            let extension = path
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if !matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp") {
                return Err(invalid("path must point to a png/jpg/webp/gif/bmp image"));
            }
            if !path.is_file() {
                return Err(execution(format!("image not found: {}", path.display())));
            }
            let caption = arguments.get("caption").and_then(Value::as_str).unwrap_or_default();
            let text = if caption.trim().is_empty() {
                format!("Displayed image: {}", path.display())
            } else {
                format!("Displayed image: {} ({})", path.display(), caption.trim())
            };
            Ok(ToolOutput::Json(Value::String(text)))
        })
    }
}

/// 绘图响应。
#[derive(Deserialize)]
struct ImagesResponse {
    #[serde(default)]
    data: Vec<ImageData>,
    #[serde(default)]
    usage: Option<Value>,
}

/// 单张图片。
#[derive(Deserialize)]
struct ImageData {
    #[serde(default)]
    b64_json: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

/// 取必填字符串参数。
fn required(arguments: &Map<String, Value>, name: &str) -> Result<String, ToolError> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid(format!("{name} is required")))
}

/// 写入文件；必要时创建父目录。
async fn save(path: &str, bytes: &[u8]) -> Result<(), ToolError> {
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err(invalid("output_path must be an absolute path"));
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| execution(format!("creating {} failed: {error}", parent.display())))?;
    }
    tokio::fs::write(path, bytes)
        .await
        .map_err(|error| execution(format!("writing {} failed: {error}", path.display())))
}

/// 按点号路径读取字段；支持对象键与数组下标。
fn pointer<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').filter(|part| !part.is_empty()).try_fold(value, |value, key| {
        if let Some(items) = value.as_array() {
            key.parse::<usize>().ok().and_then(|index| items.get(index))
        } else {
            value.get(key)
        }
    })
}

/// 根据扩展名或下载地址推断图片媒体类型。
fn image_media_type(path: &str, url: &str) -> String {
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "jpg" | "jpeg" => "image/jpeg".to_owned(),
        "webp" => "image/webp".to_owned(),
        "gif" => "image/gif".to_owned(),
        "" => {
            if url.to_ascii_lowercase().contains(".jpg") {
                "image/jpeg".to_owned()
            } else {
                "image/png".to_owned()
            }
        }
        _ => "image/png".to_owned(),
    }
}

/// 递归替换字符串里的 `{{prompt}}`。
fn fill(value: &Value, prompt: &str) -> Value {
    match value {
        Value::String(text) => Value::String(text.replace("{{prompt}}", prompt)),
        Value::Array(items) => Value::Array(items.iter().map(|item| fill(item, prompt)).collect()),
        Value::Object(map) => Value::Object(
            map.iter().map(|(key, value)| (key.clone(), fill(value, prompt))).collect(),
        ),
        other => other.clone(),
    }
}

/// 提交地址的基地址（去掉路径部分）。
fn base_of(endpoint: &str) -> String {
    let trimmed = endpoint.trim_end_matches('/');
    match trimmed.find("://") {
        Some(scheme_end) => match trimmed[scheme_end + 3..].find('/') {
            Some(path) => trimmed[..scheme_end + 3 + path].to_owned(),
            None => trimmed.to_owned(),
        },
        None => trimmed.to_owned(),
    }
}

/// 简易 base64 编码，避免为单个工具引入额外依赖。
/// 简易 base64 编码。
pub fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        output.push(TABLE[(triple >> 18) as usize & 63] as char);
        output.push(TABLE[(triple >> 12) as usize & 63] as char);
        output.push(if chunk.len() > 1 {
            TABLE[(triple >> 6) as usize & 63] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[triple as usize & 63] as char
        } else {
            '='
        });
    }
    output
}

/// 简易 base64 解码。
/// 简易 base64 解码。
pub fn decode_base64(text: &str) -> Result<Vec<u8>, ToolError> {
    let cleaned: Vec<u8> = text.bytes().filter(|byte| !byte.is_ascii_whitespace()).collect();
    let mut output = Vec::with_capacity(cleaned.len() / 4 * 3);
    let mut buffer = 0_u32;
    let mut bits = 0_u32;
    for byte in cleaned {
        if byte == b'=' {
            break;
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(execution("invalid base64 image data")),
        } as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Ok(output)
}

/// 参数错误。
fn invalid(reason: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        name: "generate".to_owned(),
        reason: reason.into(),
    }
}

/// 执行错误。
fn execution(reason: impl Into<String>) -> ToolError {
    ToolError::ExecutionFailed {
        name: "generate".to_owned(),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointers_follow_object_paths_and_indices() {
        let value = json!({ "data": { "task": { "id": "abc" } }, "output": { "results": [{ "url": "u" }] } });
        assert_eq!(pointer(&value, "data.task.id").and_then(Value::as_str), Some("abc"));
        assert_eq!(pointer(&value, "output.results.0.url").and_then(Value::as_str), Some("u"));
        assert!(pointer(&value, "data.missing").is_none());
    }

    #[test]
    fn templates_replace_the_prompt_recursively() {
        let template = json!({ "input": { "text": "{{prompt}}" }, "list": ["{{prompt}}-2"] });
        let filled = fill(&template, "a cat");
        assert_eq!(filled["input"]["text"], json!("a cat"));
        assert_eq!(filled["list"][0], json!("a cat-2"));
    }

    #[test]
    fn failure_statuses_end_the_poll_early() {
        assert!(failure_status("FAILED"));
        assert!(failure_status(" canceled "));
        assert!(!failure_status("PENDING"));
        assert!(!failure_status("RUNNING"));
        assert!(!failure_status("SUCCEEDED"));

        let value = json!({ "output": { "task_status": "FAILED", "message": "bad prompt" } });
        assert_eq!(failure_detail(&value), "bad prompt");
        assert_eq!(failure_detail(&json!({ "output": {} })), "");
    }

    #[test]
    fn base64_round_trips() {
        let data = b"hello world";
        assert_eq!(decode_base64(&encode_base64(data)).expect("decode"), data);
        assert_eq!(encode_base64(b"a"), "YQ==");
    }

    #[test]
    fn base_url_strips_the_path() {
        assert_eq!(base_of("https://api.example.com/v1/videos"), "https://api.example.com");
        assert_eq!(base_of("http://localhost:8080/tasks"), "http://localhost:8080");
    }

    #[tokio::test]
    async fn show_image_accepts_an_existing_local_file() {
        let dir = std::env::temp_dir().join(format!("haloeth-show-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("a.png");
        std::fs::write(&path, b"png").expect("file");
        let tool = ShowImageTool::new();
        let mut arguments = Map::new();
        arguments.insert("path".into(), Value::String(path.display().to_string()));
        let output = tool.invoke(&arguments).await.expect("show image");
        assert!(output.observation().contains("Displayed image"));
        assert!(tool.invoke(&Map::new()).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn image_tools_declare_path_and_network_effects() {
        let tool = ImageTool::new(ImageOptions {
            endpoint: "https://example.com/v1/images/generations".into(),
            api_key: String::new(),
            model: "img".into(),
            headers: BTreeMap::new(),
        });
        let effects = &tool.spec().effects;
        assert!(effects.iter().any(|effect| matches!(
            effect,
            ToolEffect::Path { argument, access: Access::Write } if argument == "output_path"
        )));
        assert!(effects.iter().any(|effect| matches!(effect, ToolEffect::Network { .. })));
    }
}
