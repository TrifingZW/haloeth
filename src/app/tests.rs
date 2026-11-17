use super::{Haloeth, WINDOW_MIN_SIZE, WINDOW_SIZE, format_tokens};
use agent::error::ModelError;
use agent::message::{Message, StreamDelta, ToolCall};
use agent::model::{GenerateRequest, Model, ModelBuilder};
use gpui_kit::component::Root;
use gpui_kit::component::speech::SpeechEvent;
use gpui_kit::test::TestAppContextExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Entity, Pixels, Role, Size, TestAppContext, Window, point,
    px, size,
};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 按脚本返回回复的模型，用于离线端到端测试。
pub(super) struct ScriptedModel {
    replies: Mutex<VecDeque<Message>>,
}

impl ScriptedModel {
    pub(super) fn new(replies: Vec<Message>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
        }
    }
}

#[async_trait::async_trait]
impl Model for ScriptedModel {
    fn model_id(&self) -> &'static str {
        "scripted"
    }

    async fn generate(
        &self,
        _request: GenerateRequest,
        _on_delta: &mut (dyn FnMut(StreamDelta) + Send),
    ) -> Result<Message, ModelError> {
        self.replies.lock().expect("脚本不应被毒化").pop_front().ok_or_else(|| {
            ModelError::Transport {
                reason: "the script ran out of replies".to_owned(),
            }
        })
    }
}

/// 构造一次工具调用回复。
pub(super) fn tool_call(name: &str, arguments: serde_json::Value) -> Message {
    Message {
        role: agent::message::Role::Assistant,
        content: Vec::new(),
        tool_calls: vec![ToolCall {
            id: format!("call_{name}"),
            name: name.to_owned(),
            arguments,
        }],
        tool_call_id: None,
        usage: None,
    }
}

/// 构造最终答案回复。
pub(super) fn final_answer(value: serde_json::Value) -> Message {
    tool_call("final_answer", serde_json::json!({ "answer": value }))
}

/// 等待状态栏显示指定文本。
pub(super) async fn wait_for_status(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    status: &str,
) {
    let status = status.to_owned();
    cx.wait_for(handle, Duration::from_secs(5), move |window, _| {
        window
            .try_find("session-state")
            .and_then(|element| element.label().map(str::to_owned))
            .as_deref()
            == Some(status.as_str())
    })
    .await;
}

/// 打开真实外壳窗口，返回视图与窗口句柄
pub(super) fn open_shell(
    cx: &mut TestAppContext,
    size: Size<Pixels>,
) -> (Entity<Haloeth>, AnyWindowHandle) {
    cx.update(gpui_kit::init);

    let store = crate::session_store::SessionStore::new(
        std::env::temp_dir().join(format!("haloeth-ui-{}", uuid::Uuid::new_v4())),
    );
    let mut built = None;
    let handle = cx.open_window(size, |window, cx| {
        let haloeth = cx.new(|cx| {
            Haloeth::new_with_store(
                window,
                cx,
                store.clone(),
                crate::config::Config::default(),
                false,
            )
        });
        built = Some(haloeth.clone());
        Root::new(haloeth, window, cx)
    });

    let haloeth = built.expect("the window build closure ran");

    (haloeth, handle.into())
}

/// 为会话重置为空配置，再配置一家提供商及其模型
pub(super) fn configure(
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
        haloeth.update(cx, |haloeth, cx| haloeth.show_models(&super::ShowModels, window, cx));
    })
    .expect("the window is open");
    settle_dialog(cx, handle).await;
}

/// 等待偏好窗口出现并完成首帧。
pub(super) async fn settle_preferences(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.wait_for(handle, Duration::from_secs(1), |window, _| {
        window.try_find("preferences-window").is_some()
    })
    .await;
    std::thread::sleep(Duration::from_millis(200));
    cx.update_window(handle, |_, window, cx| window.render_frame(cx)).ok();
}

/// 读取当前偏好窗口句柄。
pub(super) fn preferences_handle(
    cx: &TestAppContext,
    haloeth: &Entity<Haloeth>,
) -> AnyWindowHandle {
    haloeth
        .read_with(cx, |haloeth, _| haloeth.preferences.as_ref().map(|(handle, _)| *handle))
        .expect("the preferences window should be open")
}

/// 等待对话框出现并等它结束入场动画
///
/// 入场动画按墙上时间推进，未结束时元素位置仍在变化，
/// 因此断言与点击都要等它落位。
async fn settle_dialog(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.wait_for(handle, Duration::from_secs(1), |window, _| window.try_find("dialog").is_some())
        .await;
    std::thread::sleep(Duration::from_millis(200));
    cx.update_window(handle, |_, window, cx| window.render_frame(cx)).expect("the window is open");
}

/// 等待对话框从界面树中移除
async fn wait_for_dialog_close(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.wait_for(handle, Duration::from_secs(1), |window, _| window.try_find("dialog").is_none())
        .await;
}

/// 等待确认对话框出现并等它结束入场动画
pub(super) async fn settle_alert(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.wait_for(handle, Duration::from_secs(1), |window, _| window.try_find("ok").is_some()).await;
    std::thread::sleep(Duration::from_millis(200));
    cx.update_window(handle, |_, window, cx| window.render_frame(cx)).expect("the window is open");
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

    for id in ["session-state", "message-count", "send"] {
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
                conversation.push(crate::ui::conversation::ChatRole::User, "列出当前目录", cx);
                conversation.push(
                    crate::ui::conversation::ChatRole::Assistant,
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
                conversation.stream(crate::ui::conversation::ChatRole::Assistant, "你", cx);
                conversation.stream(crate::ui::conversation::ChatRole::Assistant, "好", cx);
                conversation.end_stream();
                conversation.stream(crate::ui::conversation::ChatRole::Assistant, "再见", cx);
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
                    crate::ui::conversation::ChatRole::Assistant,
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
    let delivered = Rc::new(Cell::new(0_u32));
    let sink = delivered.clone();

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

    assert_eq!(delivered.get(), 7);
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
    let (haloeth, handle) = open_shell(cx, size(px(480.), px(1400.)));
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
        assert_eq!(window.find("provider-toggle:local").label(), Some("提供商：local（0 个模型）"));
        assert!(window.find("models-empty:local").visible(), "向尚无模型");
    })
    .unwrap();

    add_model(cx, handle, "local", "m-b");
    add_model(cx, handle, "local", "m-a");
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        // 目录按标识排序，卡片统计随之更新
        assert_eq!(window.find("provider-toggle:local").label(), Some("提供商：local（2 个模型）"));
        assert_eq!(window.find("model-enabled:local/m-a").label(), Some("已启用"));
        assert_eq!(window.find("model-enabled:local/m-b").label(), Some("已启用"));

        // 禁用模型后状态切换，但行仍保留
        window.click("model-enabled:local/m-b", cx);
        window.render_frame(cx);
        assert_eq!(window.find("model-enabled:local/m-b").label(), Some("已停用"));

        // 行内删除模型，其余行不受影响
        window.click("model-remove:local/m-b", cx);
        window.render_frame(cx);
        assert_eq!(window.find("provider-toggle:local").label(), Some("提供商：local（1 个模型）"));
        assert!(window.find("model-enabled:local/m-a").visible());

        window.click("models-cancel", cx);
    })
    .unwrap();
    wait_for_dialog_close(cx, handle).await;
}

