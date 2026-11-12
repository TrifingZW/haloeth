//! 内置工具：网页访问与当前时间。
//!
//! 名称沿用 smolagents 的 `default_tools`；当前包含两个工具，后续按同一契约扩充。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::error::ToolError;
use crate::text::truncate;
use crate::tool::{
    ApprovalRequirement, InputSpec, InputType, Tool, ToolEffect, ToolFuture, ToolSet, ToolSpec,
};

/// 网页访问工具的名称。
pub const FETCH_URL: &str = "fetch_url";

/// 当前时间工具的名称。
pub const CURRENT_TIME: &str = "current_time";

/// 网页正文保留的最大字符数。
const MAX_PAGE_LENGTH: usize = 20_000;

/// 响应体允许读取的最大字节数。
const MAX_BODY_BYTES: usize = 2_000_000;

/// 请求超时。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// 构造内置工具集。
pub fn default_tools() -> ToolSet {
    let mut tools = ToolSet::new();
    tools.insert(FetchUrlTool::new()).expect("内置工具的元数据固定合法且名称互不冲突");
    tools.insert(CurrentTimeTool::new()).expect("内置工具的元数据固定合法且名称互不冲突");
    tools
}

/// 网页访问工具：抓取页面并把 **HTML** 转为纯文本。
pub struct FetchUrlTool {
    client: reqwest::Client,
    spec: ToolSpec,
}

impl FetchUrlTool {
    /// 构造工具。
    pub fn new() -> Self {
        let spec = ToolSpec::new(
            FETCH_URL,
            "Fetches a web page over HTTP and returns its readable content as plain text. Use it to read pages the user refers to.",
            InputType::String,
        )
        .input("url", InputSpec::new(InputType::String, "Absolute URL of the page to fetch."))
        .effect(ToolEffect::Network {
            argument: Some("url".to_owned()),
        });
        Self {
            client: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            spec,
        }
    }
}

impl Default for FetchUrlTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for FetchUrlTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke<'a>(&'a self, arguments: &'a Map<String, Value>) -> ToolFuture<'a> {
        Box::pin(async move {
            let url = arguments.get("url").and_then(Value::as_str).unwrap_or_default();
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(failure(format!("{url} is not an http(s) URL")));
            }
            let response = self
                .client
                .get(url)
                .send()
                .await
                .map_err(|error| failure(format!("request to {url} failed: {error}")))?;

            let status = response.status();
            if !status.is_success() {
                return Err(failure(format!("request to {url} was rejected with status {status}")));
            }
            if response.content_length().is_some_and(|length| length > MAX_BODY_BYTES as u64) {
                return Err(failure(format!(
                    "{url} is larger than the {MAX_BODY_BYTES} byte limit"
                )));
            }
            let is_html = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.contains("html"));
            let body = response
                .text()
                .await
                .map_err(|error| failure(format!("failed to read {url}: {error}")))?;

            let content = if is_html {
                html_to_text(&body)
            } else {
                body
            };
            Ok(Value::String(truncate(content.trim(), MAX_PAGE_LENGTH)))
        })
    }
}

/// 当前时间工具：返回 **UTC** 日期时间。
pub struct CurrentTimeTool {
    spec: ToolSpec,
}

impl CurrentTimeTool {
    /// 构造工具。
    pub fn new() -> Self {
        Self {
            spec: ToolSpec::new(
                CURRENT_TIME,
                "Returns the current date and time in UTC.",
                InputType::String,
            )
            .approval(ApprovalRequirement::Never),
        }
    }
}

impl Default for CurrentTimeTool {
    fn default() -> Self {
        Self::new()
    }
}

impl Tool for CurrentTimeTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke<'a>(&'a self, _arguments: &'a Map<String, Value>) -> ToolFuture<'a> {
        Box::pin(async move { Ok(Value::String(render_utc(SystemTime::now()))) })
    }
}

/// 构造网页访问工具的执行失败错误。
fn failure(reason: String) -> ToolError {
    ToolError::ExecutionFailed {
        name: FETCH_URL.to_owned(),
        reason,
    }
}

