//! 协议常量与线格式入口。
//!
//! 信封、操作目录与数据类型的定义分布在子模块中；本模块只保留与传输相关的常量。

pub mod dto;
pub mod notify;
pub mod request;
pub mod response;

use std::time::Duration;

pub use dto::*;
pub use notify::{Notification, NotificationFrame, TranscriptReset};
pub use request::{ClientInfo, Command, HelloParams, RequestFrame};
pub use response::{ErrorCode, RemoteError, ResponseFrame};

/// 协议大版本；逐帧携带，`hello` 阶段校验。
pub const PROTOCOL_VERSION: u8 = 1;
/// WebSocket 升级路径。
pub const WS_PATH: &str = "/v1";
/// 图片资源路径。
pub const ASSETS_PATH: &str = "/v1/assets";
/// 单帧上限。
pub const MAX_FRAME_BYTES: usize = 1 << 20;
/// 鉴权窗口。
pub const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
/// 协议层 ping 周期。
pub const PING_INTERVAL: Duration = Duration::from_secs(20);
/// 宿主通知广播的容量；连接落后于该容量时被断开并要求重新同步。
pub const BROADCAST_CAPACITY: usize = 1024;