/// 删除提供商需要确认，取消保留、确认移除
#[gpui_kit::test]
async fn models_dialog_removes_a_provider_after_confirmation(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, size(px(480.), px(1400.)));
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
        assert_eq!(window.find("provider-toggle:local").label(), Some("提供商：local（1 个模型）"));

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

/// 当前模型所在提供商缺少密钥时，保存被拒并提示
#[gpui_kit::test]
async fn models_dialog_validates_before_saving(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    configure(cx, &haloeth, "keyless", "", &["m-a"]);
    open_models_dialog(cx, &haloeth, handle).await;

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.within("dialog").click("models-save", cx);
        window.render_frame(cx);
        assert_eq!(window.find("models-error").label(), Some("请为提供商「keyless」填写 API 密钥"));
        assert!(window.find("models-panel").visible());
    })
    .unwrap();
}

/// 拉取前先校验密钥，不发起网络请求
#[gpui_kit::test]
async fn models_dialog_requires_a_key_before_fetching(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, size(px(480.), px(1400.)));
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
    let (haloeth, handle) = open_shell(cx, size(px(480.), px(1400.)));
    configure(cx, &haloeth, "ready", "secret", &["m-a"]);
    add_provider_with_models(cx, &haloeth, "keyless", "", &["m-b"]);
    open_models_dialog(cx, &haloeth, handle).await;

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.within("dialog").scroll(
            "models-panel",
            gpui_kit::ScrollDelta::Lines(point(0., -20.)),
            cx,
        );
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
    let (haloeth, handle) = open_shell(cx, size(px(480.), px(1400.)));
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
        haloeth.update(cx, |haloeth, cx| haloeth.show_about(&super::ShowAbout, window, cx));
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

/// 没有模型时底部栏的模型选择器提示去配置
#[gpui_kit::test]
async fn model_picker_is_disabled_without_models(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        // 未配置模型时按钮不置灰，而是可点击并跳到模型配置。
        assert_eq!(window.find("model").label(), Some("模型：未配置，点击设置"));
        window.click("model", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.run_until_parked();
    let prefs = preferences_handle(cx, &haloeth);
    settle_preferences(cx, prefs).await;
    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("models-panel").visible());
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
        // 底部选择只影响当前会话，不覆盖通用里的默认模型
        assert_eq!(haloeth.read(cx).session.read(cx).selection(), Some(("b", "m-b")));
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
/// 空缓冲区拒绝发送；默认 Enter 发送、Shift+Enter 换行，发送按钮同样提交
/// 并且提交后写入对话并清空缓冲区
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

    // 默认偏好：Enter 直接发送
    cx.update_window(handle, |_, window, cx| {
        window.click(buffer_id, cx);
        window.input("列出当前目录", cx);
        window.press("enter", cx);
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

    // Shift+Enter 换行，不提交
    cx.update_window(handle, |_, window, cx| {
        window.click(buffer_id, cx);
        window.input("第一行", cx);
        window.press("shift-enter", cx);
        window.input("第二行", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(composer.read(cx).buffer().read(cx).value().as_ref(), "第一行\n第二行");
        assert_eq!(window.find("message-count").label(), Some("1 条消息"));
    })
    .unwrap();

    // 即使偏好是 Enter 发送，Ctrl+Enter 也可以提交
    cx.update_window(handle, |_, window, cx| {
        window.press("secondary-enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(("message", 1_u64)).label(), Some("你：第一行\n第二行"));
        assert!(composer.read(cx).buffer().read(cx).value().is_empty());
        assert_eq!(window.find("message-count").label(), Some("2 条消息"));
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
        assert_eq!(window.find(("message", 2_u64)).label(), Some("你：再删掉临时文件"));
        assert!(composer.read(cx).buffer().read(cx).value().is_empty());
        assert_eq!(window.find("message-count").label(), Some("3 条消息"));
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

/// 按键偏好切到 Ctrl+Enter 后，Enter 换行、Ctrl+Enter 发送。
#[gpui_kit::test]
fn composer_honours_the_send_key_preference(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    let composer = haloeth.read_with(cx, |haloeth, _cx| haloeth.composer.clone());
    let buffer_id =
        ("input", composer.read_with(cx, |composer, _cx| composer.buffer().entity_id()));

    cx.update_window(handle, |_, _, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.session.update(cx, |session, cx| {
                session.update_config(cx, |config| {
                    config.ui.send_key = crate::config::SendKey::SecondaryEnter;
                });
            });
            haloeth.sync_input_preferences(cx);
        });
    })
    .unwrap();

    cx.update_window(handle, |_, window, cx| {
        window.click(buffer_id, cx);
        window.input("用 Ctrl 发送", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(composer.read(cx).buffer().read(cx).value().as_ref(), "用 Ctrl 发送\n");
        assert!(window.find("conversation-empty").visible());
    })
    .unwrap();

    cx.update_window(handle, |_, window, cx| {
        window.press("secondary-enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find(("message", 0_u64)).label(), Some("你：用 Ctrl 发送"));
        assert_eq!(window.find("message-count").label(), Some("1 条消息"));
    })
    .unwrap();
}

/// 手动审核：工具调用先弹审批卡片，允许后执行并给出最终答案
#[gpui_kit::test]
async fn manual_approval_gates_a_tool_call(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    // 运行在独立的 Tokio 运行时上，允许测试在等待跨线程事件时暂停调度。
    cx.executor().allow_parking();
    configure(cx, &haloeth, "local", "secret", &["m"]);

    let replies = vec![
        tool_call("shell", serde_json::json!({ "command": "echo hi" })),
        final_answer(serde_json::json!("done")),
    ];
    let builder: ModelBuilder = Arc::new(move || Box::new(ScriptedModel::new(replies.clone())));
    cx.update(|cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.session.update(cx, |session, cx| {
                session.set_model_builder(builder, cx);
            });
        });
    });

    cx.update_window(handle, |_, _, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.session.update(cx, |session, cx| session.submit("run".to_owned(), cx));
        });
    })
    .unwrap();
    wait_for_status(cx, handle, "等待确认").await;

    let id = cx
        .update(|cx| haloeth.read(cx).conversation.read(cx).pending_approval(cx))
        .expect("a pending approval should be visible");

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(gpui_kit::ElementId::Name(format!("approval-allow:{id}").into()), cx);
    })
    .unwrap();
    wait_for_status(cx, handle, "就绪").await;

    cx.update(|cx| {
        let conversation = haloeth.read(cx).conversation.read(cx);
        let label = conversation.last_message_source().expect("the final answer should be visible");
        assert!(label.contains("最终答案"), "最终答案可见：{label}");
        assert!(label.contains("done"), "答案内容可见：{label}");
    });
}

