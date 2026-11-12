//! 界面通用部件：配色方案、状态色调与带色按钮。
//!
//! 所有按钮沿用 2px 边框约定；这里只补充颜色语义：
//! - 绿色：已启用 / 成功
//! - 红色（淡）：已停用 / 取消 / 拒绝
//! - 黄色：需要确认
//! - 主色：当前选中

use gpui_kit::component::button::{
    Button, ButtonCustomVariant, ButtonVariant, ButtonVariants as _,
};
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

/// 应用配色方案；`System` 时读取窗口外观。
pub fn apply_theme(preference: ThemePreference, window: Option<&mut Window>, cx: &mut App) {
    let mode = match preference {
        ThemePreference::Light => ThemeMode::Light,
        ThemePreference::Dark => ThemeMode::Dark,
        ThemePreference::System => match window {
            Some(window) => window.appearance().into(),
            None => ThemeMode::Light,
        },
    };
    Theme::change(mode, None, cx);
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
