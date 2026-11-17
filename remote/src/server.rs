//! WebSocket 服务：监听、路由、资源端点与生命周期。

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Multipart, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::json;

use crate::conn;
use crate::host::RemoteHost;
use crate::proto::{
    ASSETS_PATH, ATTACHMENT_LIMIT_BYTES, ATTACHMENTS_PATH, AUTH_TIMEOUT, Command, PING_INTERVAL,
    RemoteError, WS_PATH,
};

/// TLS 材料（证书与私钥 PEM）。
#[derive(Clone)]
pub struct TlsMaterial {
    /// 证书 PEM。
    pub cert_pem: String,
    /// 私钥 PEM。
    pub key_pem: String,
}

impl std::fmt::Debug for TlsMaterial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("TlsMaterial(<redacted>)")
    }
}

/// 服务端参数。
#[derive(Debug, Clone)]
pub struct ServerOptions {
    /// 绑定地址，例如 `0.0.0.0` 或 `127.0.0.1`。
    pub bind: String,
    /// 监听端口；`0` 表示由系统分配。
    pub port: u16,
    /// 额外允许的浏览器来源；同源请求始终放行。
    pub allowed_origins: Vec<String>,
    /// 鉴权窗口。
    pub auth_timeout: Duration,
    /// 协议层 ping 周期。
    pub ping_interval: Duration,
    /// 每连接出站队列容量（帧）。
    pub queue_capacity: usize,
    /// 单窗口内允许的鉴权失败次数。
    pub auth_failures: u32,
    /// 鉴权失败计数窗口。
    pub auth_window: Duration,
    /// 启用自签 TLS；`None` 为明文 `ws://`。
    pub tls: Option<TlsMaterial>,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0".to_owned(),
            port: 8737,
            allowed_origins: Vec::new(),
            auth_timeout: AUTH_TIMEOUT,
            ping_interval: PING_INTERVAL,
            queue_capacity: 1024,
            auth_failures: 5,
            auth_window: Duration::from_secs(60),
            tls: None,
        }
    }
}

/// 启动错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerError {
    /// 地址非法或端口被占用。
    Bind(String),
    /// 运行时创建失败。
    Runtime(String),
    /// 线程创建失败。
    Spawn(String),
    /// TLS 配置无效。
    Tls(String),
    /// 监听就绪超时。
    Timeout,
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bind(message) => write!(formatter, "监听失败：{message}"),
            Self::Runtime(message) => write!(formatter, "运行时创建失败：{message}"),
            Self::Spawn(message) => write!(formatter, "线程创建失败：{message}"),
            Self::Tls(message) => write!(formatter, "TLS 配置无效：{message}"),
            Self::Timeout => write!(formatter, "监听就绪超时"),
        }
    }
}

impl std::error::Error for ServerError {}

/// 服务端内部共享状态。
pub(crate) struct ServerState {
    /// 宿主。
    pub(crate) host: Arc<dyn RemoteHost>,
    /// 已公布的图片路径。
    pub(crate) assets: Arc<AssetWhitelist>,
    /// 已上传附件的标识到路径映射。
    pub(crate) attachments: AttachmentRegistry,
    /// 参数。
    pub(crate) options: ServerOptions,
    /// 鉴权失败限速。
    pub(crate) limiter: AuthLimiter,
    /// 停机信号。
    pub(crate) shutdown: tokio::sync::watch::Receiver<bool>,
    /// 当前连接数。
    pub(crate) connections: Arc<AtomicUsize>,
}

/// 已上传附件的标识到路径映射；先进先出淘汰。
pub(crate) struct AttachmentRegistry {
    capacity: usize,
    inner: Mutex<HashMap<String, PathBuf>>,
    order: Mutex<VecDeque<String>>,
}

impl AttachmentRegistry {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            inner: Mutex::new(HashMap::new()),
            order: Mutex::new(VecDeque::new()),
        }
    }

    /// 登记一个附件。
    fn insert(&self, id: String, path: PathBuf) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.insert(id.clone(), path);
        let Ok(mut order) = self.order.lock() else {
            return;
        };
        order.push_back(id);
        while order.len() > self.capacity {
            if let Some(oldest) = order.pop_front() {
                inner.remove(&oldest);
            }
        }
    }

    /// 解析附件标识为绝对路径；未知标识返回错误。
    pub(crate) fn resolve(&self, ids: &[String]) -> Result<Vec<String>, RemoteError> {
        let Ok(inner) = self.inner.lock() else {
            return Err(RemoteError::internal("附件表不可用"));
        };
        ids.iter()
            .map(|id| {
                inner
                    .get(id)
                    .map(|path| path.display().to_string())
                    .ok_or_else(|| RemoteError::invalid_params(format!("附件不存在：{id}")))
            })
            .collect()
    }
}