/// 多次运行的用量累计，并按模型拆分。
#[gpui_kit::test]
async fn usage_accumulates_per_model(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    configure(cx, &haloeth, "local", "secret", &["m-a"]);
    let replies = vec![
        tool_call("final_answer", serde_json::json!({ "answer": "ok" }))
            .with_usage(agent::metrics::TokenUsage::new(10, 5)),
    ];
    let builder: ModelBuilder = Arc::new(move || Box::new(ScriptedModel::new(replies.clone())));
    cx.update(|cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.session.update(cx, |session, cx| {
                session.set_model_builder(builder, cx);
            });
        });
    });
    for _ in 0..2 {
        cx.update_window(handle, |_, _, cx| {
            haloeth.update(cx, |haloeth, cx| {
                haloeth.session.update(cx, |session, cx| session.submit("run".to_owned(), cx));
            });
        })
        .unwrap();
        wait_for_status(cx, handle, "就绪").await;
    }

    cx.update(|cx| {
        let session = haloeth.read(cx).session.read(cx);
        assert_eq!(session.usage().total(), 30);
        let per_model = session.usage_by_model();
        assert_eq!(per_model.get("m-a").map(|usage| usage.total()), Some(30));
    });
    // 重新打开会话后用量仍然保留
    let id = cx.update(|cx| haloeth.read(cx).active_id().to_owned());
    cx.update(|cx| haloeth.update(cx, Haloeth::save_active));
    let saved = cx.update(|cx| haloeth.read(cx).store.load(&id).expect("load"));
    assert_eq!(saved.usage.total(), 30);
    assert_eq!(saved.usage_by_model.get("m-a").map(|usage| usage.total()), Some(30));
}

/// 会话自动保存，并能在多个会话之间切换恢复
#[gpui_kit::test]
async fn sessions_persist_and_switch(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    configure(cx, &haloeth, "local", "secret", &["m"]);

    let builder: ModelBuilder =
        Arc::new(|| Box::new(ScriptedModel::new(vec![final_answer(serde_json::json!("done"))])));
    cx.update(|cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.session.update(cx, |session, cx| {
                session.set_model_builder(builder, cx);
            });
        });
    });

    cx.update_window(handle, |_, _, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.conversation.update(cx, |conversation, cx| {
                conversation.push(crate::ui::conversation::ChatRole::User, "第一件事", cx);
            });
            haloeth.session.update(cx, |session, cx| session.submit("第一件事".to_owned(), cx));
        });
    })
    .unwrap();
    wait_for_status(cx, handle, "就绪").await;

    let first = cx.update(|cx| haloeth.read(cx).active_id().to_owned());
    let saved =
        cx.update(|cx| haloeth.read(cx).store.load(&first)).expect("the session should be saved");
    assert!(saved.message_count() >= 2, "用户与最终答案都应落盘");
    assert_eq!(saved.title, "第一件事", "标题取自第一条用户消息");

    // 新建会话后对话为空
    cx.update(|cx| haloeth.update(cx, Haloeth::create_session));
    let second = cx.update(|cx| haloeth.read(cx).active_id().to_owned());
    assert_ne!(second, first);
    cx.update(|cx| {
        assert_eq!(haloeth.read(cx).conversation.read(cx).message_count(), 0);
    });

    // 切回第一个会话，记录与上下文都恢复
    cx.update(|cx| haloeth.update(cx, |haloeth, cx| haloeth.open_session(&first, cx)));
    cx.update(|cx| {
        let haloeth = haloeth.read(cx);
        assert_eq!(haloeth.active_id(), first);
        assert!(haloeth.conversation.read(cx).message_count() >= 2);
        assert_eq!(haloeth.session.read(cx).context().steps.len(), 3);
    });
}

/// 会话对话框列出会话并提供删除动作
#[gpui_kit::test]
async fn sessions_dialog_lists_the_active_session(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    let id = cx.update(|cx| haloeth.read(cx).active_id().to_owned());

    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| haloeth.show_sessions(window, cx));
    })
    .unwrap();
    settle_dialog(cx, handle).await;

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let dialog = window.within("dialog");
        assert!(dialog.find("sessions-panel").visible());
        assert!(dialog.find("sessions-new").visible());
        assert!(
            dialog.find(gpui_kit::ElementId::Name(format!("session-delete:{id}").into())).visible()
        );
    })
    .unwrap();
}

