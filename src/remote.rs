//! 远程桥：宿主实现、命令通道、事件投影与配对设备管理。
//!
//! 服务线程与界面线程之间只有两条通道：命令经 `mpsc` 交给界面线程执行，
//! 通知经 `broadcast` 回到服务线程；界面线程是唯一写入者。

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use agent::approval::{ApprovalDecision, ApprovalMode, RiskLevel};
use agent::metrics::TokenUsage;
use agent::tool::{Access, ToolEffect};
use agent::{Activity as AgentActivity, Event};
use remote::proto::dto::{
    AccessDto, ActivityRecordDto, ActivityStatusDto, ApprovalDecisionDto, ApprovalModeDto,
    ChatRoleDto, ImageItemDto, RiskLevelDto, SessionStatusDto, TranscriptEntryDto, UsageDto,
    UsagePairDto,
};
use remote::proto::notify::{
    ApprovalRequested, ApprovalResolved, ImagesAppended, MessageCompleted, MessageDelta,
    PlanAppended, ReasoningDelta, Shutdown, StatusChanged, ToolFinished, ToolStarted, UsageChanged,
};
use remote::proto::{Command, Notification, RemoteError};
use remote::{DeviceId, Published, RemoteHost};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::config::{RemoteConfig, RemoteDevice};
use crate::session::Status;
use crate::ui::conversation::{ActivityStatus, ChatRole, TranscriptEntry};

/// 命令通道的请求类型。
pub(crate) type RemoteCommand = (Command, oneshot::Sender<Result<Value, RemoteError>>);

/// 远程服务的运行状态；偏好页据此展示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BridgeStatus {
    /// 未启用。
    Disabled,
    /// 监听中。
    Running(std::net::SocketAddr),
    /// 启动失败。
    Failed(String),
}

/// 状态投影快照：用于判断一次事件是否改变了状态栏。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteStatus {
    /// 运行状态。
    pub(crate) status: Status,
    /// 细粒度活动文案。
    pub(crate) activity: Option<String>,
}

/// 通知投影句柄；`Session` 持有它发布运行期通知。
#[derive(Clone)]
pub(crate) struct RemoteTap {
    shared: Arc<Shared>,
}

impl RemoteTap {
    /// 发布一条通知。
    pub(crate) fn publish(&self, notification: Notification) {
        self.shared.publish(notification);
    }

    /// 发布状态与活动文案。
    pub(crate) fn status(&self, status: SessionStatusDto, activity: Option<String>) {
        self.publish(Notification::StatusChanged(StatusChanged {
            status,
            activity,
        }));
    }

    /// 发布用量。
    pub(crate) fn usage(&self, total: TokenUsage, by_model: &BTreeMap<String, TokenUsage>) {
        self.publish(Notification::UsageChanged(usage_changed(total, by_model)));
    }
}

/// 桥与服务线程共享的状态。
struct Shared {
    commands: mpsc::UnboundedSender<RemoteCommand>,
    events: broadcast::Sender<Published>,
    seq: AtomicU64,
    assets: Arc<remote::AssetWhitelist>,
    /// 令牌摘要 → 设备标识。
    devices: Mutex<HashMap<String, String>>,
    /// 设备标识 → 最近在线（Unix 毫秒）。
    last_seen: Mutex<HashMap<String, u64>>,
}

impl Shared {
    /// 分配序号并广播一条通知。
    fn publish(&self, notification: Notification) {
        if let Notification::ImagesAppended(items) = &notification {
            for item in &items.items {
                self.assets.insert(std::path::PathBuf::from(&item.path));
            }
        }
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.events.send(Published {
            seq,
            notification,
        });
    }

    /// 当前计数器高水位。
    fn watermark(&self) -> u64 {
        self.seq.load(Ordering::Relaxed)
    }
}

#[async_trait::async_trait]
impl RemoteHost for Shared {
    fn authorize(&self, token: &str) -> Option<DeviceId> {
        let hash = remote::auth::hash_token(token);
        let id = { self.devices.lock().ok()?.get(&hash)?.clone() };
        if let Ok(mut seen) = self.last_seen.lock() {
            seen.insert(id.clone(), now_millis());
        }
        Some(id)
    }

    fn subscribe(&self) -> broadcast::Receiver<Published> {
        self.events.subscribe()
    }

    async fn dispatch(&self, command: Command) -> Result<Value, RemoteError> {
        let (sender, receiver) = oneshot::channel();
        self.commands.send((command, sender)).map_err(|_| RemoteError::internal("远程桥已停止"))?;
        receiver.await.map_err(|_| RemoteError::internal("远程桥已停止"))?
    }
}

