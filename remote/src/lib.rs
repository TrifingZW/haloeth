//! Haloeth 远程接入：线协议、WebSocket 服务与参考客户端。
//!
//! 本 crate 不依赖 GPUI 与代理内核：协议类型可被第三方客户端实现者直接引用，
//! 宿主能力经 [`RemoteHost`] 注入，服务端只负责鉴权、请求分发与通知扇出。

pub mod auth;
pub mod client;
pub mod host;
pub mod proto;
pub mod server;
pub mod tls;

mod conn;

pub use host::{DeviceId, Published, RemoteHost};
pub use proto::{
    BROADCAST_CAPACITY, Command, ErrorCode, Notification, PROTOCOL_VERSION, RemoteError,
};
pub use server::{
    AssetWhitelist, RemoteServer, ServerError, ServerHandle, ServerOptions, TlsMaterial,
};