/// 规则审核：授权目录内的读取自动放行，不弹出审批卡片
#[gpui_kit::test]
async fn rules_mode_allows_reads_inside_a_granted_directory(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    configure(cx, &haloeth, "local", "secret", &["m"]);

    let dir = std::env::temp_dir().join(format!("haloeth-grant-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("grant dir");
    // 临时目录可能以 8.3 短名返回；授权与路径审核都按规范化形式比较。
    let dir = agent::approval::canonicalize_loose(&dir);
    let file = dir.join("note.txt");
    std::fs::write(&file, "hello from the granted directory").expect("fixture");

    cx.update(|cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.session.update(cx, |session, cx| {
                let mut config = session.config().clone();
                config.approval.mode = agent::approval::ApprovalMode::Rules;
                session.apply_config(config, cx);
                session.set_grants(
                    vec![agent::approval::DirectoryGrant::new(&dir, agent::tool::Access::Read)],
                    cx,
                );
            });
        });
    });

    let replies = vec![
        tool_call("read_file", serde_json::json!({ "path": file.to_string_lossy() })),
        final_answer(serde_json::json!("read done")),
    ];
    let builder: ModelBuilder = Arc::new(move || Box::new(ScriptedModel::new(replies.clone())));
    cx.update(|cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.session.update(cx, |session, cx| {
                session.set_model_builder(builder, cx);
            });
        });
    });

    cx.update_window(handle, |_, _, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.conversation.update(cx, |conversation, cx| {
                conversation.push(crate::ui::conversation::ChatRole::User, "读文件", cx);
            });
            haloeth.session.update(cx, |session, cx| session.submit("读文件".to_owned(), cx));
        });
    })
    .unwrap();
    wait_for_status(cx, handle, "就绪").await;

    cx.update(|cx| {
        let haloeth = haloeth.read(cx);
        let conversation = haloeth.conversation.read(cx);
        assert_eq!(
            conversation.activity_status("call_read_file", cx),
            Some(crate::ui::conversation::ActivityStatus::Ok),
            "授权目录内的读取应自动放行并成功"
        );
        assert!(
            conversation.last_message_source().is_some_and(|text| text.contains("read done")),
            "最终答案应可见"
        );
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// 底部栏可以直接切换审核模式，并立即反映到配置。
#[gpui_kit::test]
fn approval_mode_switches_from_the_bottom_bar(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("approval-mode", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let mut menu = window.within(r#"popover:dropdown-menu:Name("approval-mode")"#);
        // 顺序：手动、规则、模型、关闭。
        menu.click(gpui_kit::ElementId::Integer(1), cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        let mode = haloeth.read(cx).session.read(cx).config().approval.mode;
        assert_eq!(mode, agent::approval::ApprovalMode::Rules);
    });
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(window.find("approval-mode").label(), Some("审核模式：规则自动"));
    })
    .unwrap();
}

/// 目录授权对话框可以直接增删当前会话的授权。
#[gpui_kit::test]
async fn access_dialog_adds_and_removes_directories(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    let dir = std::env::temp_dir().join(format!("haloeth-access-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("grant dir");
    // 对话框写入的授权经过规范化，这里的期望值保持一致。
    let dir = agent::approval::canonicalize_loose(&dir);

    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| haloeth.show_access(window, cx));
    })
    .unwrap();
    settle_dialog(cx, handle).await;

    let path = dir.to_string_lossy().to_string();
    cx.update_window(handle, |_, window, cx| {
        window.within("dialog").click("access-path", cx);
        window.within("dialog").input(&path, cx);
        window.within("dialog").click("access-add", cx);
        window.render_frame(cx);
    })
    .unwrap();

    cx.update(|cx| {
        let grants = haloeth.read(cx).session.read(cx).grants().to_vec();
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].path, dir);
    });

    cx.update_window(handle, |_, window, cx| {
        window.within("dialog").click("access-remove:0", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        assert!(haloeth.read(cx).session.read(cx).grants().is_empty());
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// 「⋯」菜单里的分类入口打开对应的设置分页。
#[gpui_kit::test]
async fn more_menu_opens_the_tool_settings(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("more", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let mut menu = window.within("more-menu");
        menu.click("more-tools", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let prefs = preferences_handle(cx, &haloeth);
    settle_preferences(cx, prefs).await;

    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("settings-panel").visible());
        assert!(window.try_find("feature-visible:tool:shell").is_some());
    })
    .unwrap();
}

/// 快速设置面板一次可以连续切换多个开关，只有点击别处才收起。
#[gpui_kit::test]
fn more_menu_stays_open_while_toggling(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("more", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("more-menu").visible());
        let mut menu = window.within("more-menu");
        menu.click("more-tool:shell", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        // 点击开关后面板保持展开，且状态已经切换。
        assert!(window.find("more-menu").visible());
        assert!(
            window.find("more-tool:shell").label().is_some_and(|label| label.contains("已停用"))
        );
    })
    .unwrap();
    cx.update(|cx| {
        assert!(
            !haloeth
                .read(cx)
                .session
                .read(cx)
                .feature_state(crate::config::FeatureKind::Tool, "shell",)
        );
    });

    // 点击面板之外收起：点对话区左上角，避免落在向上的面板上。
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click_at("conversation", point(px(10.), px(10.)), cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("more-menu").is_none());
    })
    .unwrap();
}

/// 连续关闭两个 Skill 时，第二个不能反过来把第一个打开。
#[gpui_kit::test]
fn toggling_two_skills_keeps_both_disabled(cx: &mut TestAppContext) {
    let root = std::env::temp_dir().join(format!("haloeth-skills-{}", uuid::Uuid::new_v4()));
    for name in ["alpha", "beta"] {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).expect("skill dir");
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {name}\n---\n\nBody.\n"),
        )
        .expect("skill file");
    }
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    let root_text = root.to_string_lossy().to_string();
    cx.update(|cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.session.update(cx, |session, cx| {
                let mut config = session.config().clone();
                config.skills.dirs = vec![root_text];
                for name in ["alpha", "beta"] {
                    config.feature_prefs.set(
                        crate::config::FeatureKind::Skill,
                        name,
                        crate::config::FeaturePref::new(false, true),
                    );
                }
                session.apply_config(config, cx);
            });
        });
    });

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("more", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.within("more-menu").click("more-skill:alpha", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.within("more-menu").click("more-skill:beta", cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        let session = haloeth.read(cx).session.read(cx);
        assert!(!session.feature_state(crate::config::FeatureKind::Skill, "alpha"));
        assert!(!session.feature_state(crate::config::FeatureKind::Skill, "beta"));
    });
    let _ = std::fs::remove_dir_all(&root);
}

/// 全局隐藏的功能不出现在快速面板里。
#[gpui_kit::test]
fn hidden_features_are_absent_from_the_quick_panel(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.update(|cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.session.update(cx, |session, cx| {
                let mut config = session.config().clone();
                config.feature_prefs.set(
                    crate::config::FeatureKind::Tool,
                    "shell",
                    crate::config::FeaturePref::new(true, false),
                );
                session.apply_config(config, cx);
            });
        });
    });

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("more", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.within("more-menu").try_find("more-tool:shell").is_none());
        assert!(window.within("more-menu").try_find("more-tool:read_file").is_some());
    })
    .unwrap();
}

/// 新会话使用偏好里配置的默认模型。
#[gpui_kit::test]
fn new_sessions_use_the_preferred_default_model(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    configure(cx, &haloeth, "local", "secret", &["m-a", "m-b"]);
    cx.update(|cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.session.update(cx, |session, cx| {
                session.update_config(cx, |config| {
                    let _ = config.select_model("local", "m-b");
                });
            });
            haloeth.conversation.update(cx, |conversation, cx| {
                conversation.push(crate::ui::conversation::ChatRole::User, "先做点事", cx);
            });
        });
    });
    cx.update_window(handle, |_, _, cx| {
        haloeth.update(cx, Haloeth::create_session);
    })
    .unwrap();
    cx.update(|cx| {
        assert_eq!(haloeth.read(cx).session.read(cx).selection(), Some(("local", "m-b")));
    });
}

