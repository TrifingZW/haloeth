//! 搜索工具：封装提供商自带的独立搜索接口。
//!
//! 首个适配对象是 **DeepSeek** 的 `POST {base_url}/tools/search`。各家的响应字段并不统一，
//! 因此解析保持宽容：常见的结果数组、单个答案字段都能识别，其余情况原样返回正文。

use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::error::ToolError;
use crate::text::truncate;
use crate::tool::{InputSpec, InputType, Tool, ToolEffect, ToolFuture, ToolSpec};

/// 搜索工具的固定名称。
pub const WEB_SEARCH: &str = "web_search";

/// 响应正文保留的最大长度。
const MAX_RESPONSE: usize = 40_000;

/// 单条摘要保留的最大长度。
const MAX_SNIPPET: usize = 800;

/// 搜索接口配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchOptions {
    /// 接口地址，例如 `https://api.deepseek.com/v1/tools/search`。
    pub endpoint: String,
    /// **API** 密钥；为空时不发送鉴权头。
    pub api_key: String,
    /// 默认返回结果数。
    pub max_results: usize,
}

impl SearchOptions {
    /// 构造配置。
    pub fn new(endpoint: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            api_key: api_key.into(),
            max_results: 5,
        }
    }
}

/// 提供商搜索工具。
pub struct WebSearchTool {
    client: reqwest::Client,
    options: SearchOptions,
    spec: ToolSpec,
}

impl WebSearchTool {
    /// 构造工具；接口地址必须使用 http 或 https。
    pub fn new(options: SearchOptions) -> Result<Self, ToolError> {
        if !(options.endpoint.starts_with("http://") || options.endpoint.starts_with("https://")) {
            return Err(ToolError::InvalidSpec {
                name: WEB_SEARCH.to_owned(),
                reason: "the search endpoint must be an http(s) URL".to_owned(),
            });
        }
        let spec = ToolSpec::new(
            WEB_SEARCH,
            "Searches the web through the configured provider and returns titles, URLs and snippets.",
            InputType::String,
        )
        .input("query", InputSpec::new(InputType::String, "Search query."))
        .input(
            "max_results",
            InputSpec::new(InputType::Integer, "Maximum number of results.").nullable(),
        )
        .effect(ToolEffect::Network {
            argument: None,
        });
        let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().map_err(
            |error| ToolError::InvalidSpec {
                name: WEB_SEARCH.to_owned(),
                reason: error.to_string(),
            },
        )?;
        Ok(Self {
            client,
            options,
            spec,
        })
    }

    /// 构造请求体。
    fn request_body(&self, query: &str, max_results: usize) -> Value {
        json!({
            "query": query,
            "max_results": max_results,
        })
    }
}

impl Tool for WebSearchTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke<'a>(&'a self, arguments: &'a Map<String, Value>) -> ToolFuture<'a> {
        Box::pin(async move {
            let query = arguments.get("query").and_then(Value::as_str).unwrap_or_default();
            let max_results = arguments
                .get("max_results")
                .and_then(Value::as_u64)
                .map(|value| value as usize)
                .unwrap_or(self.options.max_results)
                .clamp(1, 20);

            let mut request = self
                .client
                .post(&self.options.endpoint)
                .json(&self.request_body(query, max_results));
            if !self.options.api_key.is_empty() {
                request = request.bearer_auth(&self.options.api_key);
            }
            let response = request.send().await.map_err(|error| ToolError::ExecutionFailed {
                name: WEB_SEARCH.to_owned(),
                reason: format!("search request failed: {error}"),
            })?;
            let status = response.status();
            let body = response.text().await.map_err(|error| ToolError::ExecutionFailed {
                name: WEB_SEARCH.to_owned(),
                reason: format!("failed to read the search response: {error}"),
            })?;
            if !status.is_success() {
                return Err(ToolError::ExecutionFailed {
                    name: WEB_SEARCH.to_owned(),
                    reason: format!(
                        "search was rejected with status {status}: {}",
                        truncate(&body, 500)
                    ),
                });
            }
            Ok(Value::String(render_results(query, &body)).into())
        })
    }
}

