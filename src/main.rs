//! Haloeth 主视图：标题栏、对话区、输入缓冲区与状态栏的组合。
//!
//! 应用内所有边框统一 2px，一律使用 GPUI 的 `border_2` 系列缩放方法；
//! 组件自带的 1px 边框在调用处覆盖。

mod composer;
mod config;
mod conversation;
mod models;
mod session;

use composer::{Composer, ComposerEvent};
use conversation::{ChatRole, Conversation};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    assets::{AllAssets, IconName},
    base::{TestSupportExt as _, h_flex, v_flex},
    component::{
        ActiveTheme as _, Disableable as _, Icon, Sizable, TitleBar, WindowExt as _,
        button::{Button, ButtonVariants},
        dialog::{DialogDescription, DialogFooter},
        menu::{AppMenuBar, DropdownMenu, PopupMenuItem},
        status_bar::StatusBar,
    },
    *,
};
use models::ModelsForm;
use session::{Session, Status};

actions!(haloeth, [NewSession, ClearSession, ShowModels, ShowAbout, Quit]);

/// 窗口默认尺寸，竖长矩形，y 轴为长边
const WINDOW_SIZE: Size<Pixels> = size(px(480.), px(768.));
/// 窗口最小尺寸，保证各区域在极限缩放下仍可用
const WINDOW_MIN_SIZE: Size<Pixels> = size(px(400.), px(640.));

/// Haloeth 主视图，持有窗口级保留状态
struct Haloeth {
    menu_bar: Entity<AppMenuBar>,
    conversation: Entity<Conversation>,
    composer: Entity<Composer>,
    session: Entity<Session>,
    _composer_subscription: Subscription,
    _conversation_observation: Subscription,
    _composer_observation: Subscription,
    _session_observation: Subscription,
}

impl Haloeth {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let conversation = cx.new(Conversation::new);
        let session = cx.new(|_| Session::new(config::Config::load(), conversation.clone()));
        let composer = cx.new(|cx| Composer::new(window, cx));
        let subscription = cx.subscribe(&composer, |this, _composer, event: &ComposerEvent, cx| {
            let ComposerEvent::Submit(text) = event;
            this.conversation.update(cx, |conversation, cx| {
                conversation.push(ChatRole::User, text.clone(), cx);
            });
            this.session.update(cx, |session, cx| session.submit(text.to_string(), cx));
        });
        // 底部栏的消息数、会话状态与发送按钮可用性跟随各自状态变化
        let observation = cx.observe(&conversation, |_, _, cx| cx.notify());
        let composer_observation = cx.observe(&composer, |_, _, cx| cx.notify());
        let session_observation = cx.observe(&session, |_, _, cx| cx.notify());

        // 呼出即输入：窗口就绪后把焦点交给输入缓冲区
        let buffer = composer.read(cx).buffer().clone();
        window.defer(cx, move |window, cx| {
            buffer.update(cx, |buffer, cx| buffer.focus(window, cx));
        });

