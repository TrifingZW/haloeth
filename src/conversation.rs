//! 对话区：文本消息与运行活动卡片。
//!
//! 两条独立的行轨道：
//! - 文本消息按 Markdown 渲染，流式追加；
//! - 活动卡片是独立实体，展示工具调用、审批与计划，并承载审批按钮。
//!
//! 行的稳定标识由 [`Conversation`] 维护，活动行插入不会影响正在流式写入的文本行。

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use agent::approval::{ApprovalDecision, ApprovalRequest, RiskLevel};
use gpui_kit::assets::IconName;
use gpui_kit::base::text::CodeBlock;
use gpui_kit::base::{TestSupportExt as _, h_flex, v_flex};
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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

/// 活动卡片的运行状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityStatus {
    /// 正在执行
    Running,
    /// 成功
    Ok,
    /// 失败
    Failed,
    /// 等待用户审批
    Pending,
    /// 已允许
    Allowed,
    /// 已拒绝
    Denied,
    /// 会话恢复时仍未完成
    Interrupted,
}

impl ActivityStatus {
    /// 展示文本
    fn label(self) -> &'static str {
        match self {
            Self::Running => "运行中",
            Self::Ok => "完成",
            Self::Failed => "失败",
            Self::Pending => "等待确认",
            Self::Allowed => "已允许",
            Self::Denied => "已拒绝",
            Self::Interrupted => "已中断",
        }
    }
}

/// 活动卡片的持久化记录。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ActivityRecord {
    /// 稳定标识
    pub id: String,
    /// 标题
    pub title: String,
    /// 正文
    pub detail: String,
    /// 结果预览
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    /// 耗时
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    /// 最终状态
    pub status: ActivityStatus,
    /// 是否展开
    #[serde(default)]
    pub expanded: bool,
}

/// 对话区的一行在会话文件中的表示。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TranscriptEntry {
    /// 文本消息
    Text {
        /// 发言方
        role: ChatRole,
        /// Markdown 原文
        text: String,
    },
    /// 活动卡片
    Activity(ActivityRecord),
    /// 本地图片
    Image {
        /// 图片路径
        path: String,
        /// 说明
        caption: String,
    },
}

/// 思考过程卡片的标题；渲染时据此使用灰色斜体，与最终答复区分。
const THINKING_TITLE: &str = "思考过程";

/// 一张活动卡片：自绘实体，持有会话弱引用以回报审批决定。
pub struct ActivityCard {
    id: String,
    title: String,
    detail: String,
    preview: Option<String>,
    elapsed_ms: Option<u64>,
    status: ActivityStatus,
    expanded: bool,
    risk: Option<RiskLevel>,
    conversation: WeakEntity<Conversation>,
}

impl ActivityCard {
    /// 是否是一条思考过程（灰色斜体，与最终答复区分）。
    fn is_thinking(&self) -> bool {
        self.title == THINKING_TITLE
    }
    /// 构造卡片。
    fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        detail: impl Into<String>,
        conversation: WeakEntity<Conversation>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            detail: detail.into(),
            preview: None,
            elapsed_ms: None,
            status: ActivityStatus::Running,
            expanded: false,
            risk: None,
            conversation,
        }
    }

    /// 从持久化记录恢复；未完成的审批转为「已中断」，不再提供按钮。
    fn from_record(record: ActivityRecord, conversation: WeakEntity<Conversation>) -> Self {
        let status = if record.status == ActivityStatus::Pending {
            ActivityStatus::Interrupted
        } else {
            record.status
        };
        Self {
            id: record.id,
            title: record.title,
            detail: record.detail,
            preview: record.preview,
            elapsed_ms: record.elapsed_ms,
            status,
            expanded: record.expanded,
            risk: None,
            conversation,
        }
    }

    /// 导出持久化记录。
    fn record(&self) -> ActivityRecord {
        ActivityRecord {
            id: self.id.clone(),
            title: self.title.clone(),
            detail: self.detail.clone(),
            preview: self.preview.clone(),
            elapsed_ms: self.elapsed_ms,
            status: self.status,
            expanded: self.expanded,
        }
    }

    /// 稳定标识
    pub fn id(&self) -> &str {
        &self.id
    }

    /// 当前状态
    #[allow(dead_code)]
    pub fn status(&self) -> ActivityStatus {
        self.status
    }

    /// 状态颜色
    fn status_color(&self, cx: &App) -> Hsla {
        match self.status {
            ActivityStatus::Running | ActivityStatus::Pending => cx.theme().primary,
            ActivityStatus::Ok | ActivityStatus::Allowed => cx.theme().success,
            ActivityStatus::Failed | ActivityStatus::Denied => cx.theme().danger,
            ActivityStatus::Interrupted => cx.theme().muted_foreground,
        }
    }

    /// 切换展开状态
    fn toggle(&mut self, cx: &mut Context<Self>) {
        self.expanded = !self.expanded;
        cx.notify();
    }

    /// 上报审批决定
    fn decide(&mut self, decision: ApprovalDecision, cx: &mut Context<Self>) {
        let id = self.id.clone();
        let conversation = self.conversation.clone();
        conversation
            .update(cx, |conversation, cx| conversation.emit_decision(&id, decision, cx))
            .ok();
    }
}