/// 远程桥：由 [`Haloeth`](crate::app::Haloeth) 持有。
pub(crate) struct RemoteBridge {
    shared: Arc<Shared>,
    server: Mutex<Option<remote::ServerHandle>>,
    status: Mutex<BridgeStatus>,
    /// 已应用的服务端参数；用于识别端口或绑定地址变化。
    applied: Mutex<Option<(bool, String, u16)>>,
}

impl RemoteBridge {
    /// 创建桥与命令接收端；泵任务由界面层挂到执行器。
    pub(crate) fn new() -> (Self, mpsc::UnboundedReceiver<RemoteCommand>) {
        let (commands, receiver) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(remote::BROADCAST_CAPACITY);
        let shared = Arc::new(Shared {
            commands,
            events,
            seq: AtomicU64::new(0),
            assets: Arc::new(remote::AssetWhitelist::new(256)),
            devices: Mutex::new(HashMap::new()),
            last_seen: Mutex::new(HashMap::new()),
        });
        (
            Self {
                shared,
                server: Mutex::new(None),
                status: Mutex::new(BridgeStatus::Disabled),
                applied: Mutex::new(None),
            },
            receiver,
        )
    }

    /// 通知投影句柄。
    pub(crate) fn tap(&self) -> RemoteTap {
        RemoteTap {
            shared: self.shared.clone(),
        }
    }

    /// 发布一条通知。
    pub(crate) fn publish(&self, notification: Notification) {
        self.shared.publish(notification);
    }

    /// 当前通知计数器高水位。
    pub(crate) fn watermark(&self) -> u64 {
        self.shared.watermark()
    }

    /// 服务状态。
    pub(crate) fn status(&self) -> BridgeStatus {
        self.status.lock().map(|status| status.clone()).unwrap_or(BridgeStatus::Disabled)
    }

    /// 当前连接数。
    pub(crate) fn connections(&self) -> usize {
        self.server
            .lock()
            .ok()
            .and_then(|server| server.as_ref().map(remote::ServerHandle::connections))
            .unwrap_or(0)
    }

    /// 设备最近在线时间（本次进程内）。
    pub(crate) fn last_seen(&self, device_id: &str) -> Option<u64> {
        self.shared.last_seen.lock().ok().and_then(|seen| seen.get(device_id).copied())
    }

    /// 把内存中的最近在线时间合并进配置；有变化时返回 `true`。
    pub(crate) fn merge_last_seen(&self, devices: &mut [RemoteDevice]) -> bool {
        let Ok(seen) = self.shared.last_seen.lock() else {
            return false;
        };
        let mut changed = false;
        for device in devices {
            if let Some(latest) = seen.get(&device.id)
                && *latest > device.last_seen
            {
                device.last_seen = *latest;
                changed = true;
            }
        }
        changed
    }

    /// 应用配置：同步设备表，按需启停或重启服务。
    pub(crate) fn sync(&self, config: &RemoteConfig) {
        if let Ok(mut devices) = self.shared.devices.lock() {
            devices.clear();
            for device in &config.devices {
                devices.insert(device.token_hash.clone(), device.id.clone());
            }
        }

        let desired = (config.enabled, config.bind.trim().to_owned(), config.port);
        let applied = self.applied.lock().ok().and_then(|applied| applied.clone());
        if applied == Some(desired.clone()) {
            return;
        }
        self.stop_server();
        if !config.enabled {
            self.set_status(BridgeStatus::Disabled);
            if let Ok(mut applied) = self.applied.lock() {
                *applied = Some(desired);
            }
            return;
        }

        let options = remote::ServerOptions {
            bind: desired.1.clone(),
            port: desired.2,
            ..Default::default()
        };
        let host: Arc<dyn RemoteHost> = self.shared.clone();
        match remote::RemoteServer::start(options, host, self.shared.assets.clone()) {
            Ok(handle) => {
                self.set_status(BridgeStatus::Running(handle.addr()));
                if let Ok(mut server) = self.server.lock() {
                    *server = Some(handle);
                }
            }
            Err(error) => self.set_status(BridgeStatus::Failed(error.to_string())),
        }
        if let Ok(mut applied) = self.applied.lock() {
            *applied = Some(desired);
        }
    }

    /// 更新服务状态。
    fn set_status(&self, status: BridgeStatus) {
        if let Ok(mut current) = self.status.lock() {
            *current = status;
        }
    }

    /// 停止服务；`stop` 会 join 服务线程，因此放到独立线程执行。
    fn stop_server(&self) {
        let Ok(mut server) = self.server.lock() else {
            return;
        };
        if let Some(handle) = server.take() {
            // 先通知在线客户端，再关闭监听。
            self.shared.publish(Notification::Shutdown(Shutdown {
                reason: "远程服务已停止".to_owned(),
            }));
            std::thread::spawn(move || handle.stop());
        }
    }

