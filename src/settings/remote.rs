//! 偏好页「远程」：服务开关、监听地址、配对二维码与设备管理。
//!
//! 修改即时生效：每次提交都会写回配置并让远程服务重新同步。

use gpui_kit::assets::IconName;
use gpui_kit::base::{TestSupportExt as _, h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::Haloeth;
use crate::config::{Config, RemoteConfig, RemoteDevice};
use crate::remote::BridgeStatus;
use crate::session::Session;
use crate::ui::{Tone, cancel_button, field, input_frame, tinted};

/// 远程偏好页。
pub struct RemotePage {
    session: WeakEntity<Session>,
    haloeth: WeakEntity<Haloeth>,
    port: Entity<InputState>,
    bind: Entity<InputState>,
    name: Entity<InputState>,
    /// 刚生成的令牌；只显示一次，不落盘。
    token: Option<SharedString>,
    /// 服务状态缓存；渲染期间不读取 Haloeth，避免嵌套更新断言。
    status: (String, Tone),
    /// 自签证书指纹缓存。
    fingerprint: Option<String>,
    /// 设备最近在线缓存。
    last_seen: std::collections::HashMap<String, u64>,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl RemotePage {
    /// 构造页面。
    pub fn new(
        window: &mut Window,
        session: WeakEntity<Session>,
        haloeth: WeakEntity<Haloeth>,
        cx: &mut Context<Self>,
    ) -> Self {
        let config = session
            .upgrade()
            .map(|session| session.read(cx).config().remote.clone())
            .unwrap_or_default();
        let port = cx.new(|cx| InputState::new(window, cx).placeholder("监听端口，例如 8737"));
        port.update(cx, |state, cx| state.set_value(config.port.to_string(), window, cx));
        let bind = cx.new(|cx| InputState::new(window, cx).placeholder("绑定地址，例如 0.0.0.0"));
        bind.update(cx, |state, cx| state.set_value(config.bind.clone(), window, cx));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("设备名称，例如 iPhone"));

        let subscriptions = vec![
            cx.subscribe_in(&port, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.apply_endpoint(cx);
                }
            }),
            cx.subscribe_in(&bind, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.apply_endpoint(cx);
                }
            }),
        ];

        // 窗口首次渲染发生在打开偏好窗口的更新内，此时不能读 Haloeth；延后一帧。
        let page = cx.entity().downgrade();
        window.defer(cx, move |_, cx| {
            page.update(cx, |page, cx| page.refresh(cx)).ok();
        });

        Self {
            session,
            haloeth,
            port,
            bind,
            name,
            token: None,
            status: ("未启用".to_owned(), Tone::Neutral),
            fingerprint: None,
            last_seen: std::collections::HashMap::new(),
            error: None,
            _subscriptions: subscriptions,
        }
    }

    /// 切换到本页时从配置重建输入框。
    pub fn reload_from(&mut self, config: &Config, window: &mut Window, cx: &mut Context<Self>) {
        let remote = &config.remote;
        self.port.update(cx, |state, cx| state.set_value(remote.port.to_string(), window, cx));
        self.bind.update(cx, |state, cx| state.set_value(remote.bind.clone(), window, cx));
        self.token = None;
        self.error = None;
        self.refresh(cx);
    }

    /// 刷新服务状态与设备在线时间缓存；不在渲染期间读取 Haloeth。
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(haloeth) = self.haloeth.upgrade() else {
            return;
        };
        let scheme = if self.remote_config(cx).tls {
            "wss"
        } else {
            "ws"
        };
        self.fingerprint = haloeth.read(cx).remote_tls_fingerprint();
        self.status = match haloeth.read(cx).remote_status() {
            BridgeStatus::Running(addr) => (
                format!(
                    "监听中：{scheme}://{addr}/v1，当前 {} 个连接",
                    haloeth.read(cx).remote_connections()
                ),
                Tone::Success,
            ),
            BridgeStatus::Failed(error) => (format!("启动失败：{error}"), Tone::Danger),
            BridgeStatus::Disabled => ("未启用".to_owned(), Tone::Neutral),
        };
        let mut last_seen = std::collections::HashMap::new();
        if let Some(session) = self.session.upgrade() {
            for device in &session.read(cx).config().remote.devices {
                if let Some(seen) = haloeth.read(cx).remote_last_seen(&device.id) {
                    last_seen.insert(device.id.clone(), seen);
                }
            }
        }
        self.last_seen = last_seen;
        cx.notify();
    }

    /// 提交改动：写回配置、落盘并让远程服务重新同步。
    fn commit(&mut self, mutate: impl FnOnce(&mut RemoteConfig), cx: &mut Context<Self>) -> bool {
        let Some(session) = self.session.upgrade() else {
            return false;
        };
        let mut config = session.read(cx).config().clone();
        mutate(&mut config.remote);
        if session.read(cx).persists_config()
            && let Err(error) = config.save()
        {
            self.error = Some(format!("保存失败：{error}").into());
            cx.notify();
            return false;
        }
        session.update(cx, |session, cx| session.apply_config(config, cx));
        if let Some(haloeth) = self.haloeth.upgrade() {
            haloeth.update(cx, |haloeth, cx| haloeth.sync_remote(cx));
        }
        self.error = None;
        self.refresh(cx);
        true
    }

    /// 当前配置。
    fn remote_config(&self, cx: &App) -> RemoteConfig {
        self.session
            .upgrade()
            .map(|session| session.read(cx).config().remote.clone())
            .unwrap_or_default()
    }

    /// 校验端口与绑定地址后提交。
    fn apply_endpoint(&mut self, cx: &mut Context<Self>) {
        let Ok(port) = self.port.read(cx).value().trim().parse::<u16>() else {
            self.error = Some("端口必须是 0–65535 之间的整数（0 表示自动分配）".into());
            cx.notify();
            return;
        };
        let bind = self.bind.read(cx).value().trim().to_owned();
        if bind.parse::<std::net::IpAddr>().is_err() {
            self.error = Some("绑定地址必须是 IP，例如 0.0.0.0 或 127.0.0.1".into());
            cx.notify();
            return;
        }
        self.commit(
            |remote| {
                remote.port = port;
                remote.bind = bind;
            },
            cx,
        );
    }

    /// 启用或停用远程服务。
    fn toggle(&mut self, cx: &mut Context<Self>) {
        if self.remote_config(cx).enabled {
            self.commit(|remote| remote.enabled = false, cx);
            return;
        }
        let Ok(port) = self.port.read(cx).value().trim().parse::<u16>() else {
            self.error = Some("端口必须是 0–65535 之间的整数（0 表示自动分配）".into());
            cx.notify();
            return;
        };
        let bind = self.bind.read(cx).value().trim().to_owned();
        if bind.parse::<std::net::IpAddr>().is_err() {
            self.error = Some("绑定地址必须是 IP，例如 0.0.0.0 或 127.0.0.1".into());
            cx.notify();
            return;
        }
        self.commit(
            |remote| {
                remote.port = port;
                remote.bind = bind;
                remote.enabled = true;
            },
            cx,
        );
    }

    /// 生成一台新设备的令牌并加入配对列表。
    fn add_device(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let token = remote::auth::new_token();
        let name = self.name.read(cx).value().trim().to_owned();
        let device = RemoteDevice {
            id: uuid::Uuid::new_v4().to_string(),
            name: if name.is_empty() {
                "新设备".to_owned()
            } else {
                name
            },
            token_hash: remote::auth::hash_token(&token),
            added_at: crate::remote::now_millis(),
            last_seen: 0,
        };
        if !self.commit(|remote| remote.devices.push(device), cx) {
            return;
        }
        self.token = Some(token.into());
        self.name.update(cx, |state, cx| state.set_value("", window, cx));
    }

    /// 移除一台设备：先确认，确认后删除。
    fn confirm_revoke(
        &mut self,
        device: RemoteDevice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let page = cx.entity().downgrade();
        let id = device.id.clone();
        let name = device.name.clone();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            alert
                .border_2()
                .title(format!("移除设备「{name}」？"))
                .description("移除后该设备的令牌立即失效，需要重新配对。")
                .footer(
                    DialogFooter::new()
                        .child(
                            cancel_button("cancel", "取消", cx)
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("ok").danger().label("移除").on_click({
                            let page = page.clone();
                            let id = id.clone();
                            move |_, window, cx| {
                                page.update(cx, |page, cx| page.revoke(&id, cx)).ok();
                                window.close_dialog(cx);
                            }
                        })),
                )
                .on_ok({
                    let page = page.clone();
                    let id = id.clone();
                    move |_, _, cx| {
                        page.update(cx, |page, cx| page.revoke(&id, cx)).ok();
                        true
                    }
                })
        });
    }

    /// 删除一台设备。
    fn revoke(&mut self, id: &str, cx: &mut Context<Self>) {
        self.commit(|remote| remote.devices.retain(|device| device.id != id), cx);
    }
}