/// 已公布图片路径的白名单；先进先出淘汰。
pub struct AssetWhitelist {
    capacity: usize,
    inner: Mutex<WhitelistInner>,
}

#[derive(Default)]
struct WhitelistInner {
    paths: HashSet<PathBuf>,
    order: VecDeque<PathBuf>,
}

impl AssetWhitelist {
    /// 以容量构造。
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            inner: Mutex::new(WhitelistInner::default()),
        }
    }

    /// 公布一个可拉取路径。
    pub fn insert(&self, path: PathBuf) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if !inner.paths.insert(path.clone()) {
            return;
        }
        inner.order.push_back(path);
        while inner.order.len() > self.capacity {
            if let Some(oldest) = inner.order.pop_front() {
                inner.paths.remove(&oldest);
            }
        }
    }

    /// 路径是否已公布。
    pub fn contains(&self, path: &Path) -> bool {
        self.inner.lock().is_ok_and(|inner| inner.paths.contains(path))
    }
}

/// 鉴权失败限速。
pub(crate) struct AuthLimiter {
    failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    limit: u32,
    window: Duration,
}

impl AuthLimiter {
    fn new(limit: u32, window: Duration) -> Self {
        Self {
            failures: Mutex::new(HashMap::new()),
            limit,
            window,
        }
    }

    /// 该来源是否已被限速。
    pub(crate) fn blocked(&self, ip: IpAddr) -> bool {
        let Ok(failures) = self.failures.lock() else {
            return false;
        };
        failures
            .get(&ip)
            .is_some_and(|(count, since)| *count >= self.limit && since.elapsed() < self.window)
    }

    /// 记录一次失败。
    pub(crate) fn record_failure(&self, ip: IpAddr) {
        let Ok(mut failures) = self.failures.lock() else {
            return;
        };
        let entry = failures.entry(ip).or_insert((0, Instant::now()));
        if entry.1.elapsed() >= self.window {
            *entry = (0, Instant::now());
        }
        entry.0 = entry.0.saturating_add(1);
    }
}

/// 运行中的服务端句柄。
pub struct ServerHandle {
    addr: SocketAddr,
    connections: Arc<AtomicUsize>,
    shutdown: tokio::sync::watch::Sender<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ServerHandle {
    /// 实际监听地址。
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// 当前连接数。
    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::Relaxed)
    }

    /// 停止服务：通知连接关闭并等待线程退出。
    pub fn stop(mut self) {
        let _ = self.shutdown.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

/// 远程服务入口。
pub struct RemoteServer;

impl RemoteServer {
    /// 在专用线程上启动服务；返回时监听已就绪。
    pub fn start(
        options: ServerOptions,
        host: Arc<dyn RemoteHost>,
        assets: Arc<AssetWhitelist>,
    ) -> Result<ServerHandle, ServerError> {
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let connections = Arc::new(AtomicUsize::new(0));
        let thread_connections = connections.clone();
        let thread = std::thread::Builder::new()
            .name("remote-server".to_owned())
            .spawn(move || run(options, host, assets, thread_connections, shutdown_rx, ready_tx))
            .map_err(|error| ServerError::Spawn(error.to_string()))?;
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(addr)) => Ok(ServerHandle {
                addr,
                connections,
                shutdown: shutdown_tx,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = shutdown_tx.send(true);
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = shutdown_tx.send(true);
                Err(ServerError::Timeout)
            }
        }
    }
}