/// 已经是空白会话时，再点「新对话」不会创建新文件。
#[gpui_kit::test]
fn creating_a_session_on_a_blank_conversation_is_a_noop(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    let before = cx.update(|cx| haloeth.read(cx).active_id().to_owned());

    cx.update_window(handle, |_, _, cx| {
        haloeth.update(cx, Haloeth::create_session);
    })
    .unwrap();
    cx.update(|cx| {
        assert_eq!(haloeth.read(cx).active_id(), before);
    });
}

/// 审核模型可以从「当前对话模型」切到「单独指定」，并且切换会立即保存。
#[gpui_kit::test]
async fn reviewer_model_can_be_switched_to_a_specific_model(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    configure(cx, &haloeth, "local", "secret", &["m-a"]);
    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.open_preferences(crate::settings::preferences::PrefSection::General, window, cx)
        });
    })
    .unwrap();
    let prefs = preferences_handle(cx, &haloeth);
    settle_preferences(cx, prefs).await;

    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        window.scroll("pref-scroll", gpui_kit::ScrollDelta::Lines(point(0., -30.)), cx);
        window.render_frame(cx);
        assert!(window.find("settings-reviewer").label().is_some_and(|l| l.contains("当前对话")));
        window.click("settings-reviewer", cx);
        window.render_frame(cx);
        // 切换后不会被自动保存逻辑拨回去：审核模型的按钮确实有反应。
        assert!(window.find("settings-reviewer").label().is_some_and(|l| l.contains("单独")));
        assert!(window.try_find("reviewer-model").is_some());
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        let reviewer = haloeth.read(cx).session.read(cx).config().approval.reviewer.clone();
        assert!(!reviewer.use_current);
    });
}

/// 偏好窗口在独立窗口中打开，左侧导航切换分页。
#[gpui_kit::test]
async fn preferences_window_opens_and_switches_sections(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.open_preferences(crate::settings::preferences::PrefSection::General, window, cx)
        });
    })
    .unwrap();
    let prefs = preferences_handle(cx, &haloeth);
    settle_preferences(cx, prefs).await;

    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("preferences-window").visible());
        assert!(window.find("pref-nav:models").visible());
        window.click("pref-nav:models", cx);
        window.render_frame(cx);
        assert!(window.find("models-panel").visible());
        window.click("pref-nav:tools", cx);
        window.render_frame(cx);
        assert!(window.find("settings-panel").visible());
    })
    .unwrap();
}

/// 按住说话得到的转写结果直接发送；普通麦克风输入只写进输入框。
#[gpui_kit::test]
fn hold_to_talk_submits_the_transcript(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);

    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.auto_submit_speech = true;
            haloeth.apply_speech_result(&SpeechEvent::Final("你好".into()), window, cx);
        });
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        let haloeth = haloeth.read(cx);
        assert_eq!(haloeth.composer.read(cx).buffer().read(cx).value(), "");
        assert_eq!(haloeth.conversation.read(cx).message_count(), 1);
        assert!(!haloeth.auto_submit_speech);
    });

    // 手动点麦克风时不自动发送，方便用户改完再发。
    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.apply_speech_result(&SpeechEvent::Final("再补充".into()), window, cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let haloeth = haloeth.read(cx);
        assert_eq!(haloeth.composer.read(cx).buffer().read(cx).value(), "再补充");
        assert_eq!(haloeth.conversation.read(cx).message_count(), 1);
    });
}

/// 运行中提交不会丢消息：文本留在输入框里，等这一轮结束后再发。
#[gpui_kit::test]
fn submitting_while_busy_keeps_the_text(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);

    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.session.update(cx, |session, cx| {
                session.set_status_for_test(crate::session::Status::Running, cx);
            });
            let composer = haloeth.composer.clone();
            composer.update(cx, |composer, cx| {
                composer.append_text("先排队", window, cx);
                composer.submit(window, cx);
            });
        });
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        let haloeth = haloeth.read(cx);
        assert_eq!(haloeth.composer.read(cx).buffer().read(cx).value(), "先排队");
        assert_eq!(haloeth.conversation.read(cx).message_count(), 0);
    });
}

/// 点击「录制」后按下的组合键会直接写入快捷键设置。
#[gpui_kit::test]
async fn hotkey_capture_records_the_pressed_combination(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.open_preferences(crate::settings::preferences::PrefSection::General, window, cx)
        });
    })
    .unwrap();
    let prefs = preferences_handle(cx, &haloeth);
    settle_preferences(cx, prefs).await;

    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        window.click("hotkey-capture", cx);
        window.render_frame(cx);
        window.press("ctrl-shift-k", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        let hotkey = haloeth.read(cx).session.read(cx).config().ui.voice_hotkey.clone();
        assert_eq!(hotkey, "ctrl+shift+k");
    });
}

/// 没有模型时默认模型显示「无」，点击跳到模型页。
#[gpui_kit::test]
async fn default_model_points_to_the_model_page(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.open_preferences(crate::settings::preferences::PrefSection::General, window, cx)
        });
    })
    .unwrap();
    let prefs = preferences_handle(cx, &haloeth);
    settle_preferences(cx, prefs).await;

    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("default-model").label().is_some_and(|label| label.contains("无")));
        haloeth.update(cx, |haloeth, cx| {
            haloeth.open_preferences(crate::settings::preferences::PrefSection::Models, window, cx)
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("models-panel").visible());
    })
    .unwrap();
}

/// 模型页只提供启用 / 禁用，不再改变当前选择。
#[gpui_kit::test]
async fn model_page_toggles_enabled_state(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    configure(cx, &haloeth, "local", "secret", &["m-a"]);
    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.open_preferences(crate::settings::preferences::PrefSection::Models, window, cx)
        });
    })
    .unwrap();
    let prefs = preferences_handle(cx, &haloeth);
    settle_preferences(cx, prefs).await;

    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        window.scroll("pref-scroll", gpui_kit::ScrollDelta::Lines(point(0., -30.)), cx);
        window.render_frame(cx);
        window.click("model-enabled:local/m-a", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let entry = haloeth
            .read(cx)
            .session
            .read(cx)
            .config()
            .model_entry("local", "m-a")
            .cloned()
            .expect("model");
        assert!(!entry.enabled);
    });
}

/// 未配置生成模型时，工具页显示警告而不是启用开关。
#[gpui_kit::test]
async fn generation_tools_need_a_model_before_enabling(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.open_preferences(crate::settings::preferences::PrefSection::Tools, window, cx)
        });
    })
    .unwrap();
    let prefs = preferences_handle(cx, &haloeth);
    settle_preferences(cx, prefs).await;
    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("media-unready:generate_image").is_some());
        assert!(window.try_find("feature-visible:tool:generate_image").is_none());
    })
    .unwrap();
}

