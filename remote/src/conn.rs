//! 每连接状态机：鉴权、请求分发、通知扇出与背压。

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::json;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::host::Published;
use crate::proto::dto::SnapshotDto;
use crate::proto::{
    Command, HelloParams, MAX_FRAME_BYTES, NotificationFrame, PROTOCOL_VERSION, RequestFrame,
    ResponseFrame, TranscriptReset,
};
use crate::server::ServerState;

/// 信封解析失败。
const CLOSE_PROTOCOL: u16 = 4400;
/// 鉴权前发送了非 hello 帧。
const CLOSE_UNAUTHORIZED: u16 = 4401;
/// 令牌无效或已撤销。
const CLOSE_FORBIDDEN: u16 = 4403;
/// 鉴权超时。
const CLOSE_AUTH_TIMEOUT: u16 = 4408;
/// 协议版本不匹配。
const CLOSE_VERSION: u16 = 4409;
/// 单帧超过上限。
const CLOSE_TOO_LARGE: u16 = 4413;
/// 通知积压（慢客户端）。
const CLOSE_LAGGED: u16 = 4429;
/// 服务端内部错误。
const CLOSE_INTERNAL: u16 = 4500;

/// 处理一条已升级的连接。
pub(crate) async fn handle(
    socket: WebSocket,
    state: Arc<ServerState>,
    peer: SocketAddr,
    header_token: Option<String>,
) {
    let _guard = ConnectionGuard::enter(state.connections.clone());
    let (mut sink, mut stream) = socket.split();
    let hello = match authenticate(&mut stream, &state, header_token).await {
        Ok(frame) => frame,
        Err(AuthFailure::InvalidToken) => {
            state.limiter.record_failure(peer.ip());
            let _ = send_close(&mut sink, CLOSE_FORBIDDEN, "unauthorized").await;
            return;
        }
        Err(AuthFailure::Timeout) => {
            let _ = send_close(&mut sink, CLOSE_AUTH_TIMEOUT, "auth timeout").await;
            return;
        }
        Err(AuthFailure::Close(code)) => {
            let _ = send_close(&mut sink, code, "protocol error").await;
            return;
        }
        Err(AuthFailure::Closed) => return,
    };

    // 先订阅再取快照：订阅之后、快照之前的通知序号不大于水位，
    // 客户端按游标丢弃，不会重复应用。
    let events = state.host.subscribe();
    let welcome = match state.host.dispatch(Command::Welcome).await {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!("组装握手响应失败：{error}");
            let _ = send_close(&mut sink, CLOSE_INTERNAL, "internal").await;
            return;
        }
    };
    if !send_json(&mut sink, &ResponseFrame::ok(hello.id, welcome)).await {
        return;
    }
    let snapshot = match state.host.dispatch(Command::Snapshot).await {
        Ok(value) => match serde_json::from_value::<SnapshotDto>(value) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!("会话快照格式不正确：{error}");
                let _ = send_close(&mut sink, CLOSE_INTERNAL, "internal").await;
                return;
            }
        },
        Err(error) => {
            tracing::warn!("读取会话快照失败：{error}");
            let _ = send_close(&mut sink, CLOSE_INTERNAL, "internal").await;
            return;
        }
    };
    let reset = NotificationFrame::new(
        snapshot.watermark,
        &crate::proto::Notification::TranscriptReset(TranscriptReset {
            session_id: snapshot.session_id,
            entries: snapshot.entries,
            streaming: snapshot.streaming,
        }),
    );
    if !send_json(&mut sink, &reset).await {
        return;
    }

    let (out_tx, out_rx) = mpsc::channel::<Message>(state.options.queue_capacity.max(1));
    let (control_tx, control_rx) = mpsc::channel::<WriterControl>(8);
    let mut writer = tokio::spawn(write_loop(
        sink,
        out_rx,
        events,
        control_rx,
        state.clone(),
        state.shutdown.clone(),
        state.options.ping_interval,
    ));
    let mut shutdown = state.shutdown.clone();
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_ok() {
                    break;
                }
            }
            next = stream.next() => match next {
                Some(Ok(Message::Text(text))) => {
                    if text.len() > MAX_FRAME_BYTES {
                        let _ = push_close(&out_tx, CLOSE_TOO_LARGE, "frame too large").await;
                        break;
                    }
                    match handle_request(&state, text.as_str(), &control_tx, &out_tx).await {
                        Ok(()) => {}
                        Err(code) => {
                            let _ = push_close(&out_tx, code, "protocol error").await;
                            break;
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(_)) => {
                    let _ = push_close(&out_tx, CLOSE_PROTOCOL, "unexpected frame").await;
                    break;
                }
                Some(Err(_)) => break,
            },
        }
    }
    drop(out_tx);
    if tokio::time::timeout(Duration::from_secs(5), &mut writer).await.is_err() {
        writer.abort();
    }
}