        Self {
            menu_bar: AppMenuBar::new(cx),
            conversation,
            composer,
            session,
            _composer_subscription: subscription,
            _conversation_observation: observation,
            _composer_observation: composer_observation,
            _session_observation: session_observation,
        }
    }

    /// 切换当前模型：提供商与模型一同确定
    fn select_model(&mut self, provider: &str, model: &str, cx: &mut Context<Self>) {
        self.session.update(cx, |session, cx| session.select_model(provider, model, cx));
    }

    /// 新会话：清空对话记录与代理记忆
    fn new_session(&mut self, _: &NewSession, _window: &mut Window, cx: &mut Context<Self>) {
        self.conversation.update(cx, |conversation, cx| conversation.clear(cx));
        self.session.update(cx, |session, cx| session.reset(cx));
    }

    /// 清空会话：仅清空对话记录，保留代理记忆
    fn clear_session(&mut self, _: &ClearSession, _window: &mut Window, cx: &mut Context<Self>) {
        self.conversation.update(cx, |conversation, cx| conversation.clear(cx));
    }

    /// 打开模型对话框：编辑作用于草稿，保存后才生效
    fn show_models(&mut self, _: &ShowModels, window: &mut Window, cx: &mut Context<Self>) {
        let config = self.session.read(cx).config().clone();
        let session = self.session.downgrade();
        let form = cx.new(|cx| ModelsForm::new(window, &config, session, cx));

        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .border_2()
                .title("模型")
                .overlay_closable(false)
                .child(form.clone())
                .footer(form.update(cx, |form, cx| form.render_footer(cx).into_any_element()))
                .on_ok({
                    let form = form.clone();
                    move |_, _, cx| form.update(cx, |form, cx| form.save(cx))
                })
        });
    }

    /// 打开关于对话框
    fn show_about(&mut self, _: &ShowAbout, window: &mut Window, cx: &mut Context<Self>) {
        let version = env!("CARGO_PKG_VERSION");
        let config_path = config::Config::path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "未知".to_owned());

        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .border_2()
                .title("Haloeth")
                .child(
                    v_flex()
                        .gap_4()
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    Icon::new(IconName::Orbit)
                                        .small()
                                        .text_color(cx.theme().primary),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(format!("版本 {version}")),
                                ),
                        )
                        .child(
                            DialogDescription::new()
                                .child("轻量代理：随时呼出，单会话，完成单一任务。"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .truncate()
                                .child(format!("配置文件：{config_path}")),
                        ),
                )
                .footer(
                    DialogFooter::new().child(
                        Button::new("about-close")
                            .primary()
                            .label("关闭")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
                )
        });
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
                    .menu_with_icon("模型…", IconName::BrainCircuit, Box::new(ShowModels))
                    .menu_with_icon("关于 Haloeth…", IconName::Info, Box::new(ShowAbout))
                    .separator()
                    .menu_with_icon("退出", IconName::LogOut, Box::new(Quit))
            })
    }

    /// 标题栏
    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        TitleBar::new().border_b_2().child(
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

    /// 模型切换：底部栏的下拉触发器，按提供商分组列出全部模型；没有模型时不可展开
    fn render_model_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let config = self.session.read(cx).config();
        let current =
            config.selection().map(|(provider, model)| (provider.to_owned(), model.to_owned()));
        let groups: Vec<(String, Vec<String>)> = config
            .providers
            .iter()
            .filter(|provider| !provider.models.is_empty())
            .map(|provider| (provider.name.clone(), provider.models.clone()))
            .collect();
        let label = current
            .as_ref()
            .map(|(_, model)| model.clone())
            .unwrap_or_else(|| "未选择模型".to_owned());
        let trigger = Button::new("model")
            .ghost()
            .small()
            .label(label.clone())
            .tooltip("切换模型")
            .accessibility_label(match &current {
                Some((provider, model)) => format!("模型：{model}（{provider}）"),
                None => format!("模型：{label}"),
            });
        if groups.is_empty() {
            return trigger.disabled(true).into_any_element();
        }

        let haloeth = cx.weak_entity();
        trigger
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.scrollable(true);
                for (provider, models) in &groups {
                    menu = menu.item(PopupMenuItem::label(provider.clone()));
                    for model in models {
                        let haloeth = haloeth.clone();
                        let provider = provider.clone();
                        let id = model.clone();
                        let checked = current
                            .as_ref()
                            .is_some_and(|(current, entry)| current == &provider && entry == model);
                        menu =
                            menu.item(PopupMenuItem::new(model.clone()).checked(checked).on_click(
                                move |_, _, cx| {
                                    haloeth
                                        .update(cx, |this, cx| {
                                            this.select_model(&provider, &id, cx)
                                        })
                                        .ok();
                                },
                            ));
                    }
                }
                menu
            })
            .into_any_element()
    }

    /// 底部栏：左侧提交提示，右侧模型、会话状态、**Token** 用量、消息数与发送动作
    fn render_bottom_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.session.read(cx).status();
        let message_count = self.conversation.read(cx).message_count();
        let total_tokens = self.session.read(cx).usage().total();

        StatusBar::new()
            .border_t_2()
            .child(status_item("submit-hint", composer::SUBMIT_HINT))
            .right(self.render_model_picker(cx))
            .right(status_item("session-state", status.label()))
            .when(total_tokens > 0, |this| {
                this.right(status_item("token-usage", format_tokens(total_tokens)))
            })
            .right(status_item("message-count", format!("{message_count} 条消息")))
            .right(self.render_submit_action(status, cx))
    }

    /// 提交动作：运行中显示停止按钮，否则显示发送按钮
    fn render_submit_action(&self, status: Status, cx: &mut Context<Self>) -> impl IntoElement {
        if status.is_busy() {
            return Button::new("stop")
                .secondary()
                .small()
                .icon(IconName::CircleStop)
                .tooltip("停止")
                .accessibility_label("停止")
                .disabled(status == Status::Cancelling)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.session.update(cx, |session, cx| session.cancel(cx));
                }))
                .into_any_element();
        }

        Button::new("send")
            .primary()
            .small()
            .icon(IconName::Airplay)
            .tooltip("发送")
            .accessibility_label("发送")
            .disabled(!self.composer.read(cx).is_submittable())
            .on_click(cx.listener(|this, _, window, cx| {
                this.composer.update(cx, |composer, cx| composer.submit(window, cx));
            }))
            .into_any_element()
    }
}