impl Render for ActivityCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let thinking = self.is_thinking();
        // 思考过程整体使用灰色，避免与「成功」状态色混淆。
        let color = if thinking {
            muted
        } else {
            self.status_color(cx)
        };
        let title = self.title.clone();
        let detail = self.detail.clone();
        let risk = self.risk.map(|risk| risk.label());
        let elapsed = self.elapsed_ms.map(|elapsed| format!("{elapsed} ms"));
        let preview = self.preview.clone();
        let copy_text = format!(
            "{}
{}
{}",
            title,
            detail,
            preview.clone().unwrap_or_default()
        );
        let activity_id = self.id.clone();
        let expanded = self.expanded;
        let expandable = !thinking && (preview.is_some() || detail.chars().count() > 160);

        let header = h_flex()
            .items_center()
            .gap_2()
            .w_full()
            .child(Icon::new(IconName::CircleSmall).xsmall().text_color(color))
            .child(if thinking {
                // 思考过程用灰色斜体的普通字号，与最终答复区分。
                div().text_sm().italic().text_color(muted).child(title).into_any_element()
            } else {
                div().text_sm().font_weight(FontWeight::SEMIBOLD).child(title).into_any_element()
            })
            .when_some(risk, |this, risk| {
                this.child(
                    div()
                        .px_1()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().muted)
                        .text_xs()
                        .text_color(muted)
                        .child(risk),
                )
            })
            .child(div().flex_1())
            .when_some(elapsed, |this, elapsed| {
                this.child(div().text_xs().text_color(muted).child(elapsed))
            })
            .when(!thinking, |this| {
                this.child(div().text_xs().text_color(color).child(self.status.label()))
            })
            .child(
                Button::new(ElementId::Name(format!("activity-copy:{activity_id}").into()))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Copy)
                    .tooltip("复制这条活动的内容")
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                    }),
            )
            .when(expandable, |this| {
                this.child(
                    Button::new(ElementId::Name(format!("activity-toggle:{}", self.id).into()))
                        .ghost()
                        .xsmall()
                        .label(if expanded {
                            "收起"
                        } else {
                            "展开"
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.toggle(cx))),
                )
            });

        let body = (!detail.is_empty()).then(|| {
            if thinking {
                // 思考过程：灰色斜体、正文同款字体，不折叠成一行小字。
                let shown = agent::text::truncate(&detail, 12_000);
                return div()
                    .mt_2()
                    .text_sm()
                    .italic()
                    .text_color(muted)
                    .whitespace_normal()
                    .child(shown);
            }
            let shown = if expanded {
                agent::text::truncate(&detail, 8_000)
            } else {
                format!("{}…", agent::text::truncate(&detail, 160))
            };
            div()
                .mt_2()
                .text_xs()
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(muted)
                .child(shown)
        });

        let preview = preview.map(|preview| {
            div().mt_1().text_xs().text_color(muted).child(agent::text::truncate(&preview, 400))
        });

        let actions = (self.status == ActivityStatus::Pending).then(|| {
            h_flex()
                .items_center()
                .gap_2()
                .mt_2()
                .child(
                    Button::new(ElementId::Name(format!("approval-allow:{}", self.id).into()))
                        .primary()
                        .xsmall()
                        .label("允许一次")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.decide(ApprovalDecision::AllowOnce, cx);
                        })),
                )
                .child(
                    Button::new(ElementId::Name(format!("approval-always:{}", self.id).into()))
                        .outline()
                        .border_2()
                        .xsmall()
                        .label("始终允许")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.decide(ApprovalDecision::AllowAlways, cx);
                        })),
                )
                .child(
                    Button::new(ElementId::Name(format!("approval-deny:{}", self.id).into()))
                        .danger()
                        .xsmall()
                        .label("拒绝")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.decide(ApprovalDecision::Deny, cx);
                        })),
                )
        });

        v_flex()
            .id(ElementId::Name(format!("activity:{}", self.id).into()))
            .role(Role::ListItem)
            .test_support()
            .aria_label(self.title.clone())
            .w_full()
            .p_2()
            .rounded(cx.theme().radius)
            .border_2()
            .border_color(cx.theme().border)
            .bg(cx.theme().muted.opacity(0.25))
            .child(header)
            .children(body)
            .children(preview)
            .children(actions)
    }
}

