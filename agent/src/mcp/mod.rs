//! **MCP**（Model Context Protocol）接入。
//!
//! 协议与传输由官方 [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk) 实现，
//! 这里只负责：服务器配置、连接生命周期、工具命名与 [`Tool`] 适配。
//!
//! 只支持 tools：`resources` / `prompts` / `OAuth` 不在当前范围。连接失败只让该服务器的
//! 工具不可用，不阻塞整个运行。

use std::collections::{BTreeMap, HashMap};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use rmcp::model::{CallToolRequestParams, ContentBlock, Tool as McpToolInfo};
use rmcp::service::RunningService;
use rmcp::transport::TokioChildProcess;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use rmcp::{RoleClient, ServiceExt};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::Mutex;

use crate::error::ToolError;
use crate::text::truncate;
use crate::tool::{
    ApprovalRequirement, InputType, Tool, ToolEffect, ToolFuture, ToolSpec, Validation,
};

/// 连接超时。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// 单次调用超时。
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// 工具输出保留的最大字符数。
const MAX_OUTPUT: usize = 100_000;

/// MCP 服务器配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// 服务器名称；在配置内唯一，用作工具命名空间。
    pub name: String,
    /// 是否启用。
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    /// 传输方式。
    #[serde(flatten)]
    pub transport: McpTransport,
}

/// 服务器传输方式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum McpTransport {
    /// 本地子进程（stdio）。
    Stdio {
        /// 可执行文件。
        command: String,
        /// 参数。
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        /// 环境变量。
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
    },
    /// Streamable HTTP。
    Http {
        /// 服务地址。
        url: String,
        /// 附加请求头。
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
    },
}

/// 序列化时的默认值：服务器默认启用。
fn enabled_by_default() -> bool {
    true
}

/// 一次连接尝试的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpStatus {
    /// 服务器名称。
    pub name: String,
    /// 是否连接成功。
    pub connected: bool,
    /// 可用工具数量。
    pub tool_count: usize,
    /// 失败原因。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl McpStatus {
    /// 界面展示文本。
    pub fn label(&self) -> String {
        match (&self.error, self.connected) {
            (Some(error), _) => format!("连接失败：{error}"),
            (None, true) => format!("已连接，{} 个工具", self.tool_count),
            (None, false) => "未连接".to_owned(),
        }
    }
}

/// 一个已连接的服务器。
struct Connection {
    #[allow(dead_code)]
    service: Arc<Mutex<RunningService<RoleClient, ()>>>,
    tools: Vec<McpToolInfo>,
}

/// 已连接的 MCP 工具集合。
pub struct McpTools {
    /// 可直接加入 [`crate::tool::ToolSet`] 的工具。
    pub tools: Vec<Box<dyn Tool>>,
    /// 每个服务器的连接结果。
    pub statuses: Vec<McpStatus>,
}

/// 连接全部启用的服务器并收集工具。
pub async fn connect(servers: &[McpServerConfig]) -> McpTools {
    let mut tools: Vec<Box<dyn Tool>> = Vec::new();
    let mut statuses = Vec::new();
    for server in servers.iter().filter(|server| server.enabled) {
        match connect_one(server).await {
            Ok(connection) => {
                let count = connection.tools.len();
                let service = connection.service.clone();
                for info in &connection.tools {
                    tools.push(Box::new(McpTool::new(&server.name, info, service.clone())));
                }
                statuses.push(McpStatus {
                    name: server.name.clone(),
                    connected: true,
                    tool_count: count,
                    error: None,
                });
            }
            Err(error) => statuses.push(McpStatus {
                name: server.name.clone(),
                connected: false,
                tool_count: 0,
                error: Some(error),
            }),
        }
    }
    McpTools {
        tools,
        statuses,
    }
}

