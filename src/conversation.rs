use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::base::text::CodeBlock;
use gpui_kit::base::{TestSupportExt as _, v_flex};
use gpui_kit::component::bubble::{Bubble, BubbleVariant};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::empty::{
    EmptyDescription, EmptyHeader, EmptyMedia, EmptyMediaVariant, EmptyTitle,
};
use gpui_kit::component::message::{Message, MessageAlignment, MessageContent};
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
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
    /// Markdown 原文：无障碍名称的来源，只由 [`Conversation::push`] 与 [`Conversation::stream`] 写入
    source: SharedString,
    /// 富文本状态：流式追加写入它，渲染读取它
    content: Entity<TextViewState>,
}

/// 对话区：消息记录的滚动、虚拟化与渲染
pub struct Conversation {
    messages: Rc<Vec<ChatMessage>>,
    scroller: Entity<MessageScrollerState>,
    /// 与 `messages` 同序：Markdown 解析在后台完成，落地后需重新测量对应行
    parse_observations: Vec<Subscription>,
    next_id: u64,
    /// 正在流式写入的消息下标
    streaming: Option<usize>,
}

impl Conversation {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            messages: Rc::new(Vec::new()),
            scroller: cx.new(|cx| MessageScrollerState::new(0, cx)),
            parse_observations: Vec::new(),
            next_id: 0,
            streaming: None,
        }
    }

    /// 当前消息数量
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// 追加一条消息，跟随底部时列表自动滚动到最新内容
    pub fn push(&mut self, role: ChatRole, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let source: SharedString = text.into();
        let content = cx.new(|cx| TextViewState::markdown(&source, cx));
        let row = self.messages.len();
        // 解析落地后行高变化：重新测量该行，并按最新内容重绘
        let observation = cx.observe(&content, move |this, _, cx| {
            this.scroller.update(cx, |state, cx| {
                state.remeasure_items(row..row + 1, cx);
            });
            cx.notify();
        });

        Rc::make_mut(&mut self.messages).push(ChatMessage {
            id: self.next_id,
            role,
            source,
            content,
        });
        self.next_id += 1;
        self.parse_observations.push(observation);
        self.scroller.update(cx, |state, cx| {
            state.append(1, cx);
        });
        cx.notify();
    }

    /// 追加流式文本：已有进行中的消息则拼接，否则开一条新消息
    pub fn stream(&mut self, role: ChatRole, text: &str, cx: &mut Context<Self>) {
        match self.streaming {
            Some(index) => {
                let Some(message) = self.messages.get(index) else {
                    return;
                };
                let content = message.content.clone();
                content.update(cx, |state, cx| state.push_str(text, cx));

                if let Some(message) = Rc::make_mut(&mut self.messages).get_mut(index) {
                    message.source = SharedString::from(format!("{}{text}", message.source));
                }
                cx.notify();
            }
            None => {
                let index = self.messages.len();
                self.push(role, text, cx);
                self.streaming = Some(index);
            }
        }
    }

    /// 结束当前流式消息，后续流式文本开新消息
    pub fn end_stream(&mut self) {
        self.streaming = None;
    }

    /// 清空全部消息
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.messages = Rc::new(Vec::new());
        self.parse_observations.clear();
        self.streaming = None;
        self.scroller.update(cx, |state, cx| state.reset(0, cx));
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
            message.source
        )))
        .child(
            Message::new().alignment(alignment).content(
                MessageContent::new().bubble(
                    Bubble::new().with_variant(variant).child(
                        // 正文按 Markdown 渲染，流式追加逐块淡入
                        TextView::new(&message.content)
                            .stream_fade(true)
                            .code_block_actions(copy_code_block),
                    ),
                ),
            ),
        )
}

/// 代码块右上角的复制动作：写入系统剪贴板
fn copy_code_block(code_block: &CodeBlock, _window: &mut Window, _cx: &mut App) -> AnyElement {
    let code = code_block.code();

    Button::new("copy")
        .ghost()
        .xsmall()
        .icon(IconName::Copy)
        .tooltip("复制代码")
        .accessibility_label("复制代码")
        .on_click(move |_, _, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()))
        })
        .into_any_element()
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
                    .with_bottom_fade(cx.theme().background)
                    // 回到底部按钮的边框与全局一致：组件默认为 1px
                    .with_jump_button_style(StyleRefinement::default().border_2()),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{ChatRole, Conversation, presentation};
    use gpui_kit::AppContext as _;
    use gpui_kit::TestAppContext;
    use gpui_kit::component::bubble::BubbleVariant;
    use gpui_kit::component::message::MessageAlignment;

    /// 消息正文按 Markdown 渲染：语法标记从可见文本中消失
    #[gpui_kit::test]
    fn messages_render_markdown(cx: &mut TestAppContext) {
        let conversation = cx.update(|cx| cx.new(Conversation::new));

        cx.update(|cx| {
            conversation.update(cx, |conversation, cx| {
                conversation.push(
                    ChatRole::Assistant,
                    "# 标题\n\n- 一\n- 二\n\n```\nlet x = 1;\n```",
                    cx,
                );
            });
        });

        cx.update(|cx| {
            let state = conversation.read(cx).messages[0].content.clone();
            let rendered = state.read(cx).rendered_text();
            let text = rendered.as_str();

            assert!(text.contains("标题"), "标题文本可见：{text}");
            assert!(!text.contains('#'), "标题标记已解析：{text}");
            assert!(text.contains('一') && text.contains('二'), "列表项可见：{text}");
            assert!(!text.contains("```"), "代码围栏已解析：{text}");
        });
    }

    #[test]
    fn presentation_maps_role_to_alignment_and_bubble_variant() {
        assert_eq!(presentation(ChatRole::User), (MessageAlignment::End, BubbleVariant::Secondary));
        assert_eq!(
            presentation(ChatRole::Assistant),
            (MessageAlignment::Start, BubbleVariant::Ghost)
        );
    }
}