/// 把响应正文渲染为观测文本。
pub fn render_results(query: &str, body: &str) -> String {
    let trimmed = body.trim();
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return format!("Search results for \"{query}\":\n{}", truncate(trimmed, MAX_RESPONSE));
    };

    let entries = value
        .get("results")
        .or_else(|| value.get("data"))
        .or_else(|| value.get("items"))
        .and_then(Value::as_array);
    if let Some(entries) = entries {
        if entries.is_empty() {
            return format!("Search results for \"{query}\": no results");
        }
        let mut lines = vec![format!("Search results for \"{query}\":")];
        for (index, entry) in entries.iter().enumerate() {
            let title = entry
                .get("title")
                .or_else(|| entry.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("(untitled)");
            let url = entry
                .get("url")
                .or_else(|| entry.get("link"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let snippet = entry
                .get("snippet")
                .or_else(|| entry.get("summary"))
                .or_else(|| entry.get("content"))
                .or_else(|| entry.get("description"))
                .or_else(|| entry.get("text"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            lines.push(format!("{}. {title}", index + 1));
            if !url.is_empty() {
                lines.push(format!("   {url}"));
            }
            if !snippet.is_empty() {
                lines.push(format!("   {}", truncate(snippet.trim(), MAX_SNIPPET)));
            }
        }
        return truncate(&lines.join("\n"), MAX_RESPONSE);
    }

    // 只给出整段答案的接口。
    for key in ["answer", "result", "text", "content"] {
        if let Some(answer) = value.get(key).and_then(Value::as_str) {
            return format!("Search results for \"{query}\":\n{}", truncate(answer, MAX_RESPONSE));
        }
    }
    format!("Search results for \"{query}\":\n{}", truncate(trimmed, MAX_RESPONSE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;

    #[test]
    fn rejects_non_http_endpoints() {
        let error = WebSearchTool::new(SearchOptions::new("ftp://example.com", "")).err();

        assert!(matches!(error, Some(ToolError::InvalidSpec { .. })));
    }

    #[test]
    fn renders_result_arrays() {
        let body = r#"{"results":[
            {"title":"Rust","url":"https://rust-lang.org","snippet":"A language."},
            {"name":"Docs","link":"https://doc.rust-lang.org","content":"Reference."}
        ]}"#;

        let text = render_results("rust", body);

        assert!(text.contains("1. Rust"));
        assert!(text.contains("https://rust-lang.org"));
        assert!(text.contains("2. Docs"));
        assert!(text.contains("Reference."));
    }

    #[test]
    fn renders_provider_summaries() {
        let body = r#"{"results":[{"title":"Rust book","url":"https://example.com","summary":"Async chapter."}]}"#;

        let text = render_results("rust", body);

        assert!(text.contains("Async chapter."));
    }

    #[test]
    fn renders_answer_shaped_responses() {
        assert!(render_results("rust", r#"{"answer":"It compiles."}"#).contains("It compiles."));
        assert!(render_results("rust", "plain text").contains("plain text"));
        assert!(render_results("rust", r#"{"results":[]}"#).contains("no results"));
    }

    #[test]
    fn request_body_carries_query_and_limit() {
        let tool = WebSearchTool::new(SearchOptions::new("https://api.test/v1/tools/search", "k"))
            .expect("tool should build");

        assert_eq!(tool.request_body("rust", 3), json!({ "query": "rust", "max_results": 3 }));
    }

    #[tokio::test]
    async fn posts_to_the_endpoint_and_parses_results() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        let address = listener.local_addr().expect("address should resolve");
        let captured = std::sync::Arc::new(Mutex::new(String::new()));

        let server = {
            let captured = captured.clone();
            std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("connection should arrive");
                let mut buffer = [0_u8; 4096];
                let read = stream.read(&mut buffer).expect("request should be readable");
                *captured.lock().expect("capture should not be poisoned") =
                    String::from_utf8_lossy(&buffer[..read]).to_string();
                let body =
                    r#"{"results":[{"title":"Hit","url":"https://example.com","snippet":"ok"}]}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).expect("response should be written");
            })
        };

        let tool = WebSearchTool::new(SearchOptions::new(
            format!("http://{address}/v1/tools/search"),
            "secret",
        ))
        .expect("tool should build");
        let output = tool
            .invoke(&Map::from_iter([("query".to_owned(), Value::String("rust".into()))]))
            .await
            .expect("search should succeed");
        server.join().expect("server thread should finish");

        let request = captured.lock().expect("capture should not be poisoned").clone();
        assert!(request.starts_with("POST /v1/tools/search"));
        assert!(request.contains("authorization: Bearer secret"));
        assert!(request.contains("\"query\":\"rust\""));
        assert!(output.as_str().expect("output should be text").contains("1. Hit"));
    }
}