/// 对话中的本地图片卡片。
pub struct ImageCard {
    path: PathBuf,
    caption: String,
    /// 按文件字节解码的缩略图；路径缓存会在同名文件重新生成后返回旧图。
    image: Option<Arc<Image>>,
    conversation: WeakEntity<Conversation>,
}

impl ImageCard {
    fn new(path: PathBuf, caption: String, conversation: WeakEntity<Conversation>) -> Self {
        Self {
            image: decode_thumbnail(&path),
            path,
            caption,
            conversation,
        }
    }
}

/// 读取并解码本地图片，失败时退回按路径加载。
fn decode_thumbnail(path: &std::path::Path) -> Option<Arc<Image>> {
    let bytes = std::fs::read(path).ok()?;
    let format = image::guess_format(&bytes).ok()?;
    let format = match format {
        image::ImageFormat::Png => ImageFormat::Png,
        image::ImageFormat::Jpeg => ImageFormat::Jpeg,
        image::ImageFormat::Gif => ImageFormat::Gif,
        image::ImageFormat::Bmp => ImageFormat::Bmp,
        _ => return None,
    };
    Some(Arc::new(Image::from_bytes(format, bytes)))
}

impl Render for ImageCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let path = self.path.clone();
        let caption = self.caption.clone();
        let conversation = self.conversation.clone();
        let muted = cx.theme().muted_foreground;

        v_flex()
            .id(ElementId::Name(format!("image:{}", self.path.display()).into()))
            .test_support()
            .gap_1()
            .p_2()
            .rounded(cx.theme().radius)
            .border_2()
            .border_color(cx.theme().border)
            .max_w(px(440.))
            .child(
                div()
                    .id(ElementId::Name(format!("image-open:{}", self.path.display()).into()))
                    .test_support()
                    .cursor_pointer()
                    .w(px(320.))
                    .h(px(200.))
                    .overflow_hidden()
                    .flex_shrink_0()
                    .child(
                        match &self.image {
                            Some(image) => img(image.clone()),
                            None => img(self.path.clone()),
                        }
                        .w_full()
                        .h_full()
                        .object_fit(ObjectFit::Contain),
                    )
                    .on_click(move |event, _, cx| {
                        // 双击只打开一个查看窗口。
                        if event.click_count() > 1 {
                            return;
                        }
                        let path = path.to_string_lossy().to_string();
                        let caption = caption.clone();
                        conversation
                            .update(cx, |_conversation, cx| {
                                cx.emit(ConversationEvent::OpenImage {
                                    path,
                                    caption,
                                });
                            })
                            .ok();
                    }),
            )
            .child(div().text_xs().text_color(muted).truncate().child(self.caption.clone()))
    }
}