/// 连接单个服务器并列出工具。
async fn connect_one(server: &McpServerConfig) -> Result<Connection, String> {
    let transport = match &server.transport {
        McpTransport::Stdio {
            command,
            args,
            env,
        } => {
            let process = stdio_command(command, args, env)?;
            let (transport, stderr) = TokioChildProcess::builder(process)
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|error| format!("无法启动子进程：{error}"))?;
            if let Some(stderr) = stderr {
                spawn_stderr_logger(server.name.clone(), stderr);
            }
            EitherTransport::Stdio(transport)
        }
        McpTransport::Http {
            url,
            headers,
        } => {
            let mut custom = HashMap::new();
            for (name, value) in headers {
                let name = http::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|error| format!("请求头名称不合法：{error}"))?;
                let value = http::HeaderValue::from_str(value)
                    .map_err(|error| format!("请求头取值不合法：{error}"))?;
                custom.insert(name, value);
            }
            let config =
                StreamableHttpClientTransportConfig::with_uri(url.clone()).custom_headers(custom);
            EitherTransport::Http(StreamableHttpClientTransport::from_config(config))
        }
    };

    let service = match transport {
        EitherTransport::Stdio(transport) => handshake(transport).await,
        EitherTransport::Http(transport) => handshake(transport).await,
    }?;

    let tools = service.list_all_tools().await.map_err(|error| format!("列出工具失败：{error}"))?;
    Ok(Connection {
        service: Arc::new(Mutex::new(service)),
        tools,
    })
}

/// 执行 MCP 握手；两种传输共享同一返回类型。
async fn handshake<T, E, A>(transport: T) -> Result<RunningService<RoleClient, ()>, String>
where
    T: rmcp::transport::IntoTransport<RoleClient, E, A> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    tokio::time::timeout(CONNECT_TIMEOUT, ().serve(transport))
        .await
        .map_err(|_| "连接超时".to_owned())?
        .map_err(|error| format!("握手失败：{error}"))
}

/// 把配置里的命令翻译成平台可执行的进程。
fn stdio_command(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
) -> Result<tokio::process::Command, String> {
    if command.trim().is_empty() {
        return Err("命令为空".to_owned());
    }
    let mut process = if cfg!(windows) {
        // Windows 上 `npx` 等工具是 `.cmd` 包装，需要经过命令解释器。
        let mut process = tokio::process::Command::new("cmd");
        process.arg("/C").arg(command);
        process
    } else {
        tokio::process::Command::new(command)
    };
    process.args(args);
    for (name, value) in env {
        process.env(name, value);
    }
    Ok(process)
}

/// 持续排空子进程的 stderr，避免管道写满阻塞服务器。
fn spawn_stderr_logger(name: String, mut stderr: tokio::process::ChildStderr) {
    use tokio::io::AsyncBufReadExt as _;
    tokio::spawn(async move {
        let reader = tokio::io::BufReader::new(&mut stderr);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!("[{name}] {line}");
        }
    });
}

/// 两类传输的统一占位，避免在握手时重复泛型代码。
enum EitherTransport {
    Stdio(TokioChildProcess),
    Http(StreamableHttpClientTransport<reqwest::Client>),
}

/// 一个远程 MCP 工具。
pub struct McpTool {
    spec: ToolSpec,
    remote_name: String,
    service: Arc<Mutex<RunningService<RoleClient, ()>>>,
}

impl McpTool {
    /// 由服务器返回的工具信息构造。
    fn new(
        server: &str,
        info: &McpToolInfo,
        service: Arc<Mutex<RunningService<RoleClient, ()>>>,
    ) -> Self {
        let local = mcp_tool_name(server, &info.name);
        let description = info
            .description
            .as_deref()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("MCP tool '{}' from server '{server}'", info.name));
        let input_schema = Value::Object(info.input_schema.as_ref().clone());
        let mut spec = ToolSpec::new(local, description, InputType::Any)
            .raw_parameters(input_schema)
            .validation(Validation::None)
            .approval(ApprovalRequirement::Policy)
            .effect(ToolEffect::Unknown);
        if let Some(output) = &info.output_schema {
            spec = spec.output_schema(Value::Object(output.as_ref().clone()));
        }
        Self {
            spec,
            remote_name: info.name.to_string(),
            service,
        }
    }
}