/// 状态项：带状态角色与无障碍名称的文本
fn status_item(id: &'static str, text: impl Into<SharedString>) -> impl IntoElement {
    let label: SharedString = text.into();

    div().id(id).role(Role::Status).test_support().aria_label(label.clone()).child(label)
}

/// 格式化 **Token** 总量：超过一万时以千为单位展示
fn format_tokens(total: u32) -> String {
    if total < 10_000 {
        format!("{total} tokens")
    } else {
        format!("{:.1}k tokens", f64::from(total) / 1000.0)
    }
}

impl Render for Haloeth {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .on_action(cx.listener(Self::new_session))
            .on_action(cx.listener(Self::clear_session))
            .on_action(cx.listener(Self::show_models))
            .on_action(cx.listener(Self::show_about))
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
    use super::{Haloeth, WINDOW_MIN_SIZE, WINDOW_SIZE, format_tokens};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestAppContextExt as _;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Entity, Pixels, Role, Size, TestAppContext, Window,
        point, px,
    };
    use std::time::Duration;

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

        let haloeth = built.expect("the window build closure ran");
        // 断言不依赖开发者本机的配置文件：测试统一从空配置开始
        cx.update(|cx| {
            haloeth.update(cx, |haloeth, cx| {
                haloeth.session.update(cx, |session, cx| {
                    session.apply_config(crate::config::Config::default(), cx);
                });
            });
        });

        (haloeth, handle.into())
    }

    /// 为会话重置为空配置，再配置一家提供商及其模型
    fn configure(
        cx: &mut TestAppContext,
        haloeth: &Entity<Haloeth>,
        name: &str,
        api_key: &str,
        models: &[&str],
    ) {
        cx.update(|cx| {
            haloeth.update(cx, |haloeth, cx| {
                haloeth.session.update(cx, |session, cx| {
                    session.apply_config(crate::config::Config::default(), cx);
                });
            });
        });
        add_provider_with_models(cx, haloeth, name, api_key, models);
    }

    /// 在现有配置上追加一家提供商及其模型
    fn add_provider_with_models(
        cx: &mut TestAppContext,
        haloeth: &Entity<Haloeth>,
        name: &str,
        api_key: &str,
        models: &[&str],
    ) {
        let name = name.to_owned();
        let api_key = api_key.to_owned();
        let models: Vec<String> = models.iter().map(|model| (*model).to_owned()).collect();
        cx.update(|cx| {
            haloeth.update(cx, |haloeth, cx| {
                let mut config = haloeth.session.read(cx).config().clone();
                config
                    .add_provider(crate::config::Provider::new(
                        name.clone(),
                        "https://example.com/v1",
                        api_key,
                    ))
                    .expect("新增提供商应当成功");
                for model in models {
                    config.add_model(&name, &model);
                }
                haloeth.session.update(cx, |session, cx| session.apply_config(config, cx));
            });
        });
    }

    /// 打开模型对话框并等待它挂载完成
    async fn open_models_dialog(
        cx: &mut TestAppContext,
        haloeth: &Entity<Haloeth>,
        handle: AnyWindowHandle,
    ) {
        cx.update_window(handle, |_, window, cx| {
            haloeth.update(cx, |haloeth, cx| haloeth.show_models(&crate::ShowModels, window, cx));
        })
        .expect("the window is open");
        settle_dialog(cx, handle).await;
    }

    /// 等待对话框出现并等它结束入场动画
    ///
    /// 入场动画按墙上时间推进，未结束时元素位置仍在变化，
    /// 因此断言与点击都要等它落位。
    async fn settle_dialog(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window.try_find("dialog").is_some()
        })
        .await;
        std::thread::sleep(Duration::from_millis(200));
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("the window is open");
    }

    /// 等待对话框从界面树中移除
    async fn wait_for_dialog_close(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window.try_find("dialog").is_none()
        })
        .await;
    }

    /// 等待确认对话框出现并等它结束入场动画
    async fn settle_alert(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        cx.wait_for(handle, Duration::from_secs(1), |window, _| window.try_find("ok").is_some())
            .await;
        std::thread::sleep(Duration::from_millis(200));
        cx.update_window(handle, |_, window, cx| window.render_frame(cx))
            .expect("the window is open");
    }

    /// 在对话框中新增一家提供商，并填写名称、接口地址与密钥
    ///
    /// 字段在新增卡片后的下一个动作写入草稿，因此以「添加」触发一次提交，
    /// 空标识只会提示，不会产生副作用。
    fn add_provider(cx: &mut TestAppContext, handle: AnyWindowHandle, name: &str, api_key: &str) {
        let name = name.to_owned();
        let api_key = api_key.to_owned();
        cx.update_window(handle, |_, window, cx| {
            window.within("dialog").click("provider-add", cx);
            window.render_frame(cx);
            window.within("dialog").click("provider-name", cx);
            window.press("secondary-a", cx);
            window.within("dialog").input(&name, cx);
            assert_eq!(window.find("provider-name").value(), Some(name.as_str()), "输入后名称");
            window.within("dialog").click("provider-endpoint", cx);
            window.press("secondary-a", cx);
            window.within("dialog").input("https://api.test/v1", cx);
            window.within("dialog").click("provider-key", cx);
            window.press("secondary-a", cx);
            window.within("dialog").input(&api_key, cx);
            window.within("dialog").click("model-add:新提供商", cx);
            window.render_frame(cx);
            assert_eq!(window.find("provider-name").value(), Some(name.as_str()), "字段已写回草稿");
        })
        .expect("the window is open");
    }

    /// 在对话框中为指定提供商添加一个模型
    fn add_model(cx: &mut TestAppContext, handle: AnyWindowHandle, provider: &str, model: &str) {
        let model = model.to_owned();
        let button = format!("model-add:{provider}");
        cx.update_window(handle, |_, window, cx| {
            window.within("dialog").click("model-input", cx);
            window.within("dialog").input(&model, cx);
            window.within("dialog").click(button.clone(), cx);
            window.render_frame(cx);
        })
        .expect("the window is open");
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

    /// 流式文本拼接进同一条消息，步骤结束后开一条新消息
    #[gpui_kit::test]
    fn conversation_streams_text_into_one_message(cx: &mut TestAppContext) {
        let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);

        cx.update_window(handle, |_, window, cx| {
            haloeth.update(cx, |haloeth, cx| {
                haloeth.conversation.update(cx, |conversation, cx| {
                    conversation.stream(crate::conversation::ChatRole::Assistant, "你", cx);
                    conversation.stream(crate::conversation::ChatRole::Assistant, "好", cx);
                    conversation.end_stream();
                    conversation.stream(crate::conversation::ChatRole::Assistant, "再见", cx);
                });
            });
            window.render_frame(cx);

            assert_eq!(window.find(("message", 0_u64)).label(), Some("Haloeth：你好"));
            assert_eq!(window.find(("message", 1_u64)).label(), Some("Haloeth：再见"));
        })
        .unwrap();
    }

    /// Markdown 代码块渲染出复制动作，点击后代码进入剪贴板
    #[gpui_kit::test]
    fn markdown_code_blocks_copy_their_code(cx: &mut TestAppContext) {
        let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);

        cx.update_window(handle, |_, window, cx| {
            haloeth.update(cx, |haloeth, cx| {
                haloeth.conversation.update(cx, |conversation, cx| {
                    conversation.push(
                        crate::conversation::ChatRole::Assistant,
                        "```rust\nfn main() {}\n```",
                        cx,
                    );
                });
            });
            window.render_frame(cx);

            assert!(window.within("conversation").find("copy").visible(), "代码块带复制动作");
            window.click("copy", cx);
            window.render_frame(cx);
            assert_eq!(
                cx.read_from_clipboard().and_then(|item| item.text()),
                Some("fn main() {}".to_owned())
            );
        })
        .unwrap();
    }

    /// 其他线程发送的通道事件能送达界面执行器上的任务
    #[gpui_kit::test]
    fn runtime_events_reach_the_interface_executor(cx: &mut TestAppContext) {
        use std::cell::Cell;
        use std::rc::Rc;

        let (_, handle) = open_shell(cx, WINDOW_SIZE);
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<u32>();
        let received = Rc::new(Cell::new(0_u32));
        let sink = received.clone();

        cx.update_window(handle, |_, window, cx| {
            cx.spawn(async move |_cx| {
                while let Some(value) = receiver.recv().await {
                    sink.set(value);
                }
            })
            .detach();
            window.render_frame(cx);
        })
        .unwrap();

        std::thread::spawn(move || {
            let _ = sender.send(7);
        })
        .join()
        .expect("发送线程应当正常结束");
        cx.run_until_parked();

        assert_eq!(received.get(), 7);
    }

    /// **Token** 计数在万位以下使用原值，以上使用千位表示
    #[test]
    fn token_labels_switch_to_thousands() {
        assert_eq!(format_tokens(0), "0 tokens");
        assert_eq!(format_tokens(9_999), "9999 tokens");
        assert_eq!(format_tokens(10_000), "10.0k tokens");
        assert_eq!(format_tokens(12_500), "12.5k tokens");
    }

    /// 模型对话框：新增提供商与模型，行内删除模型
    #[gpui_kit::test]
    async fn models_dialog_adds_providers_and_models(cx: &mut TestAppContext) {
        let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
        open_models_dialog(cx, &haloeth, handle).await;

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.within("dialog").find("models-panel").visible());
            assert!(window.within("dialog").find("provider-add").visible());
            assert!(window.within("dialog").find("models-save").visible());
            assert!(window.within("dialog").find("models-cancel").visible());
        })
        .unwrap();

        add_provider(cx, handle, "local", "secret");
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            // 名称在失焦后写回草稿，卡片标题随之更新
            assert_eq!(
                window.find("provider-toggle:local").label(),
                Some("提供商：local（0 个模型）")
            );
            assert!(window.find("models-empty:local").visible(), "向尚无模型");
        })
        .unwrap();

        add_model(cx, handle, "local", "m-b");
        add_model(cx, handle, "local", "m-a");
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            // 目录按标识排序，卡片统计随之更新
            assert_eq!(
                window.find("provider-toggle:local").label(),
                Some("提供商：local（2 个模型）")
            );
            assert_eq!(window.find("model-select:local/m-a").label(), Some("使用模型「m-a」"));
            assert_eq!(window.find("model-select:local/m-b").label(), Some("使用模型「m-b」"));

            // 行内删除模型，其余行不受影响
            window.click("model-remove:local/m-b", cx);
            window.render_frame(cx);
            assert_eq!(
                window.find("provider-toggle:local").label(),
                Some("提供商：local（1 个模型）")
            );
            assert!(window.find("model-select:local/m-a").visible());

            window.click("models-cancel", cx);
        })
        .unwrap();
        wait_for_dialog_close(cx, handle).await;
    }

    /// 删除提供商需要确认，取消保留、确认移除
    #[gpui_kit::test]
    async fn models_dialog_removes_a_provider_after_confirmation(cx: &mut TestAppContext) {
        let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
        configure(cx, &haloeth, "local", "secret", &["m-a"]);
        open_models_dialog(cx, &haloeth, handle).await;

        // 取消确认时保留提供商
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.within("dialog").click("provider-remove:local", cx);
        })
        .unwrap();
        settle_alert(cx, handle).await;
        cx.update_window(handle, |_, window, cx| {
            window.click("cancel", cx);
            window.render_frame(cx);
            assert_eq!(
                window.find("provider-toggle:local").label(),
                Some("提供商：local（1 个模型）")
            );

            // 确认后提供商从草稿中移除
            window.within("dialog").click("provider-remove:local", cx);
        })
        .unwrap();
        settle_alert(cx, handle).await;
        cx.update_window(handle, |_, window, cx| {
            window.click("ok", cx);
            window.render_frame(cx);
            assert!(window.find("models-empty").visible(), "回到空状态");
        })
        .unwrap();
    }

    /// 面板内换选模型后，保存校验指向新模型所在提供商
    #[gpui_kit::test]
    async fn models_dialog_validates_before_saving(cx: &mut TestAppContext) {
        let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
        configure(cx, &haloeth, "ready", "secret", &["m-a"]);
        add_provider_with_models(cx, &haloeth, "keyless", "", &["m-b"]);
        open_models_dialog(cx, &haloeth, handle).await;

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            // 展开另一个提供商并选用它的模型
            window.within("dialog").click("provider-toggle:keyless", cx);
            window.render_frame(cx);
            window.within("dialog").click("model-select:keyless/m-b", cx);
            window.render_frame(cx);

            // 保存被拒：提示指向刚选中的模型所在提供商
            window.within("dialog").click("models-save", cx);
            window.render_frame(cx);
            assert_eq!(
                window.find("models-error").label(),
                Some("请为提供商「keyless」填写 API 密钥")
            );
            assert!(window.find("models-panel").visible());
        })
        .unwrap();
    }

    /// 拉取前先校验密钥，不发起网络请求
    #[gpui_kit::test]
    async fn models_dialog_requires_a_key_before_fetching(cx: &mut TestAppContext) {
        let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
        configure(cx, &haloeth, "keyless", "", &["m-a"]);
        open_models_dialog(cx, &haloeth, handle).await;

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.within("dialog").click("models-fetch:keyless", cx);
            window.render_frame(cx);
            assert_eq!(
                window.find("fetch-status:keyless").label(),
                Some("拉取失败：请先填写 API 密钥")
            );
        })
        .unwrap();
    }

    /// 提供商之间的分隔线与全局边框一致，为 2px
    #[gpui_kit::test]
    async fn provider_divider_matches_the_two_pixel_border_rule(cx: &mut TestAppContext) {
        let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
        configure(cx, &haloeth, "ready", "secret", &["m-a"]);
        add_provider_with_models(cx, &haloeth, "keyless", "", &["m-b"]);
        open_models_dialog(cx, &haloeth, handle).await;

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let divider = window.within("dialog").find("provider-divider:keyless");
            assert!(divider.visible());
            assert_eq!(divider.bounds().size.height, px(2.));
        })
        .unwrap();
    }

    /// 输入控件由外框绘制 2px 边框，字段总高保持组件默认值
    #[gpui_kit::test]
    async fn input_frame_draws_a_two_pixel_border(cx: &mut TestAppContext) {
        let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
        configure(cx, &haloeth, "local", "secret", &["m-a"]);
        open_models_dialog(cx, &haloeth, handle).await;

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let frame = window.within("dialog").find("input-frame:provider-endpoint").bounds();
            let input = window.within("dialog").find("provider-endpoint").bounds();

            // 外框每侧 2px；控件自身高 28px，两者之和即组件默认的 32px 字段高度
            assert_eq!(frame.size.width - input.size.width, px(4.));
            assert_eq!(frame.size.height - input.size.height, px(4.));
            assert_eq!(frame.size.height, px(32.));

            // 点在外框（控件之外）同样聚焦控件：聚焦态由外框边框改色表达
            window.within("dialog").click_at(
                "input-frame:provider-endpoint",
                point(px(1.), px(1.)),
                cx,
            );
            window.render_frame(cx);
            assert_eq!(window.within("dialog").find("provider-endpoint").focused(), Some(true));
        })
        .unwrap();
    }

    /// 关于对话框展示版本并可关闭
    #[gpui_kit::test]
    async fn about_dialog_shows_version_and_closes(cx: &mut TestAppContext) {
        let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);

        cx.update_window(handle, |_, window, cx| {
            haloeth.update(cx, |haloeth, cx| haloeth.show_about(&crate::ShowAbout, window, cx));
        })
        .unwrap();
        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window.try_find("about-close").is_some()
        })
        .await;

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("about-close").visible());
            window.click("about-close", cx);
        })
        .unwrap();
        wait_for_dialog_close(cx, handle).await;
    }

    /// 保存配置后会话立即采用新配置，且不触发网络请求
    #[gpui_kit::test]
    fn session_applies_configuration(cx: &mut TestAppContext) {
        let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);

        configure(cx, &haloeth, "local", "secret", &["m-a", "m-b"]);

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("model").label(), Some("模型：m-a（local）"));
            assert_eq!(window.find("session-state").label(), Some("就绪"));
        })
        .unwrap();

        cx.update(|cx| {
            let session = haloeth.read(cx).session.clone();
            let config = session.read(cx).config();
            assert_eq!(config.provider().map(|provider| provider.name.as_str()), Some("local"));
            assert_eq!(config.provider().expect("提供者已配置").api_key, "secret");
            assert_eq!(config.model(), Some("m-a"));
            assert_eq!(session.read(cx).status(), crate::session::Status::Idle);
        });
    }

    /// 没有模型时底部栏的模型选择器为空态且不可展开
    #[gpui_kit::test]
    fn model_picker_is_disabled_without_models(cx: &mut TestAppContext) {
        let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("model").label(), Some("模型：未选择模型"));
            window.click("model", cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(r#"popover:dropdown-menu:Name("model")"#).is_none());
        })
        .unwrap();

        configure(cx, &haloeth, "local", "secret", &["m-a"]);

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("model").label(), Some("模型：m-a（local）"));
            window.click("model", cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let menu = window.within(r#"popover:dropdown-menu:Name("model")"#);
            assert!(menu.find(gpui_kit::ElementId::Integer(0)).visible());
        })
        .unwrap();
    }

    /// 提交任务前先报告缺少的运行前提
    #[gpui_kit::test]
    fn submit_reports_missing_key_or_model(cx: &mut TestAppContext) {
        let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);

        cx.update_window(handle, |_, window, cx| {
            haloeth.update(cx, |haloeth, cx| {
                haloeth.session.update(cx, |session, cx| session.submit("任务".to_owned(), cx));
            });
            window.render_frame(cx);
            assert_eq!(window.find("session-state").label(), Some("未配置密钥"));
            assert_eq!(window.find("message-count").label(), Some("0 条消息"));
        })
        .unwrap();

        cx.update_window(handle, |_, window, cx| {
            haloeth.update(cx, |haloeth, cx| {
                let mut config = crate::config::Config::default();
                config
                    .add_provider(crate::config::Provider::new(
                        "local",
                        "https://example.com/v1",
                        "secret",
                    ))
                    .expect("新增提供商应当成功");
                haloeth.session.update(cx, |session, cx| session.apply_config(config, cx));
                haloeth.session.update(cx, |session, cx| session.submit("任务".to_owned(), cx));
            });
            window.render_frame(cx);
            assert_eq!(window.find("session-state").label(), Some("未选择模型"));
            assert_eq!(window.find("message-count").label(), Some("0 条消息"));
        })
        .unwrap();
    }

    /// 底部栏的模型菜单按提供商分组列出全部模型，选择其他提供商的模型即切换提供商
    #[gpui_kit::test]
    fn model_picker_groups_models_by_provider(cx: &mut TestAppContext) {
        let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
        configure(cx, &haloeth, "a", "", &["m-a"]);
        add_provider_with_models(cx, &haloeth, "b", "", &["m-b"]);

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("model").label(), Some("模型：m-a（a）"));
            window.click("model", cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let mut menu = window.within(r#"popover:dropdown-menu:Name("model")"#);
            // 分组标题与模型项交替出现
            assert_eq!(menu.find(gpui_kit::ElementId::Integer(0)).label(), Some("a"));
            assert_eq!(menu.find(gpui_kit::ElementId::Integer(1)).label(), Some("m-a"));
            assert_eq!(menu.find(gpui_kit::ElementId::Integer(2)).label(), Some("b"));
            assert_eq!(menu.find(gpui_kit::ElementId::Integer(3)).label(), Some("m-b"));
            menu.click(gpui_kit::ElementId::Integer(3), cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("model").label(), Some("模型：m-b（b）"));
            assert_bottom_bar_fits(window);
        })
        .unwrap();

        cx.update(|cx| {
            // 选择模型同时切换了提供商
            assert_eq!(haloeth.read(cx).session.read(cx).config().selection(), Some(("b", "m-b")));
        });
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

    /// 最小窗口尺寸下底部栏仍按阅读顺序排布，不重叠也不越界
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
