mod composer;
mod conversation;
mod model;

use composer::{Composer, ComposerEvent, SUBMIT_HINT};
use conversation::{ChatRole, Conversation};
use gpui_kit::{
    assets::{AllAssets, IconName},
    base::{TestSupportExt as _, h_flex, v_flex},
    component::{
        ActiveTheme as _, Disableable as _, Icon, Sizable, TitleBar,
        button::{Button, ButtonVariants},
        menu::{AppMenuBar, DropdownMenu, PopupMenuItem},
        separator::Separator,
        status_bar::StatusBar,
    },
    *,
};
use model::{CATALOG, Model};

actions!(haloeth, [NewSession, ClearSession, ShowSettings, ShowAbout, Quit]);

/// 窗口默认尺寸，竖长矩形，y 轴为长边
const WINDOW_SIZE: Size<Pixels> = size(px(480.), px(768.));
/// 窗口最小尺寸，保证各区域在极限缩放下仍可用
const WINDOW_MIN_SIZE: Size<Pixels> = size(px(400.), px(640.));

/// Haloeth 主视图，持有窗口级保留状态
struct Haloeth {
    menu_bar: Entity<AppMenuBar>,
    conversation: Entity<Conversation>,
    composer: Entity<Composer>,
    model: Model,
    _composer_subscription: Subscription,
    _conversation_observation: Subscription,
    _composer_observation: Subscription,
}

impl Haloeth {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let conversation = cx.new(Conversation::new);
        let composer = cx.new(|cx| Composer::new(window, cx));
        let subscription = cx.subscribe(&composer, |this, _composer, event: &ComposerEvent, cx| {
            let ComposerEvent::Submit(text) = event;
            this.conversation.update(cx, |conversation, cx| {
                conversation.push(ChatRole::User, text.clone(), cx);
            });
        });
        // 底部栏的消息数与发送按钮可用性跟随各自状态变化
        let observation = cx.observe(&conversation, |_, _, cx| cx.notify());
        let composer_observation = cx.observe(&composer, |_, _, cx| cx.notify());

        // 呼出即输入：窗口就绪后把焦点交给输入缓冲区
        let buffer = composer.read(cx).buffer().clone();
        window.defer(cx, move |window, cx| {
            buffer.update(cx, |buffer, cx| buffer.focus(window, cx));
        });

        Self {
            menu_bar: AppMenuBar::new(cx),
            conversation,
            composer,
            model: model::default_model(),
            _composer_subscription: subscription,
            _conversation_observation: observation,
            _composer_observation: composer_observation,
        }
    }

    /// 切换当前模型
    fn select_model(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(model) = CATALOG.iter().find(|model| model.id() == id) else {
            return;
        };
        if self.model == *model {
            return;
        }

        self.model = model.clone();
        cx.notify();
    }

    /// 品牌标识，标题栏最左侧
    fn render_brand(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .items_center()
            .gap_1()
            .child(
                Icon::new(IconName::Orbit)
                    .small()
                    .font_weight(FontWeight::BOLD)
                    .text_color(cx.theme().primary),
            )
            .child(div().text_sm().font_weight(FontWeight::BOLD).child("Haloeth"))
    }

    /// 主菜单图标按钮，点击呼出应用主菜单
    fn render_main_menu(&self) -> impl IntoElement {
        Button::new("main-menu")
            .icon(IconName::Menu)
            .ghost()
            .small()
            .compact()
            .tooltip("主菜单")
            .accessibility_label("主菜单")
            .dropdown_menu(|menu, _, _| {
                menu.menu_with_icon("新会话", IconName::Plus, Box::new(NewSession))
                    .menu_with_icon("清空会话", IconName::Eraser, Box::new(ClearSession))
                    .separator()
                    .menu_with_icon("设置…", IconName::Settings, Box::new(ShowSettings))
                    .menu_with_icon("关于 Haloeth…", IconName::Info, Box::new(ShowAbout))
                    .separator()
                    .menu_with_icon("退出", IconName::LogOut, Box::new(Quit))
            })
    }

    /// 标题栏
    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        TitleBar::new().child(
            h_flex()
                .items_center()
                .gap_2()
                .min_w_0()
                .flex_1()
                .child(self.render_brand(cx))
                .child(self.render_main_menu())
                .child(self.menu_bar.clone()),
        )
    }

    /// 模型切换：底部栏中的下拉触发器
    fn render_model_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let haloeth = cx.weak_entity();
        let current_id = self.model.id();

        Button::new("model")
            .ghost()
            .small()
            .label(self.model.name())
            .dropdown_caret(true)
            .tooltip("切换模型")
            .accessibility_label(format!("模型：{}", self.model.name()))
            .dropdown_menu(move |menu, _, _| {
                CATALOG.iter().fold(menu, |menu, model| {
                    let haloeth = haloeth.clone();
                    let id = model.id();
                    menu.item(PopupMenuItem::new(model.name()).checked(id == current_id).on_click(
                        move |_, _, cx| {
                            haloeth.update(cx, |this, cx| this.select_model(id, cx)).ok();
                        },
                    ))
                })
            })
    }

    /// 底部栏：左侧模型与会话状态，右侧提交提示与发送动作
    fn render_bottom_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        StatusBar::new().right(self.render_model_picker(cx)).right(
            Button::new("send")
                .primary()
                .small()
                .icon(IconName::Airplay)
                .tooltip("发送")
                .accessibility_label("发送")
                .disabled(!self.composer.read(cx).is_submittable())
                .on_click(cx.listener(|this, _, window, cx| {
                    this.composer.update(cx, |composer, cx| composer.submit(window, cx));
                })),
        )
    }
}

