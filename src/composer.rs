use gpui_kit::base::{TestSupportExt as _, v_flex};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{Editor, EditorState, InputEvent};
use gpui_kit::*;

/// 提交加速键提示，按平台使用对应的修饰键
#[cfg(target_os = "macos")]
pub const SUBMIT_HINT: &str = "⌘+Enter 发送";
#[cfg(not(target_os = "macos"))]
pub const SUBMIT_HINT: &str = "Ctrl+Enter 发送";

/// 缓冲区可视高度，按主题基准字号缩放
const BUFFER_HEIGHT: Rems = rems(7.);

/// 输入缓冲区向会话层报告的事件
#[derive(Clone)]
pub enum ComposerEvent {
    /// 用户提交了一段非空文本
    Submit(SharedString),
}

/// 输入缓冲区：IDE 式编辑器缓冲区，Enter 换行，Ctrl+Enter 提交
pub struct Composer {
    buffer: Entity<EditorState>,
    can_submit: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Composer {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let buffer = cx.new(|cx| EditorState::new(window, cx).placeholder("描述你要完成的任务…"));

        let subscriptions = vec![cx.subscribe_in(
            &buffer,
            window,
            |this, buffer, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.sync_can_submit(buffer, cx),
                // 编辑器把 Enter 交给换行，提交由 secondary-Enter 触发
                InputEvent::PressEnter {
                    secondary: true,
                    ..
                } => this.submit(window, cx),
                _ => {}
            },
        )];

        Self {
            buffer,
            can_submit: false,
            _subscriptions: subscriptions,
        }
    }

    /// 编辑器缓冲区状态
    pub fn buffer(&self) -> &Entity<EditorState> {
        &self.buffer
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().id("composer").test_support().w_full().flex_shrink_0().child(
            // 缓冲区与应用同一平面；下边界由底部栏的上边框承担，避免两条细线叠粗
            div()
                .id("buffer")
                .test_support()
                .w_full()
                .border_t_1()
                .border_color(cx.theme().border)
                .child(
                    Editor::new(&self.buffer).h(BUFFER_HEIGHT).appearance(false).bordered(false),
                ),
        )
    }
}
