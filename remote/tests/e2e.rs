//! 回环连接测试：鉴权、命令分发、通知扇出、背压与停机。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use remote::proto::dto::{
    AccessDto, AppStateDto, ApprovalModeDto, ModelRefDto, ServerInfoDto, SessionStatusDto,
    SnapshotDto, UsageDto,
};
use remote::proto::{Command, Notification, RemoteError};
use remote::{
    AssetWhitelist, DeviceId, Published, RemoteHost, RemoteServer, ServerHandle, ServerOptions,
};

/// 内存宿主：记录命令并按固定形状返回握手与快照。
struct TestHost {
    events: broadcast::Sender<Published>,
    commands: Mutex<Vec<Command>>,
}

impl TestHost {
    fn new(capacity: usize) -> Arc<Self> {
        let (events, _) = broadcast::channel(capacity);
        Arc::new(Self {
            events,
            commands: Mutex::new(Vec::new()),
        })
    }

    fn publish(&self, seq: u64, notification: Notification) {
        let _ = self.events.send(Published {
            seq,
            notification,
        });
    }

    fn commands(&self) -> Vec<Command> {
        self.commands.lock().expect("command log").clone()
    }
}

#[async_trait::async_trait]
impl RemoteHost for TestHost {
    fn authorize(&self, token: &str) -> Option<DeviceId> {
        (token == "good-token").then(|| "device-1".to_owned())
    }

    fn subscribe(&self) -> broadcast::Receiver<Published> {
        self.events.subscribe()
    }

    async fn dispatch(&self, command: Command) -> Result<Value, RemoteError> {
        match command {
            Command::Welcome => Ok(serde_json::to_value(remote::proto::dto::WelcomeDto {
                protocol: 1,
                server: ServerInfoDto {
                    name: "haloeth".into(),
                    version: "0.2.0".into(),
                    platform: "test".into(),
                },
                state: sample_state(),
                sessions: Vec::new(),
            })
            .expect("welcome")),
            Command::Snapshot => Ok(serde_json::to_value(SnapshotDto {
                session_id: "s1".into(),
                entries: Vec::new(),
                streaming: None,
                watermark: 7,
            })
            .expect("snapshot")),
            Command::Ping => Ok(json!({ "t": 1 })),
            other => {
                self.commands.lock().expect("command log").push(other.clone());
                Ok(Value::Null)
            }
        }
    }
}

/// 启动一个仅回环可达的服务。
fn start(capacity: usize) -> (Arc<TestHost>, ServerHandle, Arc<AssetWhitelist>) {
    let host = TestHost::new(capacity);
    let assets = Arc::new(AssetWhitelist::new(8));
    let handle = RemoteServer::start(
        ServerOptions {
            bind: "127.0.0.1".into(),
            port: 0,
            ..Default::default()
        },
        host.clone(),
        assets.clone(),
    )
    .expect("服务应当启动");
    (host, handle, assets)
}

fn url(handle: &ServerHandle) -> String {
    remote::client::url(&handle.addr().to_string(), "/v1")
}

#[tokio::test]
async fn handshake_streams_notifications_with_sequence_numbers() {
    let (host, handle, _assets) = start(64);
    let mut client =
        remote::client::Client::connect(&url(&handle), "good-token").await.expect("握手应当成功");
    assert_eq!(client.welcome()["protocol"], json!(1));

    let (seq, notification) =
        client.next_notification().await.expect("应当读到快照").expect("连接保持打开");
    assert_eq!(seq, 7, "连接后的快照使用计数器高水位");
    assert!(matches!(notification, Notification::TranscriptReset(_)));

    host.publish(
        8,
        Notification::MessageDelta(remote::proto::notify::MessageDelta {
            text: "正在".into(),
        }),
    );
    let (seq, notification) =
        client.next_notification().await.expect("应当读到增量").expect("连接保持打开");
    assert_eq!(seq, 8);
    assert!(matches!(notification, Notification::MessageDelta(_)));

    let reply = client.request("ping", json!({})).await.expect("保活");
    assert_eq!(reply, json!({ "t": 1 }));
}

