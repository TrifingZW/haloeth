//! 界面通用部件：配色方案、状态色调与带色按钮。
//!
//! 所有按钮沿用 2px 边框约定；这里只补充颜色语义：
//! - 绿色：已启用 / 成功
//! - 红色（淡）：已停用 / 取消 / 拒绝
//! - 黄色：需要确认
//! - 主色：当前选中

use gpui_kit::base::{TestSupportExt as _, v_flex};
use gpui_kit::component::button::{
    Button, ButtonCustomVariant, ButtonVariant, ButtonVariants as _,
};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode};
use gpui_kit::*;

use crate::config::ThemePreference;

/// 语义色调。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// 中性：普通状态。
    Neutral,
    /// 成功：已启用、已完成。
    Success,
    /// 提醒：等待确认、需要处理。
    Warning,
    /// 危险：已停用、拒绝、删除。
    Danger,
    /// 强调：当前选中。
    Accent,
}

impl Tone {
    /// 对应的主题颜色。
    pub fn color(self, cx: &App) -> Hsla {
        match self {
            Self::Neutral => cx.theme().muted_foreground,
            Self::Success => cx.theme().success,
            Self::Warning => cx.theme().warning,
            Self::Danger => cx.theme().danger,
            Self::Accent => cx.theme().primary,
        }
    }
}

/// 把按钮染成低调的语义色：淡色底、同色文字、悬停加深。
pub fn tinted(button: Button, tone: Tone, cx: &App) -> Button {
    let color = tone.color(cx);
    button.with_variant(ButtonVariant::Custom(
        ButtonCustomVariant::new(cx)
            .color(color.opacity(0.12))
            .foreground(color)
            .hover(color.opacity(0.2))
            .active(color.opacity(0.28)),
    ))
}

/// 「取消 / 关闭」一类动作：淡红底与红字。
pub fn cancel_button(
    id: impl Into<gpui_kit::ElementId>,
    label: impl Into<SharedString>,
    cx: &App,
) -> Button {
    tinted(Button::new(id).outline().border_2().label(label), Tone::Danger, cx)
}

/// 状态胶囊：淡色底 + 同色文字，用于展示当前状态。
pub fn chip(
    id: impl Into<gpui_kit::ElementId>,
    label: impl Into<SharedString>,
    tone: Tone,
    cx: &App,
) -> AnyElement {
    let color = tone.color(cx);
    let label: SharedString = label.into();
    div()
        .id(id)
        .role(Role::Status)
        .test_support()
        .aria_label(label.clone())
        .px_2()
        .py(px(1.))
        .rounded(cx.theme().radius)
        .bg(color.opacity(0.12))
        .text_color(color)
        .text_xs()
        .child(label)
        .into_any_element()
}

/// 应用配色方案；`System` 时读取窗口或应用外观。
///
/// 跟随系统必须优先使用 `Window::appearance()`：设置面板里没有窗口上下文时，
/// 退回 `App::window_appearance()`，不要直接按浅色处理。
pub fn apply_theme(preference: ThemePreference, window: Option<&mut Window>, cx: &mut App) {
    match preference {
        ThemePreference::Light => Theme::change(ThemeMode::Light, window, cx),
        ThemePreference::Dark => Theme::change(ThemeMode::Dark, window, cx),
        ThemePreference::System => Theme::sync_system_appearance(window, cx),
    }
}

/// 表单字段：标签在上，控件在下。
pub fn field(label: &str, control: impl IntoElement, cx: &App) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(div().text_xs().text_color(cx.theme().muted_foreground).child(label.to_owned()))
        .child(control)
}

/// 输入控件外框：边框统一 2px，聚焦只改变边框颜色。
///
/// 组件在聚焦时会把自身边框收细为 1px 并补一圈光晕，且不可配置，
/// 因此背景与边框改由外框绘制，控件自身不绘制表面。
pub fn input_frame(
    state: &Entity<InputState>,
    id: &'static str,
    cleanable: bool,
    cx: &App,
) -> impl Styled + IntoElement {
    let focus_handle = state.read(cx).focus_handle(cx);

    div()
        .id(ElementId::Name(format!("input-frame:{id}").into()))
        .test_support()
        .bg(cx.theme().input_background())
        .border_2()
        .border_color(cx.theme().input)
        .rounded(cx.theme().radius)
        .track_focus(&focus_handle)
        .focus(|style| style.border_color(cx.theme().ring))
        // 控件默认高 2rem，外框上下各占 2px，因此控件限高 2rem − 4px，字段总高不变
        .child(Input::new(state).id(id).cleanable(cleanable).appearance(false).max_h(rems(1.75)))
}

/// 打开系统目录选择器，把选中的路径交给 `apply`。
///
/// 选择器是异步的原生对话框，期间界面仍可渲染；取消时什么也不做。
pub fn pick_folder<T, F>(window: &Window, cx: &mut Context<T>, title: &str, apply: F)
where
    T: 'static,
    F: FnOnce(&mut T, String, &mut Window, &mut Context<T>) + 'static,
{
    let title = title.to_owned();
    cx.spawn_in(window, async move |this, cx| {
        let picked = rfd::AsyncFileDialog::new().set_title(&title).pick_folder().await;
        let Some(folder) = picked else {
            return;
        };
        let path = folder.path().display().to_string();
        let _ = cx.update(|window, cx| {
            this.update(cx, |this, cx| apply(this, path, window, cx)).ok();
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::Tone;

    #[test]
    fn tones_are_ordered_for_display() {
        // 仅确认色调是纯值类型，便于在菜单与按钮间传参。
        assert_eq!(Tone::Success, Tone::Success);
        assert_ne!(Tone::Danger, Tone::Success);
    }
}
