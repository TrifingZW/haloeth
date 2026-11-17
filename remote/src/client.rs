//! 参考客户端：回环测试与命令行示例复用。

use std::collections::VecDeque;

use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::proto::{Notification, NotificationFrame, PROTOCOL_VERSION, RemoteError, ResponseFrame};

/// 客户端错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// 连接或握手失败。
    Connect(String),
    /// 服务端关闭了连接。
    Closed,
    /// 收到无法解析的帧。
    Protocol(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(message) => write!(formatter, "连接失败：{message}"),
            Self::Closed => write!(formatter, "连接已关闭"),
            Self::Protocol(message) => write!(formatter, "协议错误：{message}"),
        }
    }
}

impl std::error::Error for ClientError {}

/// 读到的一帧。
enum Incoming {
    Response(ResponseFrame),
    Notification(u64, Notification),
    Closed,
}

/// 已确认握手的客户端。
pub struct Client {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    pending: VecDeque<(u64, Notification)>,
    welcome: Value,
}

impl Client {
    /// 建立连接并完成 `hello`；返回时已收到 welcome 与首条快照。
    pub async fn connect(url: &str, token: &str) -> Result<Self, ClientError> {
        let (socket, _response) =
            connect_async(url).await.map_err(|error| ClientError::Connect(error.to_string()))?;
        let mut client = Self {
            socket,
            next_id: 1,
            pending: VecDeque::new(),
            welcome: Value::Null,
        };
        client.welcome = client
            .request(
                "hello",
                json!({
                    "token": token,
                    "client": {
                        "name": "haloeth-remote-client",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                }),
            )
            .await
            .map_err(|error| ClientError::Connect(error.to_string()))?;
        Ok(client)
    }

    /// 握手结果。
    pub fn welcome(&self) -> &Value {
        &self.welcome
    }

    /// 发送一条请求并等待配对响应；期间到达的通知进入缓冲。
    pub async fn request(&mut self, op: &str, params: Value) -> Result<Value, RemoteError> {
        self.next_id += 1;
        let id = self.next_id;
        let frame = json!({ "v": PROTOCOL_VERSION, "id": id, "op": op, "params": params });
        if let Err(error) = self.socket.send(Message::Text(frame.to_string().into())).await {
            return Err(RemoteError::internal(format!("发送失败：{error}")));
        }
        loop {
            match self.read_frame().await? {
                Incoming::Response(response) if response.id == id => {
                    return if response.ok {
                        Ok(response.result.unwrap_or(Value::Null))
                    } else {
                        Err(response.error.unwrap_or_else(|| RemoteError::internal("缺少错误详情")))
                    };
                }
                Incoming::Response(_) => {}
                Incoming::Notification(seq, notification) => {
                    self.pending.push_back((seq, notification));
                }
                Incoming::Closed => return Err(RemoteError::internal("连接已关闭")),
            }
        }
    }

    /// 读取下一条通知；连接关闭时返回 `None`。
    pub async fn next_notification(&mut self) -> Result<Option<(u64, Notification)>, RemoteError> {
        if let Some(item) = self.pending.pop_front() {
            return Ok(Some(item));
        }
        loop {
            match self.read_frame().await? {
                Incoming::Notification(seq, notification) => return Ok(Some((seq, notification))),
                Incoming::Response(_) => {}
                Incoming::Closed => return Ok(None),
            }
        }
    }

    /// 主动关闭连接。
    pub async fn close(mut self) {
        let _ = self.socket.close(None).await;
    }

    /// 读一帧并分类。
    async fn read_frame(&mut self) -> Result<Incoming, RemoteError> {
        loop {
            let Some(message) = self.socket.next().await else {
                return Ok(Incoming::Closed);
            };
            match message {
                Ok(Message::Text(text)) => {
                    let value: Value = serde_json::from_str(text.as_str())
                        .map_err(|error| RemoteError::internal(error.to_string()))?;
                    if value.get("id").is_some() {
                        let response: ResponseFrame = serde_json::from_value(value)
                            .map_err(|error| RemoteError::internal(error.to_string()))?;
                        return Ok(Incoming::Response(response));
                    }
                    let frame: NotificationFrame = serde_json::from_value(value)
                        .map_err(|error| RemoteError::internal(error.to_string()))?;
                    let Some((seq, notification)) = frame.parse() else {
                        return Err(RemoteError::internal("无法识别的通知"));
                    };
                    return Ok(Incoming::Notification(seq, notification));
                }
                Ok(Message::Close(_)) => return Ok(Incoming::Closed),
                Ok(Message::Ping(_) | Message::Pong(_)) => {}
                Ok(_) => {}
                Err(error) => return Err(RemoteError::internal(error.to_string())),
            }
        }
    }
}

/// 客户端连接路径：`host` 为 `ws://` 或 `wss://` 前缀后的地址。
pub fn url(address: &str, path: &str) -> String {
    format!("ws://{address}{path}")
}