#[tokio::test]
async fn commands_reach_the_host_and_errors_stay_structured() {
    let (host, handle, _assets) = start(64);
    let mut client =
        remote::client::Client::connect(&url(&handle), "good-token").await.expect("握手应当成功");

    client.request("message.send", json!({ "text": "整理目录" })).await.expect("发送应当成功");
    assert!(
        host.commands().iter().any(
            |command| matches!(command, Command::MessageSend { text, .. } if text == "整理目录")
        )
    );

    client
        .request("grant.add", json!({ "path": "D:/work", "access": "write" }))
        .await
        .expect("授权应当成功");
    assert!(host.commands().iter().any(|command| matches!(
        command,
        Command::GrantAdd {
            access: AccessDto::Write,
            ..
        }
    )));

    let error = client.request("session.resume", json!({})).await.expect_err("预留操作");
    assert_eq!(error.code, remote::ErrorCode::UnknownOp);

    let error = client.request("message.send", json!({ "text": " " })).await.expect_err("空文本");
    assert_eq!(error.code, remote::ErrorCode::InvalidParams);
}

#[tokio::test]
async fn invalid_tokens_are_rejected() {
    let (_host, handle, _assets) = start(64);
    let result = remote::client::Client::connect(&url(&handle), "wrong-token").await;
    assert!(result.is_err(), "错误令牌不应完成握手");
}

#[tokio::test]
async fn authorization_headers_also_authenticate() {
    let (_host, handle, _assets) = start(64);
    let mut request = url(&handle).into_client_request().expect("请求可用");
    request.headers_mut().insert("authorization", "Bearer good-token".parse().expect("请求头"));
    let (mut socket, _) =
        tokio_tungstenite::connect_async(request).await.expect("带请求头的连接应当成功");
    socket
        .send(Message::Text(
            json!({ "v": 1, "id": 1, "op": "hello", "params": {} }).to_string().into(),
        ))
        .await
        .expect("发送 hello");

    let mut welcome = None;
    while let Some(Ok(message)) = socket.next().await {
        if let Message::Text(text) = message {
            let value: Value = serde_json::from_str(text.as_str()).expect("JSON");
            if value.get("ok").and_then(Value::as_bool) == Some(true) {
                welcome = Some(value);
                break;
            }
        }
    }
    assert!(welcome.is_some(), "请求头令牌应当通过鉴权");
}

#[tokio::test]
async fn foreign_origins_are_rejected() {
    let (_host, handle, _assets) = start(64);
    let mut request = url(&handle).into_client_request().expect("请求可用");
    request.headers_mut().insert("origin", "http://evil.example".parse().expect("来源"));
    let result = tokio_tungstenite::connect_async(request).await;
    assert!(result.is_err(), "跨站来源不应完成升级");
}

#[tokio::test]
async fn version_mismatches_close_the_connection() {
    let (_host, handle, _assets) = start(64);
    let (mut socket, _) =
        tokio_tungstenite::connect_async(url(&handle)).await.expect("连接应当建立");
    socket
        .send(Message::Text(
            json!({ "v": 2, "id": 1, "op": "hello", "params": { "token": "good-token" } })
                .to_string()
                .into(),
        ))
        .await
        .expect("发送 hello");

    let mut closed = false;
    while let Some(Ok(message)) = socket.next().await {
        if let Message::Close(frame) = message {
            closed = true;
            assert_eq!(u16::from(frame.expect("关闭帧").code), 4409);
            break;
        }
    }
    assert!(closed, "版本不匹配需要关闭连接");
}