/// 偏好里的默认审核模式即时写回配置。
#[gpui_kit::test]
async fn preferences_change_the_default_approval_mode(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.open_preferences(crate::settings::preferences::PrefSection::General, window, cx)
        });
    })
    .unwrap();
    let prefs = preferences_handle(cx, &haloeth);
    settle_preferences(cx, prefs).await;
    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        window.scroll("pref-scroll", gpui_kit::ScrollDelta::Lines(point(0., -30.)), cx);
        window.render_frame(cx);
        window.click("pref-mode:model", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            haloeth.read(cx).session.read(cx).config().approval.mode,
            agent::approval::ApprovalMode::Model
        );
    });
}

/// 行号开关即时生效并同步到输入框。
#[gpui_kit::test]
async fn line_numbers_can_be_toggled(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.open_preferences(crate::settings::preferences::PrefSection::General, window, cx)
        });
    })
    .unwrap();
    let prefs = preferences_handle(cx, &haloeth);
    settle_preferences(cx, prefs).await;

    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        window.click("settings-line-numbers", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(haloeth.read(cx).session.read(cx).config().ui.line_numbers);
        assert!(haloeth.read(cx).composer.read(cx).line_numbers());
    });
}

/// 偏好页面内容被限制在标题栏下方并可滚动。
#[gpui_kit::test]
async fn preferences_content_stays_below_the_header(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.open_preferences(crate::settings::preferences::PrefSection::Models, window, cx)
        });
    })
    .unwrap();
    let prefs = preferences_handle(cx, &haloeth);
    settle_preferences(cx, prefs).await;

    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        let header = window.find("pref-header").bounds();
        let scroll = window.find("pref-scroll").bounds();
        let root = window.find("preferences-window").bounds();
        assert!(scroll.top() >= header.bottom(), "内容不应盖住头部");
        assert!(scroll.bottom() <= root.bottom(), "内容不应越出窗口");
    })
    .unwrap();
}

/// 偏好左侧导航可以收起为图标。
#[gpui_kit::test]
async fn preferences_nav_collapses(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.open_preferences(crate::settings::preferences::PrefSection::General, window, cx)
        });
    })
    .unwrap();
    let prefs = preferences_handle(cx, &haloeth);
    settle_preferences(cx, prefs).await;

    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        let before = window.find("preferences-nav").bounds().size.width;
        window.click("pref-nav-toggle", cx);
        window.render_frame(cx);
        let after = window.find("preferences-nav").bounds().size.width;
        assert!(after < before, "导航应收起：{before:?} -> {after:?}");
        assert!(window.find("pref-nav:models").visible());
    })
    .unwrap();
}

/// 真实系统语音播报（会发出声音），手动运行。
#[test]
#[ignore = "plays audio through the system voice"]
fn live_system_tts_speaks() {
    let mut service = crate::audio::TtsService::new();
    service.speak(
        "Haloeth speech test",
        crate::audio::TtsSettings {
            backend: crate::config::AudioBackend::System,
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            voice: String::new(),
            rate: 0,
            volume: 40,
            endpoint: String::new(),
            headers: Default::default(),
        },
    );
    // 等播报自然结束，覆盖 SAPI 异步读取文本的生命周期。
    std::thread::sleep(std::time::Duration::from_secs(8));
    service.stop();
}

/// 工具输出里的用量会被计入按模型统计。
#[test]
fn tool_usage_blocks_are_detected() {
    let output = agent::tool::ToolOutput::Blocks(vec![
        agent::message::ContentBlock::text("Image saved to /tmp/a.png"),
        agent::message::ContentBlock::text(
            "__usage__{\"model\":\"img-model\",\"usage\":{\"input_tokens\":12,\"output_tokens\":34}}",
        ),
    ]);
    // 解析逻辑由 agent 侧覆盖；这里确认文本块格式可被解析。
    let text = match &output {
        agent::tool::ToolOutput::Blocks(blocks) => blocks
            .iter()
            .find_map(|block| match block {
                agent::message::ContentBlock::Text(text) => {
                    text.strip_prefix("__usage__").map(str::to_owned)
                }
                _ => None,
            })
            .expect("usage block"),
        agent::tool::ToolOutput::Json(_) => panic!("expected blocks"),
    };
    let value: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(value["model"], "img-model");
    assert_eq!(value["usage"]["output_tokens"], 34);
}

/// 真实云端 TTS 播报（会发出声音），手动运行。
#[test]
#[ignore = "plays cloud TTS audio"]
fn live_cloud_tts_plays() {
    let Some(key) = std::env::var_os("HALOETH_TEST_KEYS_FILE")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("阿里云千问平台通用：").map(str::to_owned))
        })
    else {
        return;
    };
    let mut service = crate::audio::TtsService::new();
    service.speak(
        "Haloeth cloud speech test",
        crate::audio::TtsSettings {
            backend: crate::config::AudioBackend::Cloud,
            base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1".into(),
            api_key: key.trim().to_owned(),
            model: "qwen3-tts-flash".into(),
            voice: "Cherry".into(),
            rate: 0,
            volume: 80,
            endpoint: String::new(),
            headers: Default::default(),
        },
    );
    std::thread::sleep(std::time::Duration::from_secs(20));
    service.stop();
}

/// 用量提示按模型列出 Token。
#[test]
fn usage_tooltip_lists_each_model() {
    let mut usage = std::collections::BTreeMap::new();
    usage.insert("deepseek-flash".to_owned(), agent::metrics::TokenUsage::new(1200, 300));
    usage.insert("glm-5.3-flash".to_owned(), agent::metrics::TokenUsage::new(400, 100));

    let text = super::usage_tooltip(&usage);

    assert!(text.contains("deepseek-flash：1500 tokens"));
    assert!(text.contains("glm-5.3-flash：500 tokens"));
    assert!(text.find("deepseek-flash").unwrap() < text.find("glm-5.3-flash").unwrap());
}

/// 偏好窗口的修改即时生效，无需保存按钮。
#[gpui_kit::test]
async fn preferences_apply_changes_immediately(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.open_preferences(crate::settings::preferences::PrefSection::General, window, cx)
        });
    })
    .unwrap();
    let prefs = preferences_handle(cx, &haloeth);
    settle_preferences(cx, prefs).await;

    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        window.click("theme:dark", cx);
        window.render_frame(cx);
        window.click("send-key:secondary-enter", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(gpui_kit::component::Theme::global(cx).mode.is_dark());
        assert_eq!(
            haloeth.read(cx).session.read(cx).config().ui.send_key,
            crate::config::SendKey::SecondaryEnter
        );
        assert_eq!(
            haloeth.read(cx).composer.read(cx).send_key(),
            crate::config::SendKey::SecondaryEnter
        );
    });
}

