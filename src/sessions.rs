//! 会话对话框：列出、打开、新建、重命名与删除会话。
//!
//! 所有动作都经由 [`Haloeth`] 执行，保证「先保存当前会话、再切换」的顺序。

use gpui_kit::assets::IconName;
use gpui_kit::base::{TestSupportExt as _, h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::Haloeth;
use crate::session_store::{SessionStore, SessionSummary};

/// 会话对话框：列表与当前会话的标题编辑。
pub struct SessionsForm {
    store: SessionStore,
    entries: Vec<SessionSummary>,
    active: String,
    rename: Entity<InputState>,
    error: Option<SharedString>,
    haloeth: WeakEntity<Haloeth>,
}

impl SessionsForm {
    /// 构造对话框内容。
    pub fn new(
        window: &mut Window,
        store: SessionStore,
        active: &str,
        haloeth: WeakEntity<Haloeth>,
        cx: &mut Context<Self>,
    ) -> Self {
        let rename = cx.new(|cx| InputState::new(window, cx).placeholder("重命名当前会话"));
        Self {
            entries: store.list(),
            store,
            active: active.to_owned(),
            rename,
            error: None,
            haloeth,
        }
    }

    /// 重新读取会话列表。
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(haloeth) = self.haloeth.upgrade() {
            self.active = haloeth.read(cx).active_id().to_owned();
        }
        self.entries = self.store.list();
        cx.notify();
    }

    /// 打开会话。
    fn open(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(haloeth) = self.haloeth.upgrade() else {
            return;
        };
        haloeth.update(cx, |haloeth, cx| haloeth.open_session(id, cx));
        if self.haloeth.upgrade().is_some_and(|haloeth| haloeth.read(cx).active_id() != id) {
            self.error = Some("运行中不能切换会话，请先停止。".into());
        } else {
            self.error = None;
        }
        self.refresh(cx);
    }

    /// 新建会话。
    fn create(&mut self, cx: &mut Context<Self>) {
        let Some(haloeth) = self.haloeth.upgrade() else {
            return;
        };
        let before = haloeth.read(cx).active_id().to_owned();
        haloeth.update(cx, |haloeth, cx| haloeth.create_session(cx));
        if haloeth.read(cx).active_id() == before {
            self.error = Some("运行中不能新建会话，请先停止。".into());
        } else {
            self.error = None;
        }
        self.refresh(cx);
    }

    /// 删除会话；删除当前会话后由外壳切换到下一个或新建。
    fn delete(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(haloeth) = self.haloeth.upgrade() else {
            return;
        };
        let result = haloeth.update(cx, |haloeth, cx| haloeth.delete_session(id, cx));
        match result {
            Ok(()) => {
                self.error = None;
                self.refresh(cx);
            }
            Err(reason) => {
                self.error = Some(reason.into());
                cx.notify();
            }
        }
    }

    /// 重命名当前会话。
    fn rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.rename.read(cx).value().trim().to_owned();
        if title.is_empty() {
            self.error = Some("请输入会话标题".into());
            cx.notify();
            return;
        }
        let Some(haloeth) = self.haloeth.upgrade() else {
            return;
        };
        match haloeth.update(cx, |haloeth, cx| haloeth.rename_session(&title, cx)) {
            Ok(()) => {
                self.error = None;
                self.rename.update(cx, |state, cx| state.set_value("", window, cx));
                self.refresh(cx);
            }
            Err(reason) => {
                self.error = Some(reason.into());
                cx.notify();
            }
        }
    }

    /// 面板底部：错误提示、关闭与新建。
    pub fn render_footer(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let form = cx.entity();

        DialogFooter::new()
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .id("sessions-error")
                        .role(Role::Status)
                        .test_support()
                        .aria_label(error.clone())
                        .flex_1()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .child(
                crate::ui::cancel_button("sessions-close", "关闭", cx)
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
            .child(Button::new("sessions-new").primary().label("新建会话").on_click({
                let form = form.clone();
                move |_, _, cx| form.update(cx, |form, cx| form.create(cx))
            }))
    }
}

impl Render for SessionsForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let entries = self.entries.clone();
        let active = self.active.clone();

        let rows: Vec<AnyElement> = entries
            .iter()
            .map(|entry| {
                let is_active = entry.id == active;
                let title = entry.title.clone();
                let when = format_time(entry.updated_at);
                let count = format!("{} 条消息", entry.message_count);
                let id = entry.id.clone();
                let delete_id = entry.id.clone();
                let open_id = entry.id.clone();
                let title_for_label = title.clone();

                h_flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .rounded(cx.theme().radius)
                    .border_2()
                    .border_color(if is_active {
                        cx.theme().primary
                    } else {
                        cx.theme().border
                    })
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(if is_active {
                                        FontWeight::SEMIBOLD
                                    } else {
                                        FontWeight::NORMAL
                                    })
                                    .truncate()
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .child(format!("{when} · {count}")),
                            ),
                    )
                    .when(!is_active, |this| {
                        this.child(
                            Button::new(ElementId::Name(format!("session-open:{id}").into()))
                                .outline()
                                .border_2()
                                .xsmall()
                                .label("打开")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open(&open_id, cx);
                                })),
                        )
                    })
                    .child(
                        Button::new(ElementId::Name(format!("session-delete:{delete_id}").into()))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Trash)
                            .tooltip("删除会话")
                            .accessibility_label(format!("删除会话「{title_for_label}」"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.delete(&delete_id, cx);
                            })),
                    )
                    .into_any_element()
            })
            .collect();

        v_flex()
            .id("sessions-panel")
            .test_support()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("会话自动保存在本机。运行中不能切换会话，请先停止。"),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(Input::new(&self.rename).cleanable(true))
                    .child(
                        Button::new("sessions-rename")
                            .outline()
                            .border_2()
                            .icon(IconName::Check)
                            .label("重命名")
                            .on_click(cx.listener(|this, _, window, cx| this.rename(window, cx))),
                    ),
            )
            .when(rows.is_empty(), |this| {
                this.child(
                    div()
                        .id("sessions-empty")
                        .role(Role::Status)
                        .test_support()
                        .aria_label("还没有会话")
                        .text_sm()
                        .text_color(muted)
                        .child("还没有会话。点击「新建会话」开始。"),
                )
            })
            .children(rows)
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(Icon::new(IconName::FolderOpen).xsmall().text_color(muted))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .truncate()
                            .child(self.store.root().display().to_string()),
                    ),
            )
    }
}

/// 把 Unix 毫秒格式化为本地日期时间。
fn format_time(millis: u64) -> String {
    chrono::DateTime::from_timestamp_millis(millis as i64)
        .map(|time| time.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "未知时间".to_owned())
}
