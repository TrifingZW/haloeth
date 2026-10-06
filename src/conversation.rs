use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::base::{TestSupportExt as _, v_flex};
use gpui_kit::component::bubble::{Bubble, BubbleVariant};
use gpui_kit::component::empty::{
    EmptyDescription, EmptyHeader, EmptyMedia, EmptyMediaVariant, EmptyTitle,
};
use gpui_kit::component::message::{Message, MessageAlignment, MessageContent};
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// 对话中的发言方
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    /// 用户
    User,
    /// 助手；由代理层写入
    #[cfg_attr(not(test), allow(dead_code))]
    Assistant,
}

impl ChatRole {
    /// 展示用名称
    fn display_name(self) -> &'static str {
        match self {
            Self::User => "你",
            Self::Assistant => "Haloeth",
        }
    }
}

/// 一条对话消息
#[derive(Clone)]
pub struct ChatMessage {
    id: u64,
    role: ChatRole,
    text: SharedString,
}

/// 对话区：消息记录的滚动、虚拟化与渲染
pub struct Conversation {
    messages: Rc<Vec<ChatMessage>>,
    scroller: Entity<MessageScrollerState>,
    next_id: u64,
}

impl Conversation {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            messages: Rc::new(Vec::new()),
            scroller: cx.new(|cx| MessageScrollerState::new(0, cx)),
            next_id: 0,
        }
    }

    /// 当前消息数量
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// 追加一条消息，跟随底部时列表自动滚动到最新内容
    pub fn push(&mut self, role: ChatRole, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let message = ChatMessage {
            id: self.next_id,
            role,
            text: text.into(),
        };
        self.next_id += 1;

        Rc::make_mut(&mut self.messages).push(message);
        self.scroller.update(cx, |state, cx| {
            state.append(1, cx);
        });
        cx.notify();
    }

    /// 空记录时的占位内容
    fn render_empty_state(&self) -> impl IntoElement {
        v_flex()
            .id("conversation-empty")
            .test_support()
            .size_full()
            .items_center()
            .justify_center()
            .p_6()
            .text_center()
            .child(
                EmptyHeader::new()
                    .media(
                        EmptyMedia::new()
                            .with_variant(EmptyMediaVariant::Icon)
                            .child(Icon::new(IconName::MessageSquare)),
                    )
                    .title(EmptyTitle::new().child("开始一段对话"))
                    .description(
                        EmptyDescription::new()
                            .child("在下方输入你的任务，Haloeth 会与你一起完成它。"),
                    ),
            )
    }
}

/// 发言方对应的展示方式：对齐方向与气泡变体
fn presentation(role: ChatRole) -> (MessageAlignment, BubbleVariant) {
    match role {
        ChatRole::User => (MessageAlignment::End, BubbleVariant::Secondary),
        ChatRole::Assistant => (MessageAlignment::Start, BubbleVariant::Ghost),
    }
}

/// 渲染单条消息行
fn render_message_row(message: &ChatMessage) -> impl IntoElement {
    let (alignment, variant) = presentation(message.role);

    div()
        .id(("message", message.id))
        .role(Role::ListItem)
        .test_support()
        .aria_label(SharedString::from(format!(
            "{}：{}",
            message.role.display_name(),
            message.text
        )))
        .child(
            Message::new().alignment(alignment).content(
                MessageContent::new()
                    .bubble(Bubble::new().with_variant(variant).child(message.text.clone())),
            ),
        )
}

impl Render for Conversation {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_empty = self.messages.is_empty();
        let messages = self.messages.clone();

        v_flex()
            .id("conversation")
            .test_support()
            .w_full()
            .flex_1()
            .min_h_0()
            .when(is_empty, |this| this.child(self.render_empty_state()))
            .when(!is_empty, |this| {
                this.child(
                    MessageScroller::new(
                        "conversation-scroller",
                        self.scroller.clone(),
                        move |ix, _window, _cx| match messages.get(ix) {
                            Some(message) => render_message_row(message).into_any_element(),
                            None => div().into_any_element(),
                        },
                    )
                    .with_bottom_fade(cx.theme().background),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{ChatRole, presentation};
    use gpui_kit::component::bubble::BubbleVariant;
    use gpui_kit::component::message::MessageAlignment;

    #[test]
    fn presentation_maps_role_to_alignment_and_bubble_variant() {
        assert_eq!(presentation(ChatRole::User), (MessageAlignment::End, BubbleVariant::Secondary));
        assert_eq!(
            presentation(ChatRole::Assistant),
            (MessageAlignment::Start, BubbleVariant::Ghost)
        );
    }
}