/// 线程主体：创建运行时、绑定监听并进入服务循环。
fn run(
    options: ServerOptions,
    host: Arc<dyn RemoteHost>,
    assets: Arc<AssetWhitelist>,
    connections: Arc<AtomicUsize>,
    shutdown: tokio::sync::watch::Receiver<bool>,
    ready: std::sync::mpsc::SyncSender<Result<SocketAddr, ServerError>>,
) {
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(ServerError::Runtime(error.to_string())));
            return;
        }
    };
    runtime.block_on(async move {
        let address =
            match format!("{}:{}", options.bind.trim(), options.port).parse::<SocketAddr>() {
                Ok(address) => address,
                Err(error) => {
                    let _ = ready.send(Err(ServerError::Bind(error.to_string())));
                    return;
                }
            };
        let tls = match &options.tls {
            Some(material) => match axum_server::tls_rustls::RustlsConfig::from_pem(
                material.cert_pem.clone().into_bytes(),
                material.key_pem.clone().into_bytes(),
            )
            .await
            {
                Ok(config) => Some(config),
                Err(error) => {
                    let _ = ready.send(Err(ServerError::Tls(error.to_string())));
                    return;
                }
            },
            None => None,
        };
        let listener = match std::net::TcpListener::bind(address) {
            Ok(listener) => listener,
            Err(error) => {
                let _ = ready.send(Err(ServerError::Bind(error.to_string())));
                return;
            }
        };
        if let Err(error) = listener.set_nonblocking(true) {
            let _ = ready.send(Err(ServerError::Bind(error.to_string())));
            return;
        }
        let local = listener.local_addr().unwrap_or(address);
        let state = Arc::new(ServerState {
            host,
            assets,
            attachments: AttachmentRegistry::new(256),
            limiter: AuthLimiter::new(options.auth_failures, options.auth_window),
            options,
            shutdown: shutdown.clone(),
            connections,
        });
        let app = Router::new()
            .route("/", get(web_index))
            .route(WS_PATH, get(upgrade))
            .route(ASSETS_PATH, get(asset))
            .route(
                ATTACHMENTS_PATH,
                post(upload).layer(DefaultBodyLimit::max(ATTACHMENT_LIMIT_BYTES + 1024 * 1024)),
            )
            .with_state(state);
        let _ = ready.send(Ok(local));

        let handle = axum_server::Handle::new();
        let mut wait = shutdown;
        let shutdown_handle = handle.clone();
        tokio::spawn(async move {
            let _ = wait.changed().await;
            shutdown_handle.graceful_shutdown(Some(Duration::from_secs(5)));
        });
        let outcome = match tls {
            Some(config) => {
                axum_server::from_tcp_rustls(listener, config)
                    .handle(handle)
                    .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                    .await
            }
            None => {
                axum_server::from_tcp(listener)
                    .handle(handle)
                    .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                    .await
            }
        };
        if let Err(error) = outcome {
            tracing::warn!("远程服务退出：{error}");
        }
    });
}

/// 参考 Web 客户端单页。
async fn web_index() -> Response {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], crate::web::INDEX_HTML).into_response()
}

/// WebSocket 升级入口。
async fn upgrade(
    State(state): State<Arc<ServerState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if state.limiter.blocked(peer.ip()) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    if !origin_allowed(&headers, &state.options.allowed_origins) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let header_token = bearer(&headers);
    ws.on_upgrade(move |socket| conn::handle(socket, state, peer, header_token))
}

