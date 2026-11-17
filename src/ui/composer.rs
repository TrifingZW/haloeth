use gpui_kit::base::{TestSupportExt as _, v_flex};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::*;

use crate::config::SendKey;

/// 发送键提示，按发送偏好与平台生成。
pub fn submit_hint(send_key: SendKey) -> &'static str {
    match send_key {
        SendKey::Enter => "Enter 发送，Shift+Enter 换行",
        #[cfg(target_os = "macos")]
        SendKey::SecondaryEnter => "⌘+Enter 发送，Enter 换行",
        #[cfg(not(target_os = "macos"))]
        SendKey::SecondaryEnter => "Ctrl+Enter 发送，Enter 换行",
    }
}

/// 缓冲区默认高度。
const BUFFER_HEIGHT: Pixels = px(140.);
/// 缓冲区最小 / 最大高度。
const BUFFER_MIN: Pixels = px(80.);
const BUFFER_MAX: Pixels = px(420.);

/// 输入缓冲区向会话层报告的事件
#[derive(Clone)]
pub enum ComposerEvent {
    /// 用户提交了一段非空文本
    Submit(SharedString),
}

/// 输入缓冲区：按偏好使用 Enter 或 Ctrl+Enter 提交，另一个组合键换行
pub struct Composer {
    buffer: Entity<EditorState>,
    can_submit: bool,
    send_key: SendKey,
    /// 是否显示行号。
    line_numbers: bool,
    /// 已应用到编辑器的行号设置。
    line_numbers_applied: bool,
    /// 缓冲区当前高度。
    height: Pixels,
    /// 拖动调整高度的起始位置。
    drag_start: Option<Pixels>,
    /// 拖动开始时的缓冲高度。
    drag_start_height: Pixels,
    _subscriptions: Vec<Subscription>,
}

/// 拖动调整高度时使用一个不可见的拖影。
struct DragGhost;

impl Render for DragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size(px(0.))
    }
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Composer {
    pub fn new(
        window: &mut Window,
        send_key: SendKey,
        line_numbers: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let buffer = cx.new(|cx| {
            EditorState::new(window, cx)
                .placeholder("描述你要完成的任务…")
                .submit_on_enter(send_key.sends_on_enter())
                .line_number(line_numbers)
        });

        let subscriptions = vec![cx.subscribe_in(
            &buffer,
            window,
            |this, buffer, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.sync_can_submit(buffer, cx),
                InputEvent::PressEnter {
                    secondary,
                    shift,
                } => {
                    // Enter 模式：普通 Enter 发送，Shift+Enter 换行。
                    // Ctrl+Enter 模式：只有带修饰键的 Enter 发送，其余都换行。
                    let submit = if this.send_key.sends_on_enter() {
                        !*shift
                    } else {
                        *secondary
                    };
                    if submit {
                        this.submit(window, cx);
                    }
                }
                _ => {}
            },
        )];

        Self {
            buffer,
            can_submit: false,
            send_key,
            line_numbers,
            line_numbers_applied: line_numbers,
            height: BUFFER_HEIGHT,
            drag_start: None,
            drag_start_height: BUFFER_HEIGHT,
            _subscriptions: subscriptions,
        }
    }

    /// 编辑器缓冲区状态
    pub fn buffer(&self) -> &Entity<EditorState> {
        &self.buffer
    }

    /// 当前的发送键偏好
    pub fn send_key(&self) -> SendKey {
        self.send_key
    }

    /// 当前是否显示行号（测试用）。
    #[cfg(test)]
    pub fn line_numbers(&self) -> bool {
        self.line_numbers
    }

    /// 设置是否显示行号；在下一帧应用。
    pub fn set_line_numbers(&mut self, line_numbers: bool, cx: &mut Context<Self>) {
        if self.line_numbers != line_numbers {
            self.line_numbers = line_numbers;
            cx.notify();
        }
    }

    /// 切换发送键偏好：设置保存后立即生效。
    pub fn set_send_key(&mut self, send_key: SendKey, cx: &mut Context<Self>) {
        if self.send_key == send_key {
            return;
        }
        self.send_key = send_key;
        self.buffer
            .update(cx, |buffer, cx| buffer.set_submit_on_enter(send_key.sends_on_enter(), cx));
        cx.notify();
    }

    /// 缓冲区是否含有可提交的文本
    pub fn is_submittable(&self) -> bool {
        self.can_submit
    }

    /// 同步发送按钮的可用状态
    fn sync_can_submit(&mut self, buffer: &Entity<EditorState>, cx: &mut Context<Self>) {
        let can_submit = !buffer.read(cx).value().trim().is_empty();
        if self.can_submit != can_submit {
            self.can_submit = can_submit;
            cx.notify();
        }
    }

    /// 把文本追加到缓冲区，用于语音识别结果；保留用户已输入的内容。
    pub fn append_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.buffer.read(cx).value().to_string();
        let combined = if current.trim().is_empty() {
            text.to_owned()
        } else if current.ends_with(' ') || current.ends_with('\n') {
            format!("{current}{text}")
        } else {
            format!("{current} {text}")
        };
        self.buffer.update(cx, |buffer, cx| {
            buffer.set_value(combined.clone(), window, cx);
            buffer.focus(window, cx);
        });
        self.sync_can_submit(&self.buffer.clone(), cx);
    }

    /// 提交当前文本，清空缓冲区并把焦点交还缓冲区
    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.buffer.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }

        self.buffer.update(cx, |buffer, cx| {
            buffer.set_value("", window, cx);
            buffer.focus(window, cx);
        });
        self.can_submit = false;
        cx.emit(ComposerEvent::Submit(text.into()));
        cx.notify();
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.line_numbers_applied != self.line_numbers {
            self.line_numbers_applied = self.line_numbers;
            let enabled = self.line_numbers;
            self.buffer.update(cx, |buffer, cx| buffer.set_line_number(enabled, window, cx));
        }
        let height = self.height;

        v_flex()
            .id("composer")
            .test_support()
            .w_full()
            .flex_shrink_0()
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<()>, _, cx| {
                if let Some(start) = this.drag_start {
                    let delta = event.event.position.y - start;
                    this.height = (this.drag_start_height - delta).clamp(BUFFER_MIN, BUFFER_MAX);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.drag_start = None;
                    cx.notify();
                }),
            )
            .child(
                // 拖动条：向上 / 向下拖动调整输入区高度。
                div()
                    .id("composer-resize")
                    .test_support()
                    .w_full()
                    .h(px(6.))
                    .cursor_ns_resize()
                    .hover(|style| style.bg(cx.theme().border))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.drag_start = Some(event.position.y);
                            this.drag_start_height = this.height;
                            cx.notify();
                        }),
                    )
                    .on_drag((), |_, _, _, cx| cx.new(|_| DragGhost)),
            )
            .child(
                // 缓冲区与应用同一平面；上边界由全局 2px 边框给出，
                // 下边界由底部栏的上边框承担，避免两条线叠粗
                div()
                    .id("buffer")
                    .test_support()
                    .w_full()
                    .border_t_2()
                    .border_color(cx.theme().border)
                    .child(Editor::new(&self.buffer).h(height).appearance(false).bordered(false)),
            )
    }
}