#[tokio::test]
async fn asset_endpoint_requires_a_token_and_a_published_path() {
    let (_host, handle, assets) = start(64);
    let directory =
        std::env::temp_dir().join(format!("haloeth-remote-assets-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).expect("临时目录");
    let image = directory.join("shot.png");
    std::fs::write(&image, [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]).expect("写入图片");
    let text = directory.join("notes.txt");
    std::fs::write(&text, b"hello").expect("写入文本");
    assets.insert(image.clone());
    assets.insert(text.clone());

    let base = format!("http://{}", handle.addr());
    let encoded = encode_path(&image);
    let ok = reqwest::get(format!("{base}/v1/assets?path={encoded}&token=good-token"))
        .await
        .expect("请求应当完成");
    assert_eq!(ok.status(), reqwest::StatusCode::OK);
    assert_eq!(
        ok.headers().get(reqwest::header::CONTENT_TYPE).and_then(|value| value.to_str().ok()),
        Some("image/png")
    );

    let denied = reqwest::get(format!("{base}/v1/assets?path={encoded}&token=wrong"))
        .await
        .expect("请求应当完成");
    assert_eq!(denied.status(), reqwest::StatusCode::UNAUTHORIZED);

    let hidden = reqwest::get(format!(
        "{base}/v1/assets?path={}&token=good-token",
        encode_path(&directory.join("secret.png"))
    ))
    .await
    .expect("请求应当完成");
    assert_eq!(hidden.status(), reqwest::StatusCode::NOT_FOUND);

    let unsupported =
        reqwest::get(format!("{base}/v1/assets?path={}&token=good-token", encode_path(&text)))
            .await
            .expect("请求应当完成");
    assert_eq!(unsupported.status(), reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE);

    let _ = std::fs::remove_dir_all(&directory);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_clients_are_disconnected_after_lagging() {
    let (host, handle, _assets) = start(4);
    let mut client =
        remote::client::Client::connect(&url(&handle), "good-token").await.expect("握手应当成功");

    // 客户端不读取：广播容量只有 4，写入 24 MiB 必然落后。
    let payload = "x".repeat(512 * 1024);
    for seq in 0..48 {
        host.publish(
            seq,
            Notification::MessageDelta(remote::proto::notify::MessageDelta {
                text: payload.clone(),
            }),
        );
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("慢客户端没有被断开");
        }
        match tokio::time::timeout(remaining, client.next_notification()).await {
            Ok(Ok(Some(_))) => continue,
            Ok(Ok(None)) | Ok(Err(_)) => break,
            Err(_) => panic!("慢客户端没有被断开"),
        }
    }
}

#[tokio::test]
async fn stopping_the_server_closes_connections() {
    let (_host, handle, _assets) = start(64);
    let mut client =
        remote::client::Client::connect(&url(&handle), "good-token").await.expect("握手应当成功");
    handle.stop();

    let ended = tokio::time::timeout(Duration::from_secs(10), async {
        while let Ok(Some(_)) = client.next_notification().await {}
    })
    .await;
    assert!(ended.is_ok(), "停机后连接应被关闭");
}

/// 最少的百分号编码，避免为测试引入额外依赖。
fn encode_path(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

fn sample_state() -> AppStateDto {
    AppStateDto {
        active_session_id: "s1".into(),
        title: "会话".into(),
        status: SessionStatusDto::Idle,
        activity: None,
        selection: Some(ModelRefDto {
            provider: "p".into(),
            model: "m".into(),
        }),
        models: Vec::new(),
        features: Vec::new(),
        grants: Vec::new(),
        approval_mode: ApprovalModeDto::Rules,
        usage: UsageDto::default(),
    }
}

#[tokio::test]
async fn oversized_frames_are_rejected_with_the_documented_close_code() {
    let (_host, handle, _assets) = start(64);
    let (mut socket, _) =
        tokio_tungstenite::connect_async(url(&handle)).await.expect("连接应当建立");
    let padding = "x".repeat(1_100_000);
    let hello = format!(
        r#"{{"v":1,"id":1,"op":"hello","params":{{"token":"good-token","pad":"{padding}"}}}}"#
    );
    socket.send(Message::Text(hello.into())).await.expect("发送超限帧");

    let mut closed = false;
    while let Some(Ok(message)) = socket.next().await {
        if let Message::Close(frame) = message {
            closed = true;
            assert_eq!(u16::from(frame.expect("关闭帧").code), 4413);
            break;
        }
    }
    assert!(closed, "超限帧需要关闭连接");
}
