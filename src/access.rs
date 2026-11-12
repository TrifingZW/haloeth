//! 目录授权对话框：当前会话可访问的目录，只读或读写。
//!
//! 授权属于会话而不是全局设置，因此这里直接修改会话并立即保存，没有「保存」按钮。

use agent::approval::{AllowRule, DirectoryGrant, resolve_path};
use agent::tool::Access;
use gpui_kit::assets::IconName;
use gpui_kit::base::{TestSupportExt as _, h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::Haloeth;
use crate::session::Session;
use crate::ui::{Tone, cancel_button, tinted};

/// 目录授权面板。
pub struct AccessForm {
    session: WeakEntity<Session>,
    haloeth: WeakEntity<Haloeth>,
    path: Entity<InputState>,
    write: bool,
    error: Option<SharedString>,
}

impl AccessForm {
    /// 构造面板。
    pub fn new(
        window: &mut Window,
        session: WeakEntity<Session>,
        haloeth: WeakEntity<Haloeth>,
        cx: &mut Context<Self>,
    ) -> Self {
        let path = cx.new(|cx| InputState::new(window, cx).placeholder("目录路径，例如 G:\\work"));
        Self {
            session,
            haloeth,
            path,
            write: false,
            error: None,
        }
    }

    /// 当前会话的「始终允许」规则。
    fn rules(&self, cx: &App) -> Vec<AllowRule> {
        self.session
            .upgrade()
            .map(|session| session.read(cx).always_allow().to_vec())
            .unwrap_or_default()
    }

    /// 移除一条「始终允许」规则。
    fn remove_rule(&mut self, index: usize, cx: &mut Context<Self>) {
        let mut rules = self.rules(cx);
        if index < rules.len() {
            rules.remove(index);
            if let Some(session) = self.session.upgrade() {
                session.update(cx, |session, cx| session.set_always_allow(rules, cx));
            }
            if let Some(haloeth) = self.haloeth.upgrade() {
                haloeth.update(cx, |haloeth, cx| haloeth.save_active(cx));
            }
            cx.notify();
        }
    }

    /// 当前会话的授权。
    fn grants(&self, cx: &App) -> Vec<DirectoryGrant> {
        self.session.upgrade().map(|session| session.read(cx).grants().to_vec()).unwrap_or_default()
    }

    /// 写回会话并立即保存。
    fn commit(&mut self, grants: Vec<DirectoryGrant>, cx: &mut Context<Self>) {
        if let Some(session) = self.session.upgrade() {
            session.update(cx, |session, cx| session.set_grants(grants, cx));
        }
        if let Some(haloeth) = self.haloeth.upgrade() {
            haloeth.update(cx, |haloeth, cx| haloeth.save_active(cx));
        }
        cx.notify();
    }

    /// 添加一条授权。
    fn add(&mut self, cx: &mut Context<Self>) {
        let raw = self.path.read(cx).value().trim().to_owned();
        if raw.is_empty() {
            self.error = Some("请输入目录路径".into());
            cx.notify();
            return;
        }
        let base = std::env::current_dir().unwrap_or_default();
        let path = resolve_path(&raw, &base);
        if !path.is_dir() {
            self.error = Some(format!("目录不存在：{}", path.display()).into());
            cx.notify();
            return;
        }
        let access = if self.write {
            Access::Write
        } else {
            Access::Read
        };
        let mut grants = self.grants(cx);
        if let Some(existing) = grants.iter_mut().find(|grant| grant.path == path) {
            existing.access = access;
        } else {
            grants.push(DirectoryGrant::new(path, access));
        }
        self.error = None;
        self.commit(grants, cx);
    }

    /// 移除一条授权。
    fn remove(&mut self, index: usize, cx: &mut Context<Self>) {
        let mut grants = self.grants(cx);
        if index < grants.len() {
            grants.remove(index);
            self.commit(grants, cx);
        }
    }
}

impl Render for AccessForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let grants = self.grants(cx);
        let rows: Vec<AnyElement> = grants
            .iter()
            .enumerate()
            .map(|(index, grant)| {
                let available = grant.path.is_dir();
                let access = match grant.access {
                    Access::Read => ("只读", Tone::Warning),
                    Access::Write => ("读写", Tone::Success),
                };
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
                            .child(
                                div()
                                    .text_sm()
                                    .truncate()
                                    .text_color(if available {
                                        cx.theme().foreground
                                    } else {
                                        cx.theme().danger
                                    })
                                    .child(grant.path.display().to_string()),
                            )
                            .when(!available, |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().danger)
                                        .child("目录已不存在"),
                                )
                            }),
                    )
                    .child(crate::ui::chip(
                        ElementId::Name(format!("access-kind:{index}").into()),
                        access.0,
                        access.1,
                        cx,
                    ))
                    .child(
                        Button::new(ElementId::Name(format!("access-remove:{index}").into()))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Trash)
                            .tooltip("移除授权")
                            .accessibility_label("移除授权")
                            .on_click(cx.listener(move |this, _, _, cx| this.remove(index, cx))),
                    )
                    .into_any_element()
            })
            .collect();

        let rules = self.rules(cx);
        let rule_rows: Vec<AnyElement> = rules
            .iter()
            .enumerate()
            .map(|(index, rule)| {
                let label = if rule.scope.is_empty() {
                    rule.tool.clone()
                } else {
                    format!("{} · {}", rule.tool, rule.scope)
                };
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(muted)
                            .child(label),
                    )
                    .child(
                        Button::new(ElementId::Name(format!("rule-remove:{index}").into()))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Trash)
                            .tooltip("移除规则")
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.remove_rule(index, cx)),
                            ),
                    )
                    .into_any_element()
            })
            .collect();

        v_flex()
            .id("access-panel")
            .test_support()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("没有授权目录时，文件与 Shell 操作都会走审核。授权只影响当前会话。"),
            )
            .when(rows.is_empty(), |this| {
                this.child(
                    div()
                        .id("access-empty")
                        .role(Role::Status)
                        .test_support()
                        .aria_label("尚未授权任何目录")
                        .text_sm()
                        .text_color(muted)
                        .child("尚未授权任何目录。"),
                )
            })
            .children(rows)
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(Input::new(&self.path).id("access-path").cleanable(true).flex_1())
                    .child(
                        tinted(
                            Button::new("access-mode").outline().border_2().xsmall(),
                            if self.write {
                                Tone::Success
                            } else {
                                Tone::Warning
                            },
                            cx,
                        )
                        .label(if self.write {
                            "读写"
                        } else {
                            "只读"
                        })
                        .tooltip("切换访问方式")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.write = !this.write;
                            cx.notify();
                        })),
                    )
                    .child(
                        Button::new("access-add")
                            .primary()
                            .xsmall()
                            .icon(IconName::Plus)
                            .label("添加目录")
                            .on_click(cx.listener(|this, _, _, cx| this.add(cx))),
                    ),
            )
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("始终允许"))
            .when(rule_rows.is_empty(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("还没有规则。审批时选择「始终允许」会在这里追加一条。"),
                )
            })
            .children(rule_rows)
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .id("access-error")
                        .role(Role::Status)
                        .test_support()
                        .aria_label(error.clone())
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .child(
                h_flex().justify_end().child(
                    cancel_button("access-close", "关闭", cx)
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use agent::tool::Access;

    #[test]
    fn access_modes_are_distinct() {
        assert_ne!(Access::Read, Access::Write);
    }
}