/// 渲染系统时间为 `YYYY-MM-DD HH:MM:SS UTC`。
fn render_utc(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default();
    let days = seconds.div_euclid(86_400);
    let remainder = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (remainder / 3_600, remainder % 3_600 / 60, remainder % 60);

    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} UTC")
}

/// 把 Unix 纪元以来的天数转换为公历日期，算法取自 Howard Hinnant 的 `civil_from_days`。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u32;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u32;

    (
        if month <= 2 {
            year + 1
        } else {
            year
        },
        month,
        day,
    )
}

/// 把 **HTML** 转为纯文本：先移除脚本与样式，再由 **html5ever** 解析并取正文，最后压缩空白。
fn html_to_text(html: &str) -> String {
    let without_scripts = strip_blocks(html, "script");
    let without_styles = strip_blocks(&without_scripts, "style");
    let document = scraper::Html::parse_document(&without_styles);

    for selector in ["body", "html"] {
        let Ok(selector) = scraper::Selector::parse(selector) else {
            continue;
        };
        let Some(element) = document.select(&selector).next() else {
            continue;
        };
        return element
            .text()
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
    }
    without_styles.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 移除成对标签之间的内容，标签名按 **ASCII** 大小写不敏感匹配。
fn strip_blocks(html: &str, tag: &str) -> String {
    let opening = format!("<{tag}");
    let closing = format!("</{tag}>");
    let mut text = String::with_capacity(html.len());
    let mut rest = html;

    while let Some(start) = find_ignore_case(rest, &opening) {
        text.push_str(&rest[..start]);
        let block = &rest[start..];
        match find_ignore_case(block, &closing) {
            Some(end) => rest = &block[end + closing.len()..],
            None => return text,
        }
    }
    text.push_str(rest);
    text
}

/// **ASCII** 大小写不敏感的子串查找，返回字节下标。
fn find_ignore_case(haystack: &str, needle: &str) -> Option<usize> {
    let length = needle.len();
    haystack.char_indices().find_map(|(index, _)| {
        haystack.get(index..index + length)?.eq_ignore_ascii_case(needle).then_some(index)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_expose_their_metadata() {
        let tools = default_tools();

        let names: Vec<&str> = tools.iter().map(|tool| tool.spec().name.as_str()).collect();
        assert_eq!(names, [CURRENT_TIME, FETCH_URL]);
        assert_eq!(
            tools.get(FETCH_URL).expect("tool should exist").spec().inputs["url"].ty,
            InputType::String
        );
        assert!(tools.get(CURRENT_TIME).expect("tool should exist").spec().inputs.is_empty());
    }

    #[test]
    fn renders_known_timestamps() {
        assert_eq!(render_utc(UNIX_EPOCH), "1970-01-01 00:00:00 UTC");
        assert_eq!(
            render_utc(UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000)),
            "2001-09-09 01:46:40 UTC"
        );
        assert_eq!(
            render_utc(UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000)),
            "2023-11-14 22:13:20 UTC"
        );
    }

    #[test]
    fn extracts_text_from_html() {
        let html = "<html><head><style>p { color: red }</style><script>alert('x')</script></head>\
                    <body><h1>标题</h1><p>正文 &amp; 说明</p></body></html>";

        assert_eq!(html_to_text(html), "标题 正文 & 说明");
    }

    #[test]
    fn strips_blocks_case_insensitively() {
        assert_eq!(strip_blocks("a<SCRIPT>hidden</Script>b", "script"), "ab");
    }

    #[test]
    fn keeps_text_after_an_unclosed_block() {
        assert_eq!(strip_blocks("a<script>hidden", "script"), "a");
    }

    #[test]
    fn finds_substrings_ignoring_ascii_case() {
        assert_eq!(find_ignore_case("Hello World", "world"), Some(6));
        assert_eq!(find_ignore_case("Hello", "xyz"), None);
    }

    #[tokio::test]
    async fn current_time_tool_returns_a_timestamp() {
        let tools = default_tools();

        let output =
            tools.call(CURRENT_TIME, &serde_json::json!({})).await.expect("call should succeed");

        assert!(output.as_str().is_some_and(|text| text.ends_with("UTC")));
    }
}