impl Tool for McpTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke<'a>(&'a self, arguments: &'a Map<String, Value>) -> ToolFuture<'a> {
        Box::pin(async move {
            let params = CallToolRequestParams::new(self.remote_name.clone())
                .with_arguments(arguments.clone());
            let call = async {
                let service = self.service.lock().await;
                service.call_tool(params).await
            };
            let result = tokio::time::timeout(CALL_TIMEOUT, call)
                .await
                .map_err(|_| ToolError::ExecutionFailed {
                    name: self.spec.name.clone(),
                    reason: "the MCP server did not answer in time".to_owned(),
                })?
                .map_err(|error| ToolError::ExecutionFailed {
                    name: self.spec.name.clone(),
                    reason: error.to_string(),
                })?;

            let text = render_content(&result.content);
            if result.is_error == Some(true) {
                return Err(ToolError::ExecutionFailed {
                    name: self.spec.name.clone(),
                    reason: truncate(&text, MAX_OUTPUT),
                });
            }
            Ok(Value::String(truncate(&text, MAX_OUTPUT)))
        })
    }
}

/// 把 MCP 内容块渲染为观测文本。
fn render_content(content: &[ContentBlock]) -> String {
    let mut lines = Vec::new();
    for block in content {
        match block {
            ContentBlock::Text(text) => lines.push(text.text.clone()),
            other => match serde_json::to_string(other) {
                Ok(text) => lines.push(text),
                Err(_) => lines.push(format!("{other:?}")),
            },
        }
    }
    if lines.is_empty() {
        "the MCP server returned no content".to_owned()
    } else {
        lines.join("\n")
    }
}

/// 生成带服务器命名空间的本地工具名。
///
/// 结果满足 [`is_valid_name`](crate::tool::ToolSpec::validate) 的约束：首字符为字母或下划线，
/// 只含字母、数字、下划线与连字符，长度不超过 64。超长时保留前缀并追加短哈希。
pub fn mcp_tool_name(server: &str, tool: &str) -> String {
    let server = sanitize(server);
    let tool = sanitize(tool);
    let full = format!("mcp__{server}__{tool}");
    if full.chars().count() <= 64 {
        return full;
    }
    let digest = short_hash(&full);
    let keep = 64 - digest.len() - 1;
    let head: String = full.chars().take(keep).collect();
    format!("{head}_{digest}")
}

/// 把名称中不合法的字符替换为下划线，并保证首字符合法。
fn sanitize(name: &str) -> String {
    let mut output = String::with_capacity(name.len());
    for (index, character) in name.chars().enumerate() {
        let valid = character.is_alphanumeric() || character == '_' || character == '-';
        let first = index == 0;
        if valid && !(first && character.is_numeric()) {
            output.push(character);
        } else if first && character.is_numeric() {
            output.push('_');
            output.push(character);
        } else {
            output.push('_');
        }
    }
    if output.is_empty() {
        "_".to_owned()
    } else {
        output
    }
}