impl Render for RemotePage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let remote = self.remote_config(cx);
        let enabled = remote.enabled;
        let tls = remote.tls;
        let (status, tone) = self.status.clone();
        let devices = remote.devices.clone();

        let rows: Vec<AnyElement> = devices
            .iter()
            .enumerate()
            .map(|(index, device)| {
                let last_seen = self.last_seen.get(&device.id).copied().unwrap_or(device.last_seen);
                let last_seen = if last_seen == 0 {
                    "从未连接".to_owned()
                } else {
                    format!("最近在线：{}", format_time(last_seen))
                };
                let device = device.clone();
                h_flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .rounded(cx.theme().radius)
                    .border_2()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_sm().truncate().child(device.name.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("配对于 {}", format_time(device.added_at))),
                            )
                            .child(div().text_xs().text_color(muted).child(last_seen)),
                    )
                    .child(
                        Button::new(ElementId::Name(format!("remote-revoke:{index}").into()))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Trash)
                            .tooltip("移除设备")
                            .accessibility_label("移除设备")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_revoke(device.clone(), window, cx)
                            })),
                    )
                    .into_any_element()
            })
            .collect();

        let pairing = self.token.clone().map(|token| self.pairing_string(token.as_ref(), cx));

        v_flex()
            .id("remote-panel")
            .test_support()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("远程默认关闭。启用后监听局域网，可选用自签 TLS；客户端按 docs/remote-protocol.md 实现，跨网访问建议使用隧道。"),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        tinted(
                            Button::new("remote-toggle").outline().border_2().xsmall(),
                            if enabled { Tone::Success } else { Tone::Danger },
                            cx,
                        )
                        .label(if enabled { "远程：已启用" } else { "远程：已停用" })
                        .selected(enabled)
                        .toggled(enabled)
                        .on_click(cx.listener(|this, _, _, cx| this.toggle(cx))),
                    )
                    .child(
                        tinted(
                            Button::new("remote-tls").outline().border_2().xsmall(),
                            if tls { Tone::Success } else { Tone::Warning },
                            cx,
                        )
                        .label(if tls { "传输：自签 TLS" } else { "传输：明文" })
                        .selected(tls)
                        .toggled(tls)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.commit(|remote| remote.tls = !remote.tls, cx);
                        })),
                    )
                    .child(
                        div()
                            .id("remote-status")
                            .role(Role::Status)
                            .test_support()
                            .aria_label(status.clone())
                            .text_xs()
                            .text_color(match tone {
                                Tone::Success => cx.theme().success,
                                Tone::Danger => cx.theme().danger,
                                _ => muted,
                            })
                            .child(status),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(field(
                        "监听端口",
                        input_frame(&self.port, "remote-port", true, cx),
                        cx,
                    ))
                    .child(field(
                        "绑定地址",
                        input_frame(&self.bind, "remote-bind", true, cx),
                        cx,
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("端口 0 表示由系统分配；首次启用时 Windows 会请求防火墙权限。改动即时生效，服务会自动重启。启用 TLS 后配对串包含证书指纹 fp，客户端据此钉扎；明文模式仅建议在受信网络使用。"),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_end()
                    .child(div().flex_1().min_w_0().child(field(
                        "新设备名称",
                        input_frame(&self.name, "remote-device-name", true, cx),
                        cx,
                    )))
                    .child(
                        Button::new("remote-device-add")
                            .primary()
                            .xsmall()
                            .icon(IconName::Plus)
                            .label("生成配对")
                            .on_click(cx.listener(|this, _, window, cx| this.add_device(window, cx))),
                    ),
            )
            .when_some(pairing, |this, pairing| {
                let qr = render_qr(&pairing, cx);
                this.child(
                    h_flex()
                        .items_center()
                        .gap_3()
                        .p_2()
                        .border_2()
                        .border_color(cx.theme().border)
                        .when_some(qr, |this, qr| this.child(qr))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("扫描二维码完成配对"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(muted)
                                        .child("令牌只显示这一次；桌面端只保存摘要，泄露的影响限于该设备。"),
                                )
                                .child(
                                    div()
                                        .id("remote-pairing")
                                        .role(Role::Status)
                                        .test_support()
                                        .aria_label(pairing.clone())
                                        .text_xs()
                                        .text_color(muted)
                                        .child(pairing),
                                ),
                        ),
                )
            })
            .child(
                v_flex()
                    .gap_2()
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("已配对设备"))
                    .when(devices.is_empty(), |this| {
                        this.child(
                            div()
                                .id("remote-empty")
                                .role(Role::Status)
                                .test_support()
                                .aria_label("尚未配对任何设备")
                                .text_sm()
                                .text_color(muted)
                                .child("尚未配对任何设备。"),
                        )
                    })
                    .children(rows),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .id("remote-error")
                        .role(Role::Status)
                        .test_support()
                        .aria_label(error.clone())
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
    }
}