/// 图片资源端点：只提供已公布的路径。
async fn asset(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<AssetQuery>,
    headers: HeaderMap,
) -> Response {
    let token = bearer(&headers).or(query.token);
    let Some(token) = token else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if state.host.authorize(&token).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let path = PathBuf::from(&query.path);
    if !state.assets.contains(&path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(bytes) = tokio::fs::read(&path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(mime) = sniff_image(&bytes) else {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    };
    ([(header::CONTENT_TYPE, mime)], bytes).into_response()
}

#[derive(Deserialize)]
struct AssetQuery {
    path: String,
    #[serde(default)]
    token: Option<String>,
}

#[derive(Deserialize)]
struct UploadQuery {
    #[serde(default)]
    token: Option<String>,
}

/// 附件上传：`multipart/form-data`，字段名 `file`。
async fn upload(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    let token = bearer(&headers).or(query.token);
    let Some(token) = token else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if state.host.authorize(&token).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let field = loop {
        match multipart.next_field().await {
            Ok(Some(field)) if field.name() == Some("file") => break field,
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        }
    };
    let bytes: Bytes = match field.bytes().await {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if bytes.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if bytes.len() > ATTACHMENT_LIMIT_BYTES {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let Some(mime) = sniff_attachment(&bytes) else {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    };
    // 目标目录由宿主提供，并已附带会话级隐式只读授权。
    let dir = match state.host.dispatch(Command::AttachmentTarget).await {
        Ok(value) => value.get("dir").and_then(serde_json::Value::as_str).map(PathBuf::from),
        Err(error) => {
            tracing::warn!("附件目录不可用：{error}");
            None
        }
    };
    let Some(dir) = dir else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    if let Err(error) = tokio::fs::create_dir_all(&dir).await {
        tracing::warn!("创建附件目录失败：{error}");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let id = uuid::Uuid::new_v4().to_string();
    let path = dir.join(format!("{id}.{}", extension_for(mime)));
    if let Err(error) = tokio::fs::write(&path, &bytes).await {
        tracing::warn!("写入附件失败：{error}");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    state.attachments.insert(id.clone(), path);
    axum::Json(json!({
        "attachment_id": id,
        "mime": mime,
        "size": bytes.len(),
    }))
    .into_response()
}

/// 探测附件类型：图片，或 UTF-8 文本；其它类型返回 [`None`]。
fn sniff_attachment(bytes: &[u8]) -> Option<&'static str> {
    if let Some(mime) = sniff_image(bytes) {
        return Some(mime);
    }
    let text = std::str::from_utf8(bytes).ok()?;
    (!text.contains('\0')).then_some("text/plain")
}

/// 附件扩展名。
fn extension_for(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        _ => "txt",
    }
}

/// 浏览器来源校验：同源放行，或命中显式允许列表。
fn origin_allowed(headers: &HeaderMap, allowed: &[String]) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|value| value.to_str().ok()) else {
        return true;
    };
    if allowed.iter().any(|entry| entry == origin) {
        return true;
    }
    let Some(host) = headers.get(header::HOST).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    origin_authority(origin).is_some_and(|authority| authority.eq_ignore_ascii_case(host))
}

/// 取来源的 `host:port` 部分。
fn origin_authority(origin: &str) -> Option<&str> {
    let rest = origin.split_once("://")?.1;
    Some(rest.split('/').next().unwrap_or(rest))
}

/// 取 `Authorization: Bearer` 令牌。
fn bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim().to_owned())
}

/// 按内容探测图片类型。
fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF8") {
        return Some("image/gif");
    }
    if bytes.len() > 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(b"BM") {
        return Some("image/bmp");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_must_match_the_host_or_the_allowlist() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "192.168.1.5:8737".parse().expect("host"));
        assert!(origin_allowed(&headers, &[]));

        headers.insert(header::ORIGIN, "http://192.168.1.5:8737".parse().expect("origin"));
        assert!(origin_allowed(&headers, &[]));

        headers.insert(header::ORIGIN, "http://evil.example".parse().expect("origin"));
        assert!(!origin_allowed(&headers, &[]));
        assert!(origin_allowed(&headers, &["http://evil.example".to_owned()]));
    }

    #[test]
    fn image_sniffing_recognises_common_formats() {
        assert_eq!(sniff_image(&[0x89, b'P', b'N', b'G', 0]), Some("image/png"));
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0]), Some("image/jpeg"));
        assert_eq!(sniff_image(b"GIF89a"), Some("image/gif"));
        assert_eq!(sniff_image(b"RIFF\x00\x00\x00\x00WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_image(b"BM...."), Some("image/bmp"));
        assert_eq!(sniff_image(b"hello"), None);
    }

    #[test]
    fn asset_whitelist_evicts_the_oldest_entry() {
        let whitelist = AssetWhitelist::new(2);
        whitelist.insert(PathBuf::from("a"));
        whitelist.insert(PathBuf::from("b"));
        whitelist.insert(PathBuf::from("c"));
        assert!(!whitelist.contains(Path::new("a")));
        assert!(whitelist.contains(Path::new("b")));
        assert!(whitelist.contains(Path::new("c")));
    }

    #[test]
    fn bearer_tokens_parse_case_insensitively() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "bearer abc".parse().expect("authorization"));
        assert_eq!(bearer(&headers).as_deref(), Some("abc"));
        headers.insert(header::AUTHORIZATION, "Basic abc".parse().expect("authorization"));
        assert_eq!(bearer(&headers), None);
    }

    #[test]
    fn auth_limiter_blocks_after_repeated_failures() {
        let limiter = AuthLimiter::new(2, Duration::from_secs(60));
        let ip: IpAddr = "127.0.0.1".parse().expect("ip");
        assert!(!limiter.blocked(ip));
        limiter.record_failure(ip);
        assert!(!limiter.blocked(ip));
        limiter.record_failure(ip);
        assert!(limiter.blocked(ip));
    }
}