/// 拖动输入框上方的拖动条可以调整高度。
#[gpui_kit::test]
fn composer_can_be_resized_by_dragging(cx: &mut TestAppContext) {
    let (_haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let before = window.find("buffer").bounds().size.height;
        window.drag_to("composer-resize", "conversation", cx);
        window.render_frame(cx);
        let after = window.find("buffer").bounds().size.height;
        assert!(after > before, "拖动后应更高：{before:?} -> {after:?}");
    })
    .unwrap();
}

/// 点击对话里的图片会打开独立查看窗口。
#[gpui_kit::test]
async fn clicking_an_image_opens_the_viewer(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    let path = std::env::temp_dir().join("haloeth-click-image.png");
    let image = image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 255, 255]));
    image.save(&path).expect("image");
    cx.update(|cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.conversation.update(cx, |conversation, cx| {
                conversation.push_images(&[path.display().to_string()], cx);
            });
        });
    });
    let before = cx.update(|cx| cx.windows().len());
    let target = gpui_kit::ElementId::Name(format!("image-open:{}", path.display()).into());
    for _ in 0..20 {
        cx.update_window(handle, |_, window, cx| window.render_frame(cx)).ok();
        let visible = cx
            .update_window(handle, |_, window, _| {
                window.try_find(target.clone()).is_some_and(|element| element.visible())
            })
            .unwrap_or(false);
        if visible {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(target.clone(), cx);
    })
    .unwrap();
    cx.run_until_parked();
    let windows = cx.update(|cx| cx.windows());
    assert!(windows.len() > before, "应打开图片窗口");
    let viewer = windows.into_iter().find(|candidate| *candidate != handle).expect("viewer window");
    cx.wait_for(viewer, Duration::from_secs(1), |window, _| {
        window.try_find("image-viewer").is_some()
    })
    .await;
    let _ = std::fs::remove_file(path);
}

/// 无模型时点击麦克风 / 播报按钮跳到偏好设置。
#[gpui_kit::test]
async fn audio_buttons_open_preferences_when_unconfigured(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("mic", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let prefs = preferences_handle(cx, &haloeth);
    settle_preferences(cx, prefs).await;
    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("preferences-window").visible());
    })
    .unwrap();
}

/// 无模型时点击播报按钮同样跳到偏好设置。
#[gpui_kit::test]
async fn speaker_button_opens_preferences_when_unconfigured(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("tts-toggle", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let prefs = preferences_handle(cx, &haloeth);
    settle_preferences(cx, prefs).await;
    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        window.click("pref-nav:audio", cx);
        window.render_frame(cx);
        assert!(window.find("asr-backend:system").visible());
        assert!(window.find("tts-backend:system").visible());
        assert!(window.find("asr-model").visible());
        assert!(window.find("tts-model").visible());
        assert!(window.find("asr-test").visible());
        assert!(window.find("tts-test").visible());
        assert!(window.find("asr-device").visible());
    })
    .unwrap();
}

/// 工具页展示生成工具的模型选择。
#[gpui_kit::test]
async fn tools_page_lists_generation_models(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.open_preferences(crate::settings::preferences::PrefSection::Tools, window, cx)
        });
    })
    .unwrap();
    let prefs = preferences_handle(cx, &haloeth);
    settle_preferences(cx, prefs).await;
    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("media-model:generate_image").is_some());
        assert!(window.try_find("media-model:generate_video").is_some());
        assert!(window.try_find("media-model:generate_music").is_some());
        assert!(window.try_find("media-advanced:generate_image").is_some());
    })
    .unwrap();
}

/// 模型页的模态开关即时写回配置。
#[gpui_kit::test]
async fn model_modalities_apply_immediately(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    configure(cx, &haloeth, "local", "secret", &["m-a"]);
    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.open_preferences(crate::settings::preferences::PrefSection::Models, window, cx)
        });
    })
    .unwrap();
    let prefs = preferences_handle(cx, &haloeth);
    settle_preferences(cx, prefs).await;

    cx.update_window(prefs, |_, window, cx| {
        window.render_frame(cx);
        window.scroll("pref-scroll", gpui_kit::ScrollDelta::Lines(point(0., -30.)), cx);
        window.render_frame(cx);
        window.click("model-in:local/m-a:image", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();

    cx.update(|cx| {
        let entry = haloeth
            .read(cx)
            .session
            .read(cx)
            .config()
            .model_entry("local", "m-a")
            .cloned()
            .expect("model");
        assert!(entry.inputs().contains(&agent::model::Modality::Image));
    });
}

/// 主题设置立即切换到深色。
#[gpui_kit::test]
async fn settings_switches_the_color_scheme(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.open_settings(crate::settings::SettingsTab::General, window, cx)
        });
    })
    .unwrap();
    settle_dialog(cx, handle).await;

    cx.update_window(handle, |_, window, cx| {
        window.within("dialog").click("theme:dark", cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        assert!(gpui_kit::component::Theme::global(cx).mode.is_dark());
    });
}

/// 设置对话框展示各能力分区
#[gpui_kit::test]
async fn settings_dialog_shows_capability_sections(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);

    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.show_settings(&super::ShowSettings, window, cx);
        });
    })
    .unwrap();
    settle_dialog(cx, handle).await;

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        {
            let mut dialog = window.within("dialog");
            assert!(dialog.find("settings-panel").visible());
            assert!(dialog.find("settings-save").visible());
            // 通用分页：外观与审核模型。
            assert!(dialog.find("theme:dark").visible());
            // 新增的通用项把审核移到首屏下方，存在即可。
            assert!(dialog.try_find("settings-reviewer").is_some());
            dialog.click("settings-tab:tools", cx);
        }
        window.render_frame(cx);
        {
            let mut dialog = window.within("dialog");
            // 分页内容可能超出首屏，存在即可。
            assert!(dialog.try_find("feature-visible:tool:shell").is_some());
            assert!(dialog.try_find("settings-search").is_some());
            dialog.click("settings-tab:mcp", cx);
        }
        window.render_frame(cx);
        window.within("dialog").click("settings-tab:skill", cx);
        window.render_frame(cx);
        let dialog = window.within("dialog");
        assert!(dialog.try_find("skill-dir-add").is_some());
        assert!(dialog.try_find("skill-new").is_some());
    })
    .unwrap();
}