/// 状态项：带状态角色与无障碍名称的文本
fn status_item(id: &'static str, text: impl Into<SharedString>) -> impl IntoElement {
    let label: SharedString = text.into();

    div().id(id).role(Role::Status).test_support().aria_label(label.clone()).child(label)
}

impl Render for Haloeth {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .child(self.render_title_bar(cx))
            .child(self.conversation.clone())
            .child(self.composer.clone())
            .child(self.render_bottom_bar(cx))
    }
}

fn main() {
    application().with_assets(AllAssets).run(|cx| {
        init(cx);

        cx.on_action(|_: &Quit, cx| cx.quit());

        open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    WINDOW_SIZE,
                    cx,
                ))),
                window_min_size: Some(WINDOW_MIN_SIZE),
                titlebar: Some(TitlebarOptions {
                    title: Some("Haloeth".into()),
                    ..TitleBar::title_bar_options()
                }),
                ..TitleBar::window_options()
            },
            cx,
            |window, cx| cx.new(|cx| Haloeth::new(window, cx)),
        )
        .expect("Failed to open window");
    });
}

#[cfg(test)]
mod tests {
    use super::{Haloeth, WINDOW_MIN_SIZE, WINDOW_SIZE};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Entity, Pixels, Role, Size, TestAppContext, Window, px,
    };

    /// 打开真实外壳窗口，返回视图与窗口句柄
    fn open_shell(
        cx: &mut TestAppContext,
        size: Size<Pixels>,
    ) -> (Entity<Haloeth>, AnyWindowHandle) {
        cx.update(gpui_kit::init);

        let mut built = None;
        let handle = cx.open_window(size, |window, cx| {
            let haloeth = cx.new(|cx| Haloeth::new(window, cx));
            built = Some(haloeth.clone());
            Root::new(haloeth, window, cx)
        });

        (built.expect("the window build closure ran"), handle.into())
    }

    /// 底部栏各项按阅读顺序排列，且都在窗口宽度内
    fn assert_bottom_bar_fits(window: &Window) {
        let content = window.find("conversation").bounds();
        let mut previous_right = content.left();

        for id in ["model", "session-state", "message-count", "send"] {
            let bounds = window.find(id).bounds();
            assert!(bounds.left() >= previous_right, "{id} 与相邻项重叠");
            assert!(bounds.right() <= content.right(), "{id} 越出窗口");
            previous_right = bounds.right();
        }
    }

    /// 对话区从引导态进入消息列表，追加的消息按顺序渲染并携带发言方名称
    #[gpui_kit::test]
    fn conversation_area_shows_prompt_then_appended_messages_in_order(cx: &mut TestAppContext) {
        let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("conversation-empty").visible());
            assert!(window.try_find(("message", 0_u64)).is_none());

            haloeth.update(cx, |haloeth, cx| {
                let conversation = haloeth.conversation.clone();
                conversation.update(cx, |conversation, cx| {
                    conversation.push(crate::conversation::ChatRole::User, "列出当前目录", cx);
                    conversation.push(
                        crate::conversation::ChatRole::Assistant,
                        "好的，已列出。",
                        cx,
                    );
                });
            });
            window.render_frame(cx);

            assert!(window.try_find("conversation-empty").is_none());

            let region = window.find("conversation").bounds();
            let user_row = window.find(("message", 0_u64));
            let assistant_row = window.find(("message", 1_u64));
            assert_eq!(user_row.role(), Some(Role::ListItem));
            assert_eq!(user_row.label(), Some("你：列出当前目录"));
            assert_eq!(assistant_row.label(), Some("Haloeth：好的，已列出。"));
            assert!(user_row.visible() && assistant_row.visible());
            assert!(user_row.bounds().size.height > px(0.));
            assert!(user_row.bounds().top() >= region.top());
            assert!(assistant_row.bounds().bottom() <= region.bottom());
            assert!(assistant_row.bounds().top() >= user_row.bounds().bottom());
        })
        .unwrap();
    }

    /// 最小窗口尺寸下底部栏仍按阅读顺序排布，不重叠也不越界
    #[gpui_kit::test]
    fn bottom_bar_fits_at_minimum_window(cx: &mut TestAppContext) {
        let (_, handle) = open_shell(cx, WINDOW_MIN_SIZE);

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_bottom_bar_fits(window);
        })
        .unwrap();
    }

    /// 底部栏的模型菜单列出目录中的模型，选择后当前模型随之切换
    #[gpui_kit::test]
    fn model_picker_switches_the_session_model(cx: &mut TestAppContext) {
        let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("model").label(), Some("模型：Claude Sonnet 4.5"));
        })
        .unwrap();

        cx.update_window(handle, |_, window, cx| {
            window.click("model", cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let mut menu = window.within(r#"popover:dropdown-menu:Name("model")"#);
            assert_eq!(
                menu.find(gpui_kit::ElementId::Integer(0)).label(),
                Some("Claude Sonnet 4.5")
            );
            assert_eq!(menu.find(gpui_kit::ElementId::Integer(1)).label(), Some("GPT-5 Codex"));
            menu.click(gpui_kit::ElementId::Integer(1), cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("model").label(), Some("模型：GPT-5 Codex"));
            assert_bottom_bar_fits(window);
        })
        .unwrap();

        cx.update(|cx| {
            assert_eq!(haloeth.read(cx).model.name(), "GPT-5 Codex");
        });
    }

    /// 空缓冲区拒绝发送；Enter 换行，Ctrl+Enter 与发送按钮提交，提交后写入对话并清空缓冲区
    #[gpui_kit::test]
    fn composer_submits_and_resets(cx: &mut TestAppContext) {
        let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
        let composer = haloeth.read_with(cx, |haloeth, _cx| haloeth.composer.clone());
        let buffer_id =
            ("input", composer.read_with(cx, |composer, _cx| composer.buffer().entity_id()));

        // 空输入：发送按钮不生效
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("send", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("conversation-empty").visible());
            assert_eq!(window.find("send").label(), Some("发送"));
            assert_eq!(window.find("session-state").label(), Some("就绪"));
            assert_eq!(window.find("message-count").label(), Some("0 条消息"));

            // 缓冲区通体满宽，与对话区共用同一条基准线；下边界由底部栏承担
            let conversation = window.find("conversation").bounds();
            let buffer = window.find("buffer").bounds();
            assert_eq!(buffer.left(), conversation.left());
            assert_eq!(buffer.right(), conversation.right());
            assert!(buffer.size.height > px(0.));
            assert_eq!(buffer.bottom(), window.find(buffer_id).bounds().bottom());

            // 底部只有一栏：状态项与发送动作同处一行
            let state = window.find("session-state").bounds();
            let count = window.find("message-count").bounds();
            let send = window.find("send").bounds();
            assert!(buffer.bottom() <= state.top());
            assert!(state.right() <= count.left());
            assert!(state.top() < send.bottom() && send.top() < state.bottom());
            assert!(count.right() <= send.left());
        })
        .unwrap();

        // Enter 在编辑器里换行，不提交
        cx.update_window(handle, |_, window, cx| {
            window.click(buffer_id, cx);
            window.input("列出当前目录", cx);
            window.press("enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("conversation-empty").visible());
            assert_eq!(composer.read(cx).buffer().read(cx).value().as_ref(), "列出当前目录\n");
        })
        .unwrap();

        // Ctrl+Enter 提交
        cx.update_window(handle, |_, window, cx| {
            window.press("secondary-enter", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("conversation-empty").is_none());
            assert_eq!(window.find(("message", 0_u64)).label(), Some("你：列出当前目录"));
            assert!(composer.read(cx).buffer().read(cx).value().is_empty());
            assert_eq!(window.find("message-count").label(), Some("1 条消息"));
        })
        .unwrap();

        // 发送按钮提交
        cx.update_window(handle, |_, window, cx| {
            window.click(buffer_id, cx);
            window.input("再删掉临时文件", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.click("send", cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find(("message", 1_u64)).label(), Some("你：再删掉临时文件"));
            assert!(composer.read(cx).buffer().read(cx).value().is_empty());
            assert_eq!(window.find("message-count").label(), Some("2 条消息"));
            assert!(
                window.find(("message", 1_u64)).bounds().top()
                    >= window.find(("message", 0_u64)).bounds().bottom()
            );
            assert!(
                window.find("message-count").bounds().top()
                    >= window.find("composer").bounds().bottom()
            );
        })
        .unwrap();
    }
}
