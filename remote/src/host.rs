//! 宿主契约与广播信箱。
//!
//! 服务端只认识协议类型；一切真实状态与动作经 [`RemoteHost`] 注入。

use serde_json::Value;
use tokio::sync::broadcast;

use crate::proto::{Command, Notification, RemoteError};

/// 设备标识。
pub type DeviceId = String;

/// 一条已分配序号的通知。
#[derive(Debug, Clone)]
pub struct Published {
    /// 通知序号；宿主发布时分配，进程生命周期内严格递增。
    pub seq: u64,
    /// 通知本体。
    pub notification: Notification,
}

/// 宿主能力的最小面，由前端实现。
///
/// `dispatch` 是唯一入口：读操作与写命令共用同一条通道，
/// 因此不存在绕开主线程串行化的第二条路径。
#[async_trait::async_trait]
pub trait RemoteHost: Send + Sync + 'static {
    /// 校验设备令牌；通过则返回设备标识。
    fn authorize(&self, token: &str) -> Option<DeviceId>;

    /// 订阅通知流；连接期间的推送都来自它。
    fn subscribe(&self) -> broadcast::Receiver<Published>;

    /// 分发一条已解析的强类型命令，返回结果或结构化错误。
    async fn dispatch(&self, command: Command) -> Result<Value, RemoteError>;
}