/// 鉴权失败原因。
enum AuthFailure {
    /// 鉴权窗口内没有收到合法 hello。
    Timeout,
    /// 连接已关闭。
    Closed,
    /// 令牌缺失或无效。
    InvalidToken,
    /// 以指定关闭码终结。
    Close(u16),
}

/// 首帧必须是 hello；令牌可来自请求头或参数。
async fn authenticate(
    stream: &mut SplitStream<WebSocket>,
    state: &ServerState,
    header_token: Option<String>,
) -> Result<RequestFrame, AuthFailure> {
    let deadline = tokio::time::sleep(state.options.auth_timeout);
    tokio::pin!(deadline);
    loop {
        let next = tokio::select! {
            () = &mut deadline => return Err(AuthFailure::Timeout),
            next = stream.next() => next,
        };
        match next {
            Some(Ok(Message::Text(text))) => {
                if text.len() > MAX_FRAME_BYTES {
                    return Err(AuthFailure::Close(CLOSE_TOO_LARGE));
                }
                let frame: RequestFrame = serde_json::from_str(text.as_str())
                    .map_err(|_| AuthFailure::Close(CLOSE_PROTOCOL))?;
                if frame.v != PROTOCOL_VERSION {
                    return Err(AuthFailure::Close(CLOSE_VERSION));
                }
                if frame.op != "hello" {
                    return Err(AuthFailure::Close(CLOSE_UNAUTHORIZED));
                }
                let params: HelloParams = serde_json::from_value(frame.params.clone())
                    .map_err(|_| AuthFailure::Close(CLOSE_PROTOCOL))?;
                let token = header_token.or(params.token);
                let Some(token) = token else {
                    return Err(AuthFailure::InvalidToken);
                };
                return match state.host.authorize(&token) {
                    Some(_device) => Ok(frame),
                    None => Err(AuthFailure::InvalidToken),
                };
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(Message::Close(_))) | None => return Err(AuthFailure::Closed),
            Some(Ok(_)) => return Err(AuthFailure::Close(CLOSE_PROTOCOL)),
            Some(Err(_)) => return Err(AuthFailure::Closed),
        }
    }
}

/// 写任务的控制消息。
enum WriterControl {
    /// 增量续接：回放宿主返回的通知，或回退为快照。
    Replay {
        session_id: String,
        last_seq: u64,
        id: u64,
        responder: oneshot::Sender<()>,
    },
}

/// 解析并分发一条请求；响应与回放都经写任务按序写出。
///
/// `Err` 表示需要关闭连接。
async fn handle_request(
    state: &ServerState,
    text: &str,
    control: &mpsc::Sender<WriterControl>,
    out: &mpsc::Sender<Message>,
) -> Result<(), u16> {
    let frame: RequestFrame = serde_json::from_str(text).map_err(|_| CLOSE_PROTOCOL)?;
    if frame.v != PROTOCOL_VERSION {
        return Err(CLOSE_VERSION);
    }
    let id = frame.id;
    let command = match Command::parse(&frame) {
        Ok(command) => command,
        Err(error) => {
            if !send_json_queued(out, &ResponseFrame::err(id, error)).await {
                return Err(CLOSE_INTERNAL);
            }
            return Ok(());
        }
    };
    if let Command::Resume {
        session_id,
        last_seq,
    } = command
    {
        // 回放必须由写任务完成：它与实时通知共用同一条有序出站流。
        let (responder, done) = oneshot::channel();
        if control
            .send(WriterControl::Replay {
                session_id,
                last_seq,
                id,
                responder,
            })
            .await
            .is_err()
        {
            return Err(CLOSE_INTERNAL);
        }
        let _ = done.await;
        return Ok(());
    }
    // 附件标识在服务端解析为绝对路径；宿主只需处理路径。
    let command = match command {
        Command::MessageSend {
            text,
            attachments,
        } if !attachments.is_empty() => match state.attachments.resolve(&attachments) {
            Ok(paths) => Command::MessageSend {
                text,
                attachments: paths,
            },
            Err(error) => {
                if !send_json_queued(out, &ResponseFrame::err(id, error)).await {
                    return Err(CLOSE_INTERNAL);
                }
                return Ok(());
            }
        },
        other => other,
    };
    let response = match state.host.dispatch(command).await {
        Ok(result) => ResponseFrame::ok(id, result),
        Err(error) => ResponseFrame::err(id, error),
    };
    if !send_json_queued(out, &response).await {
        return Err(CLOSE_INTERNAL);
    }
    Ok(())
}