impl RemotePage {
    /// 二维码内容：自定义 scheme，原生客户端按协议文档解析。
    ///
    /// TLS 启用且证书就绪时追加证书指纹 `fp`，客户端据此钉扎。
    fn pairing_string(&self, token: &str, cx: &App) -> String {
        let port = self.port.read(cx).value().trim().to_owned();
        let host = crate::remote::local_address().unwrap_or_else(|| "127.0.0.1".to_owned());
        match &self.fingerprint {
            Some(fingerprint) => {
                format!("haloeth://pair?host={host}&port={port}&token={token}&fp={fingerprint}")
            }
            None => format!("haloeth://pair?host={host}&port={port}&token={token}"),
        }
    }
}

/// 渲染二维码为矩形点阵。
fn render_qr(content: &str, cx: &App) -> Option<AnyElement> {
    let code = qrcode::QrCode::new(content.as_bytes()).ok()?;
    let width = code.width();
    let colors = code.to_colors();
    let cell = px(4.);
    let mut grid = v_flex().id("remote-qr").test_support().gap_0();
    for y in 0..width {
        let mut row = h_flex().gap_0();
        for x in 0..width {
            let dark = matches!(colors[y * width + x], qrcode::types::Color::Dark);
            row = row.child(div().w(cell).h(cell).bg(if dark {
                cx.theme().foreground
            } else {
                cx.theme().background
            }));
        }
        grid = grid.child(row);
    }
    Some(div().p(px(8.)).bg(cx.theme().background).child(grid).into_any_element())
}

/// 毫秒时间戳格式化；0 表示未知。
fn format_time(millis: u64) -> String {
    if millis == 0 {
        return "未知".to_owned();
    }
    let time = std::time::UNIX_EPOCH + std::time::Duration::from_millis(millis);
    chrono::DateTime::<chrono::Utc>::from(time)
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}