/// 对话区中的一行
#[derive(Clone)]
pub enum ChatRow {
    /// 文本消息
    Message(ChatMessage),
    /// 活动卡片
    Activity(Entity<ActivityCard>),
    /// 本地图片
    Image(Entity<ImageCard>),
}

/// 对话区向会话层报告的事件
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConversationEvent {
    /// 用户对一次审批作出决定
    ApprovalDecision {
        /// 审批请求标识符
        id: String,
        /// 决定
        decision: ApprovalDecision,
    },
    /// 用户点击了对话中的图片
    OpenImage {
        /// 图片路径
        path: String,
        /// 说明
        caption: String,
    },
}

/// 对话区：消息记录的滚动、虚拟化与渲染
pub struct Conversation {
    rows: Rc<Vec<ChatRow>>,
    scroller: Entity<MessageScrollerState>,
    /// 与 `rows` 同序：Markdown 解析在后台完成，落地后需重新测量对应行
    parse_observations: Vec<Subscription>,
    /// 活动卡片的高度变化需要重新测量
    activity_observations: Vec<Subscription>,
    next_id: u64,
    /// 正在流式写入的消息 ID
    streaming: Option<u64>,
}

impl EventEmitter<ConversationEvent> for Conversation {}

impl Conversation {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            rows: Rc::new(Vec::new()),
            scroller: cx.new(|cx| MessageScrollerState::new(0, cx)),
            parse_observations: Vec::new(),
            activity_observations: Vec::new(),
            next_id: 0,
            streaming: None,
        }
    }

    /// 当前消息数量（不含活动卡片）
    pub fn message_count(&self) -> usize {
        self.rows.iter().filter(|row| matches!(row, ChatRow::Message(_))).count()
    }

    /// 当前行数量
    #[allow(dead_code)]
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// 最后一条助手回复的 Markdown 原文，供语音播报使用。
    pub fn last_assistant_text(&self) -> Option<&str> {
        self.rows.iter().rev().find_map(|row| match row {
            ChatRow::Message(message) if message.role == ChatRole::Assistant => {
                Some(message.source.as_ref())
            }
            _ => None,
        })
    }

    /// 最后一条文本消息的 Markdown 原文
    #[allow(dead_code)]
    pub fn last_message_source(&self) -> Option<&str> {
        self.rows.iter().rev().find_map(|row| match row {
            ChatRow::Message(message) => Some(message.source.as_ref()),
            ChatRow::Activity(_) | ChatRow::Image(_) => None,
        })
    }

    /// 指定活动卡片的当前状态
    #[allow(dead_code)]
    pub fn activity_status(&self, id: &str, cx: &App) -> Option<ActivityStatus> {
        self.activity(id, cx).map(|card| card.read(cx).status())
    }

    /// 第一个等待确认的审批请求标识符
    #[allow(dead_code)]
    pub fn pending_approval(&self, cx: &App) -> Option<String> {
        self.rows.iter().find_map(|row| match row {
            ChatRow::Activity(card) => {
                let card = card.read(cx);
                (card.status() == ActivityStatus::Pending).then(|| card.id().to_owned())
            }
            ChatRow::Message(_) | ChatRow::Image(_) => None,
        })
    }

    /// 追加一条消息，跟随底部时列表自动滚动到最新内容
    pub fn push(&mut self, role: ChatRole, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let source: SharedString = text.into();
        let content = cx.new(|cx| TextViewState::markdown(&source, cx));
        let id = self.next_id;
        let row = self.rows.len();
        // 解析落地后行高变化：重新测量该行，并按最新内容重绘
        let observation = cx.observe(&content, move |this, _, cx| {
            this.scroller.update(cx, |state, cx| {
                state.remeasure_items(row..row + 1, cx);
            });
            cx.notify();
        });

        let message = ChatMessage {
            id,
            role,
            source,
            content,
        };
        self.next_id += 1;
        Rc::make_mut(&mut self.rows).push(ChatRow::Message(message));
        self.parse_observations.push(observation);
        self.scroller.update(cx, |state, cx| state.append(1, cx));
        cx.notify();
    }

    /// 追加流式文本：已有进行中的消息则拼接，否则开一条新消息
    pub fn stream(&mut self, role: ChatRole, text: &str, cx: &mut Context<Self>) {
        let Some(index) = self.streaming.and_then(|id| self.index_of_message(id)) else {
            let id = self.next_id;
            self.push(role, text, cx);
            self.streaming = Some(id);
            return;
        };

        let Some(ChatRow::Message(message)) = self.rows.get(index) else {
            return;
        };
        let content = message.content.clone();
        content.update(cx, |state, cx| state.push_str(text, cx));
        if let Some(ChatRow::Message(message)) = Rc::make_mut(&mut self.rows).get_mut(index) {
            message.source = SharedString::from(format!("{}{text}", message.source));
        }
        cx.notify();
    }

    /// 结束当前流式消息，后续流式文本开新消息
    pub fn end_stream(&mut self) {
        self.streaming = None;
    }

    /// 追加或更新一条工具活动
    pub fn tool_activity(
        &mut self,
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        let id = id.into();
        if self.index_of_activity(&id, cx).is_some() {
            return;
        }
        let title = format!("调用 {}", name.into());
        let card = self.push_activity(id, title, arguments.into(), cx);
        card.update(cx, |card, cx| {
            card.status = ActivityStatus::Running;
            cx.notify();
        });
    }

    /// 更新工具活动的完成状态
    pub fn finish_activity(
        &mut self,
        id: &str,
        ok: bool,
        preview: String,
        elapsed_ms: u64,
        cx: &mut Context<Self>,
    ) {
        if let Some(card) = self.activity(id, cx) {
            card.update(cx, |card, cx| {
                card.status = if ok {
                    ActivityStatus::Ok
                } else {
                    ActivityStatus::Failed
                };
                card.preview = Some(preview);
                card.elapsed_ms = Some(elapsed_ms);
                cx.notify();
            });
        }
    }

    /// 记录一次审批请求
    pub fn approval_requested(&mut self, request: &ApprovalRequest, cx: &mut Context<Self>) {
        let status = if request.manual {
            ActivityStatus::Pending
        } else {
            ActivityStatus::Running
        };
        let card = self.push_activity(
            request.id.clone(),
            format!("需要确认：{}", request.summary),
            request.summary.clone(),
            cx,
        );
        card.update(cx, |card, cx| {
            card.status = status;
            card.expanded = true;
            card.risk = Some(request.risk);
            cx.notify();
        });
    }

    /// 更新审批结果
    pub fn approval_resolved(
        &mut self,
        id: &str,
        decision: ApprovalDecision,
        reason: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(card) = self.activity(id, cx) {
            card.update(cx, |card, cx| {
                card.status = if decision.is_allowed() {
                    ActivityStatus::Allowed
                } else {
                    ActivityStatus::Denied
                };
                card.preview = Some(reason);
                cx.notify();
            });
        }
    }

    /// 记录一条计划
    pub fn plan(&mut self, text: String, cx: &mut Context<Self>) {
        let id = format!("plan-{}", self.next_id);
        let card = self.push_activity(id, "计划".to_owned(), text, cx);
        card.update(cx, |card, cx| {
            card.status = ActivityStatus::Ok;
            card.expanded = true;
            cx.notify();
        });
    }

    /// 追加推理过程；连续的推理增量合并到同一条卡片
    pub fn reasoning(&mut self, text: &str, cx: &mut Context<Self>) {
        let id = "reasoning";
        if let Some(card) = self.activity(id, cx) {
            card.update(cx, |card, cx| {
                card.detail.push_str(text);
                cx.notify();
            });
            return;
        }
        let card =
            self.push_activity(id.to_owned(), THINKING_TITLE.to_owned(), text.to_owned(), cx);
        card.update(cx, |card, cx| {
            card.status = ActivityStatus::Ok;
            cx.notify();
        });
    }

    /// 结束推理卡片，后续推理开新卡片
    pub fn end_reasoning(&mut self, cx: &mut Context<Self>) {
        let Some(card) = self.activity("reasoning", cx) else {
            return;
        };
        let suffix = self.next_id;
        self.next_id += 1;
        card.update(cx, |card, cx| {
            card.id = format!("reasoning-{suffix}");
            cx.notify();
        });
    }

    /// 清空全部消息
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.rows = Rc::new(Vec::new());
        self.parse_observations.clear();
        self.activity_observations.clear();
        self.streaming = None;
        self.scroller.update(cx, |state, cx| state.reset(0, cx));
        cx.notify();
    }

    /// 导出显示记录，供会话文件保存。
    pub fn snapshot(&self, cx: &App) -> Vec<TranscriptEntry> {
        self.rows
            .iter()
            .map(|row| match row {
                ChatRow::Message(message) => TranscriptEntry::Text {
                    role: message.role,
                    text: message.source.to_string(),
                },
                ChatRow::Activity(card) => TranscriptEntry::Activity(card.read(cx).record()),
                ChatRow::Image(card) => TranscriptEntry::Image {
                    path: card.read(cx).path.display().to_string(),
                    caption: card.read(cx).caption.clone(),
                },
            })
            .collect()
    }

    /// 从会话文件恢复显示记录；未完成的审批显示为已中断。
    pub fn restore(&mut self, entries: Vec<TranscriptEntry>, cx: &mut Context<Self>) {
        self.clear(cx);
        for entry in entries {
            match entry {
                TranscriptEntry::Text {
                    role,
                    text,
                } => self.push(role, text, cx),
                TranscriptEntry::Activity(record) => {
                    let conversation = cx.entity().downgrade();
                    let card = cx.new(|_| ActivityCard::from_record(record, conversation));
                    let row = self.rows.len();
                    let observation = cx.observe(&card, move |this, _, cx| {
                        this.scroller.update(cx, |state, cx| {
                            state.remeasure_items(row..row + 1, cx);
                        });
                        cx.notify();
                    });
                    self.activity_observations.push(observation);
                    Rc::make_mut(&mut self.rows).push(ChatRow::Activity(card));
                    self.scroller.update(cx, |state, cx| state.append(1, cx));
                }
                TranscriptEntry::Image {
                    path,
                    caption,
                } => {
                    self.push_image(path, caption, cx);
                }
            }
        }
        cx.notify();
    }

    /// 追加一张本地图片；同一路径紧邻出现时只保留一张。
    pub fn push_image(&mut self, path: String, caption: String, cx: &mut Context<Self>) {
        // 同一张图在生成本身与随后的 show_image 里各出现一次，只保留一行。
        // 去重只看当前这一轮（最后一条用户消息之后）：之后的轮次重新生成同名文件
        // 应当显示新内容，而不是被当成重复丢掉。
        let start = self
            .rows
            .iter()
            .rposition(
                |row| matches!(row, ChatRow::Message(message) if message.role == ChatRole::User),
            )
            .map(|index| index + 1)
            .unwrap_or(0);
        let repeated = self.rows[start..].iter().any(|row| {
            matches!(row, ChatRow::Image(card) if card.read(cx).path.to_string_lossy() == path)
        });
        if repeated {
            return;
        }
        let conversation = cx.entity().downgrade();
        let card = cx.new(|_| ImageCard::new(PathBuf::from(path), caption, conversation));
        Rc::make_mut(&mut self.rows).push(ChatRow::Image(card));
        self.scroller.update(cx, |state, cx| state.append(1, cx));
        cx.notify();
    }

    /// 追加一批工具产出的图片。
    pub fn push_images(&mut self, paths: &[String], cx: &mut Context<Self>) {
        for path in paths {
            let caption = std::path::Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone());
            self.push_image(path.clone(), caption, cx);
        }
    }

    /// 上报一次审批决定
    fn emit_decision(&mut self, id: &str, decision: ApprovalDecision, cx: &mut Context<Self>) {
        cx.emit(ConversationEvent::ApprovalDecision {
            id: id.to_owned(),
            decision,
        });
    }

    /// 查找文本消息所在行
    fn index_of_message(&self, id: u64) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| matches!(row, ChatRow::Message(message) if message.id == id))
    }

    /// 查找活动卡片
    fn activity(&self, id: &str, cx: &App) -> Option<Entity<ActivityCard>> {
        self.rows.iter().find_map(|row| match row {
            ChatRow::Activity(card) if card.read(cx).id() == id => Some(card.clone()),
            _ => None,
        })
    }

    /// 查找活动卡片所在行
    fn index_of_activity(&self, id: &str, cx: &App) -> Option<usize> {
        self.rows.iter().position(|row| match row {
            ChatRow::Activity(card) => card.read(cx).id() == id,
            ChatRow::Message(_) | ChatRow::Image(_) => false,
        })
    }

    /// 追加活动卡片并测量新行
    fn push_activity(
        &mut self,
        id: String,
        title: String,
        detail: String,
        cx: &mut Context<Self>,
    ) -> Entity<ActivityCard> {
        let conversation = cx.entity().downgrade();
        let card = cx.new(|_| ActivityCard::new(id, title, detail, conversation));
        let row = self.rows.len();
        let observation = cx.observe(&card, move |this, _, cx| {
            this.scroller.update(cx, |state, cx| {
                state.remeasure_items(row..row + 1, cx);
            });
            cx.notify();
        });
        self.activity_observations.push(observation);
        Rc::make_mut(&mut self.rows).push(ChatRow::Activity(card.clone()));
        self.scroller.update(cx, |state, cx| state.append(1, cx));
        cx.notify();
        card
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
        let is_empty = self.rows.is_empty();
        let rows = self.rows.clone();

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
                        move |ix, _window, _cx| match rows.get(ix) {
                            Some(ChatRow::Message(message)) => {
                                render_message_row(message).into_any_element()
                            }
                            Some(ChatRow::Activity(card)) => card.clone().into_any_element(),
                            Some(ChatRow::Image(card)) => card.clone().into_any_element(),
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
    use super::{ActivityStatus, ChatRole, ChatRow, Conversation, TranscriptEntry, presentation};
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
            let rows = conversation.read(cx).rows.clone();
            let ChatRow::Message(message) = &rows[0] else {
                panic!("expected a message row");
            };
            let rendered = message.content.read(cx).rendered_text();
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

    #[gpui_kit::test]
    fn images_round_trip_through_the_transcript(cx: &mut TestAppContext) {
        let conversation = cx.update(|cx| cx.new(Conversation::new));
        let path = std::env::temp_dir().join("haloeth-image-row.png");
        std::fs::write(&path, b"not-a-real-image").expect("file");

        cx.update(|cx| {
            conversation.update(cx, |conversation, cx| {
                conversation.push_images(&[path.display().to_string()], cx);
            });
        });
        cx.update(|cx| {
            let snapshot = conversation.read(cx).snapshot(cx);
            assert!(matches!(snapshot[0], TranscriptEntry::Image { .. }));
        });

        let conversation2 = cx.update(|cx| cx.new(Conversation::new));
        let snapshot = cx.update(|cx| conversation.read(cx).snapshot(cx));
        cx.update(|cx| {
            conversation2.update(cx, |conversation, cx| conversation.restore(snapshot, cx));
        });
        cx.update(|cx| {
            assert_eq!(conversation2.read(cx).row_count(), 1);
        });
        let _ = std::fs::remove_file(path);
    }

    #[gpui_kit::test]
    fn activity_rows_are_separate_from_messages(cx: &mut TestAppContext) {
        let conversation = cx.update(|cx| cx.new(Conversation::new));

        cx.update(|cx| {
            conversation.update(cx, |conversation, cx| {
                conversation.push(ChatRole::User, "任务", cx);
                conversation.tool_activity("call_1", "shell", "{\"command\":\"ls\"}", cx);
                conversation.finish_activity("call_1", true, "ok".into(), 12, cx);
            });
        });

        cx.update(|cx| {
            let conversation = conversation.read(cx);
            assert_eq!(conversation.message_count(), 1);
            assert_eq!(conversation.row_count(), 2);
            let ChatRow::Activity(card) = &conversation.rows[1] else {
                panic!("expected an activity row");
            };
            assert_eq!(card.read(cx).status(), ActivityStatus::Ok);
        });
    }
}