/// 写任务：控制消息、出站队列、通知扇出、保活与停机。
async fn write_loop(
    mut sink: SplitSink<WebSocket, Message>,
    mut out: mpsc::Receiver<Message>,
    mut events: broadcast::Receiver<Published>,
    mut control: mpsc::Receiver<WriterControl>,
    state: Arc<ServerState>,
    mut shutdown: watch::Receiver<bool>,
    ping_interval: Duration,
) {
    let mut ticker = tokio::time::interval(ping_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            // 控制消息优先：回放帧必须排在后续实时通知之前写出。
            biased;
            control_message = control.recv() => match control_message {
                Some(WriterControl::Replay {
                    session_id,
                    last_seq,
                    id,
                    responder,
                }) => {
                    if !write_replay(&mut sink, &state, session_id, last_seq, id).await {
                        return;
                    }
                    let _ = responder.send(());
                }
                None => return,
            },
            message = out.recv() => match message {
                Some(message) => {
                    if sink.send(message).await.is_err() {
                        return;
                    }
                }
                None => {
                    let _ = sink.send(close_frame(1000, "normal")).await;
                    return;
                }
            },
            published = events.recv() => match published {
                Ok(published) => {
                    let frame = NotificationFrame::new(published.seq, &published.notification);
                    let Ok(text) = serde_json::to_string(&frame) else {
                        return;
                    };
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let _ = sink.send(close_frame(CLOSE_LAGGED, "backpressure")).await;
                    return;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    let _ = sink.send(close_frame(1000, "server stopped")).await;
                    return;
                }
            },
            _ = ticker.tick() => {
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return;
                }
            }
            changed = shutdown.changed() => {
                if changed.is_ok() {
                    let _ = sink.send(close_frame(1000, "shutdown")).await;
                }
                return;
            }
        }
    }
}

/// 写出一次增量续接：回放通知或快照重置；返回 `false` 表示连接应关闭。
async fn write_replay(
    sink: &mut SplitSink<WebSocket, Message>,
    state: &ServerState,
    session_id: String,
    last_seq: u64,
    id: u64,
) -> bool {
    let result = state
        .host
        .dispatch(Command::Resume {
            session_id,
            last_seq,
        })
        .await;
    let value = match result {
        Ok(value) => value,
        Err(error) => return send_json(sink, &ResponseFrame::err(id, error)).await,
    };
    if let Some(frames) = value.get("notifications").and_then(|frames| frames.as_array()) {
        for frame in frames {
            let Ok(frame) = serde_json::from_value::<NotificationFrame>(frame.clone()) else {
                return false;
            };
            let Ok(text) = serde_json::to_string(&frame) else {
                return false;
            };
            if sink.send(Message::Text(text.into())).await.is_err() {
                return false;
            }
        }
    } else if let Some(snapshot) = value.get("snapshot").cloned() {
        let Ok(snapshot) = serde_json::from_value::<SnapshotDto>(snapshot) else {
            return false;
        };
        let reset = NotificationFrame::new(
            snapshot.watermark,
            &crate::proto::Notification::TranscriptReset(TranscriptReset {
                session_id: snapshot.session_id,
                entries: snapshot.entries,
                streaming: snapshot.streaming,
            }),
        );
        if !send_json(sink, &reset).await {
            return false;
        }
    }
    send_json(sink, &ResponseFrame::ok(id, json!({}))).await
}

/// 写出一个 JSON 帧。
async fn send_json(
    sink: &mut SplitSink<WebSocket, Message>,
    value: &impl serde::Serialize,
) -> bool {
    let Ok(text) = serde_json::to_string(value) else {
        return false;
    };
    sink.send(Message::Text(text.into())).await.is_ok()
}

/// 通过写任务写出一个 JSON 帧。
async fn send_json_queued(out: &mpsc::Sender<Message>, value: &impl serde::Serialize) -> bool {
    let Ok(text) = serde_json::to_string(value) else {
        return true;
    };
    out.send(Message::Text(text.into())).await.is_ok()
}

/// 直接写出一条关闭帧。
async fn send_close(sink: &mut SplitSink<WebSocket, Message>, code: u16, reason: &str) -> bool {
    sink.send(close_frame(code, reason)).await.is_ok()
}

/// 通过写任务发送关闭帧。
async fn push_close(out: &mpsc::Sender<Message>, code: u16, reason: &str) -> bool {
    out.send(close_frame(code, reason)).await.is_ok()
}

/// 构造关闭帧。
fn close_frame(code: u16, reason: &str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: reason.to_owned().into(),
    }))
}

/// 连接计数守卫。
struct ConnectionGuard(Arc<AtomicUsize>);

impl ConnectionGuard {
    fn enter(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self(counter)
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}