    /// 仅供测试：订阅通知流。
    #[cfg(test)]
    pub(crate) fn subscribe(&self) -> broadcast::Receiver<Published> {
        self.shared.events.subscribe()
    }

    /// 仅供测试：直接投递一条命令，返回响应接收端。
    #[cfg(test)]
    pub(crate) fn dispatch_for_test(
        &self,
        command: Command,
    ) -> oneshot::Receiver<Result<Value, RemoteError>> {
        let (sender, receiver) = oneshot::channel();
        let _ = self.shared.commands.send((command, sender));
        receiver
    }
}

impl Drop for RemoteBridge {
    fn drop(&mut self) {
        if let Ok(mut server) = self.server.lock() {
            let _ = server.take();
        }
    }
}

/// 把一个运行事件投影为远程通知。
pub(crate) fn project_event(tap: &RemoteTap, event: &Event, step_text: Option<String>) {
    match event {
        Event::Delta(text) => tap.publish(Notification::MessageDelta(MessageDelta {
            text: text.clone(),
        })),
        Event::Step(_) => tap.publish(Notification::MessageCompleted(MessageCompleted {
            text: step_text,
        })),
        Event::Activity(activity) => match activity {
            AgentActivity::ReasoningDelta(text) => {
                tap.publish(Notification::ReasoningDelta(ReasoningDelta {
                    text: text.clone(),
                }))
            }
            AgentActivity::ToolStarted {
                call,
                ..
            } => tap.publish(Notification::ToolStarted(ToolStarted {
                call_id: call.id.clone(),
                name: call.name.clone(),
                arguments: crate::session::compact(&call.arguments),
            })),
            AgentActivity::ToolFinished {
                call_id,
                ok,
                preview,
                elapsed_ms,
                ..
            } => tap.publish(Notification::ToolFinished(ToolFinished {
                call_id: call_id.clone(),
                ok: *ok,
                preview: preview.clone(),
                elapsed_ms: *elapsed_ms,
            })),
            AgentActivity::ApprovalRequested(request) => {
                tap.publish(Notification::ApprovalRequested(ApprovalRequested {
                    id: request.id.clone(),
                    tool: request.tool.clone(),
                    summary: request.summary.clone(),
                    risk: risk_dto(request.risk),
                    manual: request.manual,
                    effects: request.effects.iter().map(effect_label).collect(),
                }))
            }
            AgentActivity::ApprovalResolved {
                id,
                decision,
                reason,
            } => tap.publish(Notification::ApprovalResolved(ApprovalResolved {
                id: id.clone(),
                decision: decision_dto(*decision),
                reason: reason.clone(),
            })),
            AgentActivity::Images(paths) => {
                tap.publish(Notification::ImagesAppended(ImagesAppended {
                    items: paths
                        .iter()
                        .map(|path| ImageItemDto {
                            path: path.clone(),
                            caption: String::new(),
                        })
                        .collect(),
                }))
            }
            AgentActivity::PlanFinished {
                plan,
                ..
            } => tap.publish(Notification::PlanAppended(PlanAppended {
                text: plan.clone(),
            })),
            AgentActivity::Thinking {
                ..
            }
            | AgentActivity::SkillLoaded {
                ..
            }
            | AgentActivity::PlanStarted {
                ..
            }
            | AgentActivity::ToolUsage {
                ..
            } => {}
        },
    }
}

/// 会话状态映射。
pub(crate) fn status_dto(status: Status) -> SessionStatusDto {
    match status {
        Status::Idle => SessionStatusDto::Idle,
        Status::Preparing => SessionStatusDto::Preparing,
        Status::Running => SessionStatusDto::Running,
        Status::AwaitingApproval => SessionStatusDto::AwaitingApproval,
        Status::Cancelling => SessionStatusDto::Cancelling,
        Status::Unconfigured => SessionStatusDto::Unconfigured,
        Status::Unselected => SessionStatusDto::Unselected,
        Status::Failed => SessionStatusDto::Failed,
    }
}

/// 目录访问级别映射。
pub(crate) fn access_dto(access: Access) -> AccessDto {
    match access {
        Access::Read => AccessDto::Read,
        Access::Write => AccessDto::Write,
    }
}

/// 目录访问级别还原。
pub(crate) fn access_from(dto: AccessDto) -> Access {
    match dto {
        AccessDto::Read => Access::Read,
        AccessDto::Write => Access::Write,
    }
}

/// 审批决定还原。
pub(crate) fn decision_from(dto: ApprovalDecisionDto) -> ApprovalDecision {
    match dto {
        ApprovalDecisionDto::AllowOnce => ApprovalDecision::AllowOnce,
        ApprovalDecisionDto::AllowAlways => ApprovalDecision::AllowAlways,
        ApprovalDecisionDto::Deny => ApprovalDecision::Deny,
    }
}