/// 短哈希：取自 **FNV-1a** 的 32 位结果，足够区分同名前缀。
fn short_hash(text: &str) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in text.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_namespaced_and_sanitised() {
        assert_eq!(mcp_tool_name("files", "read_file"), "mcp__files__read_file");
        assert_eq!(mcp_tool_name("my server", "read file"), "mcp__my_server__read_file");
        assert_eq!(mcp_tool_name("2fast", "9lives"), "mcp___2fast___9lives");
    }

    #[test]
    fn long_tool_names_are_truncated_with_a_hash() {
        let name = mcp_tool_name(&"s".repeat(40), &"t".repeat(40));

        assert!(name.chars().count() <= 64, "{name}");
        assert!(name.starts_with("mcp__"));
        assert!(ToolSpec::new(&name, "Tool.", InputType::Any).validate().is_ok());
        assert_ne!(name, mcp_tool_name(&"s".repeat(40), &"u".repeat(40)));
    }

    #[test]
    fn content_blocks_render_as_text() {
        let content = vec![ContentBlock::text("hello"), ContentBlock::text("world")];

        assert_eq!(render_content(&content), "hello\nworld");
    }

    #[test]
    fn status_labels_cover_connected_and_failed() {
        let connected = McpStatus {
            name: "a".into(),
            connected: true,
            tool_count: 2,
            error: None,
        };
        let failed = McpStatus {
            name: "b".into(),
            connected: false,
            tool_count: 0,
            error: Some("离线".into()),
        };

        assert_eq!(connected.label(), "已连接，2 个工具");
        assert_eq!(failed.label(), "连接失败：离线");
    }

    /// 一个最小 MCP 服务器：提供一个回显工具。
    struct EchoServer;

    impl rmcp::ServerHandler for EchoServer {
        fn get_info(&self) -> rmcp::model::ServerConfig {
            rmcp::model::ServerConfig::new(
                rmcp::model::ServerCapabilities::builder().enable_tools().build(),
            )
        }

        async fn list_tools(
            &self,
            _request: Option<rmcp::model::PaginatedRequestParams>,
            _context: rmcp::service::RequestContext<rmcp::RoleServer>,
        ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
            let schema = serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            });
            let schema = schema.as_object().expect("schema should be an object").clone();
            let tool = rmcp::model::Tool::new("echo", "Echoes the given text.", Arc::new(schema));
            Ok(rmcp::model::ListToolsResult::with_all_items(vec![tool]))
        }

        async fn call_tool(
            &self,
            request: rmcp::model::CallToolRequestParams,
            _context: rmcp::service::RequestContext<rmcp::RoleServer>,
        ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
            let text = request
                .arguments
                .as_ref()
                .and_then(|arguments| arguments.get("text"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            Ok(rmcp::model::CallToolResponse::Complete(rmcp::model::CallToolResult::success(vec![
                ContentBlock::text(format!("echo: {text}")),
            ])))
        }
    }

    #[tokio::test]
    async fn tools_are_discovered_and_called_over_a_duplex_pair() {
        use rmcp::RoleServer;
        use rmcp::transport::async_rw::AsyncRwTransport;

        let (client_end, server_end) = tokio::io::duplex(64 * 1024);
        let (client_read, client_write) = tokio::io::split(client_end);
        let (server_read, server_write) = tokio::io::split(server_end);

        let server = tokio::spawn(async move {
            let transport = AsyncRwTransport::<RoleServer, _, _>::new(server_read, server_write);
            rmcp::serve_server(EchoServer, transport).await
        });

        let transport = AsyncRwTransport::<RoleClient, _, _>::new_client(client_read, client_write);
        let service = ().serve(transport).await.expect("client should connect");
        let tools = service.list_all_tools().await.expect("tools should list");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");

        let service = Arc::new(Mutex::new(service));
        let tool = McpTool::new("echo server", &tools[0], service);
        assert_eq!(tool.spec().name, "mcp__echo_server__echo");
        assert_eq!(tool.spec().validation, Validation::None);

        let output = tool
            .invoke(&Map::from_iter([("text".to_owned(), Value::String("hi".into()))]))
            .await
            .expect("call should succeed");
        assert_eq!(output.as_str(), Some("echo: hi"));

        server.abort();
    }

    #[test]
    fn configs_round_trip_through_json() {
        let config = McpServerConfig {
            name: "files".into(),
            enabled: true,
            transport: McpTransport::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), "server-filesystem".into()],
                env: BTreeMap::new(),
            },
        };

        let json = serde_json::to_string(&config).expect("config should serialize");
        let restored: McpServerConfig =
            serde_json::from_str(&json).expect("config should deserialize");

        assert_eq!(restored, config);
        assert!(json.contains("\"transport\":\"stdio\""));
    }
}