/// 设置页眉展示当前分页的说明，而不是固定的提示语。
#[gpui_kit::test]
async fn settings_describes_the_active_tab(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);

    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.open_settings(crate::settings::SettingsTab::General, window, cx)
        });
    })
    .unwrap();
    settle_dialog(cx, handle).await;

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("settings-description").label(),
            Some(crate::settings::SettingsTab::General.description())
        );
        window.within("dialog").click("settings-tab:mcp", cx);
        window.render_frame(cx);
        assert_eq!(
            window.find("settings-description").label(),
            Some(crate::settings::SettingsTab::Mcp.description())
        );
    })
    .unwrap();
}

/// 按键偏好保存后立即作用于输入缓冲区。
#[gpui_kit::test]
async fn settings_switch_the_send_key(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);

    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.open_settings(crate::settings::SettingsTab::General, window, cx)
        });
    })
    .unwrap();
    settle_dialog(cx, handle).await;

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.within("dialog").click("send-key:secondary-enter", cx);
        window.render_frame(cx);
        window.within("dialog").click("settings-save", cx);
    })
    .unwrap();
    wait_for_dialog_close(cx, handle).await;

    cx.update(|cx| {
        let haloeth = haloeth.read(cx);
        assert_eq!(
            haloeth.session.read(cx).config().ui.send_key,
            crate::config::SendKey::SecondaryEnter
        );
        assert_eq!(haloeth.composer.read(cx).send_key(), crate::config::SendKey::SecondaryEnter);
    });
}

/// 「新建 MCP 服务器」在独立对话框里收集字段，保存后回到设置列表。
#[gpui_kit::test]
async fn mcp_form_creates_a_server(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);

    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.open_settings(crate::settings::SettingsTab::Mcp, window, cx)
        });
    })
    .unwrap();
    settle_dialog(cx, handle).await;

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.within("dialog").click("mcp-new", cx);
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(1), |window, _| {
        window.try_find("mcp-form-panel").is_some()
    })
    .await;
    std::thread::sleep(Duration::from_millis(200));

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("mcp-form-name", cx);
        window.input("demo", cx);
        window.click("mcp-form-command", cx);
        window.input("npx", cx);
        window.render_frame(cx);
        window.click("mcp-form-save", cx);
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(1), |window, _| {
        window.try_find("mcp-form-panel").is_none()
    })
    .await;
    std::thread::sleep(Duration::from_millis(200));

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("feature-visible:mcp:demo").is_some());
        window.within("dialog").click("settings-save", cx);
    })
    .unwrap();
    wait_for_dialog_close(cx, handle).await;

    cx.update(|cx| {
        let servers = &haloeth.read(cx).session.read(cx).config().mcp_servers;
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "demo");
        assert!(matches!(
            &servers[0].transport,
            agent::mcp::McpTransport::Stdio { command, .. } if command == "npx"
        ));
    });
}

/// 「新建 Skill」在独立对话框里收集字段，并在所选根目录写入骨架。
#[gpui_kit::test]
async fn skill_form_creates_a_skill(cx: &mut TestAppContext) {
    let root = std::env::temp_dir().join(format!("haloeth-skill-form-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("skill root");
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    let root_text = root.to_string_lossy().to_string();
    cx.update(|cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.session.update(cx, |session, cx| {
                let mut config = session.config().clone();
                config.skills.dirs = vec![root_text];
                session.apply_config(config, cx);
            });
        });
    });

    cx.update_window(handle, |_, window, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.open_settings(crate::settings::SettingsTab::Skill, window, cx)
        });
    })
    .unwrap();
    settle_dialog(cx, handle).await;

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        // Skill 分页内容较长，先滚到底部再点「新建」。
        window.within("dialog").scroll(
            "settings-panel",
            gpui_kit::ScrollDelta::Lines(point(0., -30.)),
            cx,
        );
        window.render_frame(cx);
        window.within("dialog").click("skill-new", cx);
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(1), |window, _| {
        window.try_find("skill-form-panel").is_some()
    })
    .await;
    std::thread::sleep(Duration::from_millis(200));

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("skill-form-name", cx);
        window.input("demo-skill", cx);
        window.click("skill-form-description", cx);
        window.input("演示技能", cx);
        window.click("skill-form-when", cx);
        window.input("用户要求演示", cx);
        window.render_frame(cx);
        window.click("skill-form-save", cx);
    })
    .unwrap();
    cx.wait_for(handle, Duration::from_secs(1), |window, _| {
        window.try_find("skill-form-panel").is_none()
    })
    .await;
    std::thread::sleep(Duration::from_millis(200));

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("feature-visible:skill:demo-skill").is_some());
    })
    .unwrap();

    let text = std::fs::read_to_string(root.join("demo-skill").join("SKILL.md"))
        .expect("the skill file should be created");
    assert!(text.contains("when_to_use: 用户要求演示"), "{text}");
    let _ = std::fs::remove_dir_all(&root);
}

/// 手动审核：拒绝后工具不执行，模型收到拒绝观测后结束
#[gpui_kit::test]
async fn manual_denial_skips_the_tool(cx: &mut TestAppContext) {
    let (haloeth, handle) = open_shell(cx, WINDOW_SIZE);
    // 运行在独立的 Tokio 运行时上，允许测试在等待跨线程事件时暂停调度。
    cx.executor().allow_parking();
    configure(cx, &haloeth, "local", "secret", &["m"]);

    let replies = vec![
        tool_call("shell", serde_json::json!({ "command": "echo hi" })),
        final_answer(serde_json::json!("stopped")),
    ];
    let builder: ModelBuilder = Arc::new(move || Box::new(ScriptedModel::new(replies.clone())));
    cx.update(|cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.session.update(cx, |session, cx| {
                session.set_model_builder(builder, cx);
            });
        });
    });

    cx.update_window(handle, |_, _, cx| {
        haloeth.update(cx, |haloeth, cx| {
            haloeth.session.update(cx, |session, cx| session.submit("run".to_owned(), cx));
        });
    })
    .unwrap();
    wait_for_status(cx, handle, "等待确认").await;

    let id = cx
        .update(|cx| haloeth.read(cx).conversation.read(cx).pending_approval(cx))
        .expect("a pending approval should be visible");

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(gpui_kit::ElementId::Name(format!("approval-deny:{id}").into()), cx);
    })
    .unwrap();
    wait_for_status(cx, handle, "就绪").await;

    cx.update(|cx| {
        let conversation = haloeth.read(cx).conversation.read(cx);
        let label = conversation.last_message_source().expect("a final answer");
        assert!(label.contains("stopped"), "答案来自被拒绝后的观测：{label}");
    });
}