/// 审批决定映射。
pub(crate) fn decision_dto(decision: ApprovalDecision) -> ApprovalDecisionDto {
    match decision {
        ApprovalDecision::AllowOnce => ApprovalDecisionDto::AllowOnce,
        ApprovalDecision::AllowAlways => ApprovalDecisionDto::AllowAlways,
        ApprovalDecision::Deny => ApprovalDecisionDto::Deny,
    }
}

/// 审核模式映射。
pub(crate) fn mode_dto(mode: ApprovalMode) -> ApprovalModeDto {
    match mode {
        ApprovalMode::Manual => ApprovalModeDto::Manual,
        ApprovalMode::Rules => ApprovalModeDto::Rules,
        ApprovalMode::Model => ApprovalModeDto::Model,
        ApprovalMode::Off => ApprovalModeDto::Off,
    }
}

/// 审核模式还原。
pub(crate) fn mode_from(dto: ApprovalModeDto) -> ApprovalMode {
    match dto {
        ApprovalModeDto::Manual => ApprovalMode::Manual,
        ApprovalModeDto::Rules => ApprovalMode::Rules,
        ApprovalModeDto::Model => ApprovalMode::Model,
        ApprovalModeDto::Off => ApprovalMode::Off,
    }
}

/// 角色映射。
pub(crate) fn role_dto(role: ChatRole) -> ChatRoleDto {
    match role {
        ChatRole::User => ChatRoleDto::User,
        ChatRole::Assistant => ChatRoleDto::Assistant,
    }
}

/// 活动卡片状态映射。
pub(crate) fn activity_status_dto(status: ActivityStatus) -> ActivityStatusDto {
    match status {
        ActivityStatus::Running => ActivityStatusDto::Running,
        ActivityStatus::Ok => ActivityStatusDto::Ok,
        ActivityStatus::Failed => ActivityStatusDto::Failed,
        ActivityStatus::Pending => ActivityStatusDto::Pending,
        ActivityStatus::Allowed => ActivityStatusDto::Allowed,
        ActivityStatus::Denied => ActivityStatusDto::Denied,
        ActivityStatus::Interrupted => ActivityStatusDto::Interrupted,
    }
}

/// 显示记录映射。
pub(crate) fn entry_dto(entry: &TranscriptEntry) -> TranscriptEntryDto {
    match entry {
        TranscriptEntry::Text {
            role,
            text,
        } => TranscriptEntryDto::Text {
            role: role_dto(*role),
            text: text.clone(),
        },
        TranscriptEntry::Activity(record) => TranscriptEntryDto::Activity(ActivityRecordDto {
            id: record.id.clone(),
            title: record.title.clone(),
            detail: record.detail.clone(),
            preview: record.preview.clone(),
            elapsed_ms: record.elapsed_ms,
            status: activity_status_dto(record.status),
            expanded: record.expanded,
        }),
        TranscriptEntry::Image {
            path,
            caption,
        } => TranscriptEntryDto::Image {
            path: path.clone(),
            caption: caption.clone(),
        },
    }
}

/// 用量映射。
pub(crate) fn usage_dto(total: TokenUsage, by_model: &BTreeMap<String, TokenUsage>) -> UsageDto {
    UsageDto {
        total: usage_pair(total),
        by_model: by_model
            .iter()
            .map(|(model, usage)| (model.clone(), usage_pair(*usage)))
            .collect(),
    }
}

/// 用量通知体。
fn usage_changed(total: TokenUsage, by_model: &BTreeMap<String, TokenUsage>) -> UsageChanged {
    let usage = usage_dto(total, by_model);
    UsageChanged {
        total: usage.total,
        by_model: usage.by_model,
    }
}

/// 单组用量映射。
fn usage_pair(usage: TokenUsage) -> UsagePairDto {
    UsagePairDto {
        input: usage.input_tokens,
        output: usage.output_tokens,
    }
}

/// 风险等级映射。
fn risk_dto(risk: RiskLevel) -> RiskLevelDto {
    match risk {
        RiskLevel::Low => RiskLevelDto::Low,
        RiskLevel::Medium => RiskLevelDto::Medium,
        RiskLevel::High => RiskLevelDto::High,
    }
}

/// 工具影响种类的线上标签。
fn effect_label(effect: &ToolEffect) -> String {
    match effect {
        ToolEffect::Path {
            access: Access::Read,
            ..
        } => "read",
        ToolEffect::Path {
            access: Access::Write,
            ..
        } => "write",
        ToolEffect::Command {
            ..
        } => "command",
        ToolEffect::Network {
            ..
        } => "network",
        ToolEffect::Unknown => "unknown",
    }
    .to_owned()
}

/// 当前时间（Unix 毫秒）。
pub(crate) fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_default(|duration| duration.as_millis() as u64)
}
