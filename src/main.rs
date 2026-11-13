//! Haloeth 主视图：标题栏、对话区、输入缓冲区与状态栏的组合。
//!
//! 应用内所有边框统一 2px，一律使用 GPUI 的 `border_2` 系列缩放方法；
//! 组件自带的 1px 边框在调用处覆盖。

// Release 构建不附带控制台窗口；Debug 保留控制台，便于查看日志与调试输出。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod access;
mod audio;
mod background;
mod composer;
mod config;
mod conversation;
mod image_viewer;
mod logging;
mod mcp_form;
mod media_form;
mod models;
mod preferences;
mod session;
mod session_store;
mod sessions;
mod settings;
mod skill_form;
mod ui;

use access::AccessForm;
use audio::{ConfiguredRecognizer, TtsService, TtsSettings};
use composer::{Composer, ComposerEvent};
use conversation::{ChatRole, Conversation, ConversationEvent};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    assets::{AllAssets, IconName},
    base::{TestSupportExt as _, h_flex, v_flex},
    component::{
        ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable, TitleBar,
        WindowExt as _,
        button::{Button, ButtonVariants},
        dialog::{DialogDescription, DialogFooter},
        menu::{AppMenuBar, DropdownMenu, PopupMenuItem},
        popover::Popover,
        speech::{SpeechEvent, SpeechState},
        status_bar::StatusBar,
    },
    *,
};
use image_viewer::ImageViewer;
use models::ModelsForm;
use preferences::{PrefSection, PreferencesWindow};
use session::{Session, Status};
use session_store::{SessionFile, SessionStore};
use sessions::SessionsForm;
use settings::{SettingsForm, SettingsTab};
use ui::{Tone, chip, tinted};

actions!(
    haloeth,
    [
        NewSession,
        ClearSession,
        ShowSessions,
        ShowModels,
        ShowSettings,
        ShowPreferences,
        ShowAbout,
        Quit
    ]
);

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
    /// 会话目录
    store: SessionStore,
    /// 当前会话的文件元数据
    active: SessionFile,
    /// 标题栏展示的会话标题
    title: SharedString,
    /// 左下角快速设置面板是否展开
    more_open: bool,
    /// 偏好窗口；单实例，重复打开时聚焦已有窗口。
    preferences: Option<(AnyWindowHandle, WeakEntity<PreferencesWindow>)>,
    /// 语音输入状态。
    speech: Entity<SpeechState>,
    /// 语音播报服务。
    tts: TtsService,
    /// 是否自动播报最终回复。
    auto_speak: bool,
    /// 最近一次语音错误。
    audio_error: Option<SharedString>,
    /// 按住说话后自动发送（后台快捷键触发）。
    auto_submit_speech: bool,
    /// 托盘与全局快捷键。
    background: Option<background::Background>,
    /// 上一次观察到的运行状态，用于在终态触发保存
    last_status: Status,
    /// 主窗口句柄，托盘菜单用它恢复窗口。
    main_window: AnyWindowHandle,
    _composer_subscription: Subscription,
    _conversation_subscription: Subscription,
    _conversation_observation: Subscription,
    _composer_observation: Subscription,
    _session_observation: Subscription,
    _speech_subscription: Subscription,
    _speech_observation: Subscription,
    _appearance_subscription: Subscription,
}

impl Haloeth {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let config = config::Config::load();
        let store = SessionStore::new(config.sessions_root());
        Self::new_with_store(window, cx, store, config, true)
    }

    /// 以指定的会话目录与配置构造；测试用临时目录注入。
    fn new_with_store(
        window: &mut Window,
        cx: &mut Context<Self>,
        store: SessionStore,
        config: config::Config,
        persist_config: bool,
    ) -> Self {
        // 配色方案：跟随系统时读取窗口外观，系统切换时同步更新。
        ui::apply_theme(config.ui.theme, Some(window), cx);
        let conversation = cx.new(Conversation::new);
        let session = cx.new(|_| Session::new(config, conversation.clone()));
        session.update(cx, |session, cx| session.set_config_persistence(persist_config, cx));
        let send_key = session.read(cx).config().ui.send_key;
        let line_numbers = session.read(cx).config().ui.line_numbers;
        let composer = cx.new(|cx| Composer::new(window, send_key, line_numbers, cx));
        let speech = cx.new(|cx| {
            SpeechState::new(cx)
                .recognizer(ConfiguredRecognizer::new(session.downgrade()))
                .input(crate::audio::SelectableMicrophone::new(session.downgrade()))
                .system_fallback(false)
                // 云端转写可能要十几秒，给停止后的收尾留足时间。
                .stop_timeout(std::time::Duration::from_secs(60))
        });

        // 启动恢复：上次会话 → 最近更新的会话 → 新建。
        let active = Self::restore_session(&store, cx, &session, &conversation);

        let subscription = cx.subscribe_in(
            &composer,
            window,
            |this, _composer, event: &ComposerEvent, window, cx| {
                let ComposerEvent::Submit(text) = event;
                // 运行中不接受新消息：把文本放回输入框，避免用户看到消息已发出却没被处理。
                if this.session.read(cx).status().is_busy() {
                    this.composer.update(cx, |composer, cx| {
                        composer.append_text(text, window, cx);
                    });
                    return;
                }
                this.conversation.update(cx, |conversation, cx| {
                    conversation.push(ChatRole::User, text.clone(), cx);
                });
                this.session.update(cx, |session, cx| session.submit(text.to_string(), cx));
                this.save_active(cx);
            },
        );
        // 审批卡片上的按钮回到会话，由会话把决定交给挂起中的审批
        let conversation_subscription = cx.subscribe_in(
            &conversation,
            window,
            |this, _, event: &ConversationEvent, window, cx| match event {
                ConversationEvent::ApprovalDecision {
                    id,
                    decision,
                } => {
                    this.session.update(cx, |session, cx| {
                        session.resolve_approval(id, *decision, cx);
                    });
                    this.save_active(cx);
                }
                ConversationEvent::OpenImage {
                    path,
                    caption,
                } => this.open_image_viewer(path.clone(), caption.clone(), window, cx),
            },
        );
        // 底部栏的消息数、会话状态与发送按钮可用性跟随各自状态变化
        let observation = cx.observe(&conversation, |_, _, cx| cx.notify());
        let composer_observation = cx.observe(&composer, |_, _, cx| cx.notify());
        // 录音状态变化时刷新麦克风按钮。
        let speech_observation = cx.observe(&speech, |_, _, cx| cx.notify());
        // 语音识别结束后把文本追加到输入框；按住说话触发的识别直接发送。
        let speech_subscription =
            cx.subscribe_in(&speech, window, |this, _, event: &SpeechEvent, window, cx| {
                this.apply_speech_result(event, window, cx)
            });
        let appearance_subscription = cx.observe_window_appearance(window, |this, window, cx| {
            let preference = this.session.read(cx).config().ui.theme;
            if preference == config::ThemePreference::System {
                ui::apply_theme(preference, Some(window), cx);
            }
        });
        let session_observation = cx.observe(&session, |this, session, cx| {
            let status = session.read(cx).status();
            let previous = this.last_status;
            // 运行进入终态或等待审批时落盘，避免流式增量逐帧写文件。
            if status != previous {
                this.last_status = status;
                if matches!(status, Status::Idle | Status::Failed | Status::AwaitingApproval) {
                    this.save_active(cx);
                }
                if status == Status::Cancelling {
                    this.tts.stop();
                }
                // 自动播报：一轮运行结束后朗读最终回复。
                if status == Status::Idle
                    && previous.is_busy()
                    && this.auto_speak
                    && this.session.read(cx).last_run_ok()
                    && let Some(text) = this.conversation.read(cx).last_assistant_text()
                {
                    let text = text.to_owned();
                    let settings = this.tts_settings(cx);
                    this.tts.speak(text, settings);
                }
            }
            cx.notify();
        });

        // 呼出即输入：窗口就绪后把焦点交给输入缓冲区
        let buffer = composer.read(cx).buffer().clone();
        window.defer(cx, move |window, cx| {
            buffer.update(cx, |buffer, cx| buffer.focus(window, cx));
        });

        // 关闭主窗口时按偏好最小化到托盘，而不是退出应用。
        // GPUI 默认在最后一个窗口关闭后退出；必须拦截平台层的 WM_CLOSE。
        let this = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| {
            let keep_running = this
                .upgrade()
                .map(|this| this.read(cx).session.read(cx).config().ui.run_in_background)
                .unwrap_or(false);
            if keep_running {
                window.minimize_window();
                false
            } else {
                true
            }
        });

        // 托盘与全局快捷键只在真实运行中启动，避免测试创建系统托盘项。
        let background = persist_config.then(|| {
            let hotkey = background::Hotkey::parse(&session.read(cx).config().ui.voice_hotkey);
            background::Background::start(hotkey, store.root().to_path_buf())
        });
        if background.is_some() {
            let this = cx.weak_entity();
            let main_window = window.window_handle();
            // 事件处理必须发生在实体更新之外：在 `this.update` 里再调用
            // `window_handle.update` / `open_window` 会二次借用 App 并直接 panic
            // （托盘里的「偏好设置」就是这样把程序弄崩的）。
            cx.spawn(async move |_, cx| {
                loop {
                    cx.background_executor().timer(std::time::Duration::from_millis(120)).await;
                    let Ok(events) = this.update(cx, |this, _| this.take_background_events())
                    else {
                        break;
                    };
                    for event in events {
                        match event {
                            background::BackgroundEvent::Quit => {
                                let _ = this.update(cx, |_, cx| cx.quit());
                            }
                            background::BackgroundEvent::Show => {
                                let _ = cx.update_window(main_window, |_, window, _| {
                                    window.activate_window();
                                });
                            }
                            background::BackgroundEvent::ShowPreferences => {
                                let haloeth = this.clone();
                                let _ = cx.update_window(main_window, move |_, window, cx| {
                                    let _ = haloeth.update(cx, |this, cx| {
                                        this.open_preferences(PrefSection::General, window, cx)
                                    });
                                });
                            }
                            background::BackgroundEvent::OpenSession(id) => {
                                let haloeth = this.clone();
                                let _ = cx.update_window(main_window, move |_, window, cx| {
                                    let _ = haloeth.update(cx, |this, cx| {
                                        this.open_session(&id, cx);
                                        if this.active.id == id {
                                            window.activate_window();
                                        }
                                    });
                                });
                            }
                            background::BackgroundEvent::TalkStart => {
                                let _ = this.update(cx, |this, cx| this.start_hold_to_talk(cx));
                            }
                            background::BackgroundEvent::TalkStop => {
                                let _ = this.update(cx, |this, cx| this.stop_hold_to_talk(cx));
                            }
                        }
                    }
                }
            })
            .detach();
        }

        let initial_title = SharedString::from(active.title.clone());
        let mut this = Self {
            menu_bar: AppMenuBar::new(cx),
            conversation,
            composer,
            session,
            store,
            active,
            title: initial_title,
            more_open: false,
            preferences: None,
            speech,
            tts: TtsService::new(),
            auto_speak: false,
            audio_error: None,
            auto_submit_speech: false,
            background,
            main_window: window.window_handle(),
            last_status: Status::Idle,
            _composer_subscription: subscription,
            _conversation_subscription: conversation_subscription,
            _conversation_observation: observation,
            _composer_observation: composer_observation,
            _session_observation: session_observation,
            _speech_subscription: speech_subscription,
            _speech_observation: speech_observation,
            _appearance_subscription: appearance_subscription,
        };
        this.refresh_tray(cx);
        this
    }

    /// 启动时恢复会话：先尝试上次打开的会话，再退到最近更新的会话，最后新建。
    fn restore_session(
        store: &SessionStore,
        cx: &mut App,
        session: &Entity<Session>,
        conversation: &Entity<Conversation>,
    ) -> SessionFile {
        let last = session.read(cx).config().storage.last_session.clone();
        let restored = store
            .load(&last)
            .ok()
            .or_else(|| store.list().first().and_then(|summary| store.load(&summary.id).ok()));
        let mut file = match restored {
            Some(file) => file,
            None => {
                let (provider, model) = session
                    .read(cx)
                    .config()
                    .selection()
                    .map(|(provider, model)| (Some(provider.to_owned()), Some(model.to_owned())))
                    .unwrap_or((None, None));
                store.create(provider, model).unwrap_or_else(|_| SessionFile::new(None, None))
            }
        };
        file.suggest_title();
        conversation.update(cx, |conversation, cx| {
            conversation.restore(file.transcript.clone(), cx);
        });
        session.update(cx, |session, cx| {
            session.load_state(&file, cx);
            session.remember_session(&file.id, cx);
        });
        file
    }

    /// 保存当前会话：显示记录、执行上下文、授权与当前模型。
    pub(crate) fn save_active(&mut self, cx: &mut Context<Self>) {
        let transcript = self.conversation.read(cx).snapshot(cx);
        let (context, usage, usage_by_model, selection, grants, rules, features, last_session) = {
            let session = self.session.read(cx);
            (
                session.context().clone(),
                session.usage(),
                session.usage_by_model().clone(),
                session
                    .selection()
                    .map(|(provider, model)| (provider.to_owned(), model.to_owned())),
                session.grants().to_vec(),
                session.always_allow().to_vec(),
                session.feature_overrides().clone(),
                session.config().storage.last_session.clone(),
            )
        };
        self.active.transcript = transcript;
        self.active.context = context;
        self.active.usage = usage;
        self.active.usage_by_model = usage_by_model;
        self.active.grants = grants;
        self.active.rules = rules;
        self.active.features = features;
        if let Some((provider, model)) = selection {
            self.active.provider = Some(provider);
            self.active.model = Some(model);
        }
        self.active.suggest_title();
        self.title = SharedString::from(self.active.title.clone());
        if let Err(error) = self.store.save(&mut self.active) {
            tracing::warn!("保存会话失败：{error}");
            return;
        }
        // 托盘菜单里的「当前对话」跟随标题与会话列表变化。
        self.refresh_tray(cx);
        if last_session != self.active.id {
            self.session.update(cx, |session, cx| session.remember_session(&self.active.id, cx));
        }
    }

    /// 打开一个已有会话；运行中不允许切换。
    fn open_session(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.session.read(cx).status().is_busy() || id == self.active.id {
            return;
        }
        self.save_active(cx);
        let Ok(file) = self.store.load(id) else {
            return;
        };
        self.activate(file, cx);
    }

    /// 新建一个空会话，并把它设为当前会话。
    /// 新建会话；当前已经是空白会话时不做任何事。
    fn create_session(&mut self, cx: &mut Context<Self>) {
        if self.session.read(cx).status().is_busy() {
            return;
        }
        let blank = self.conversation.read(cx).message_count() == 0
            && self.session.read(cx).context().steps.is_empty();
        if blank {
            return;
        }
        self.save_active(cx);
        self.create_blank(cx);
    }

    /// 切换当前模型：提供商与模型一同确定
    fn select_model(&mut self, provider: &str, model: &str, cx: &mut Context<Self>) {
        self.session.update(cx, |session, cx| session.select_model(provider, model, cx));
    }

    /// 新会话：保存当前会话，再创建一个空的持久会话
    fn new_session(&mut self, _: &NewSession, _window: &mut Window, cx: &mut Context<Self>) {
        self.create_session(cx);
    }

    /// 清空会话：清空对话记录与代理记忆，保留会话标识、授权与模型
    fn clear_session(&mut self, _: &ClearSession, _window: &mut Window, cx: &mut Context<Self>) {
        self.conversation.update(cx, |conversation, cx| conversation.clear(cx));
        self.session.update(cx, |session, cx| session.clear_context(cx));
        self.save_active(cx);
    }

    /// 打开设置对话框，并定位到指定分页（测试与内部兼容入口）。
    #[allow(dead_code)]
    fn open_settings(&mut self, tab: SettingsTab, window: &mut Window, cx: &mut Context<Self>) {
        let config = self.session.read(cx).config().clone();
        let session = self.session.downgrade();
        let haloeth = cx.weak_entity();
        let form =
            cx.new(|cx| SettingsForm::new(window, &config, session, haloeth, tab, false, cx));

        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .border_2()
                .title("设置")
                .overlay_closable(false)
                .child(form.clone())
                .footer(form.update(cx, |form, cx| form.render_footer(cx).into_any_element()))
        });
    }

    /// 打开偏好窗口；已有窗口时聚焦并切换分区。
    pub(crate) fn open_preferences(
        &mut self,
        section: PrefSection,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((handle, view)) = &self.preferences {
            if let Some(view) = view.upgrade() {
                let switched = handle
                    .update(cx, |_, window, cx| {
                        view.update(cx, |view, cx| view.set_section(section, window, cx));
                        window.activate_window();
                    })
                    .is_ok();
                if !switched {
                    view.update(cx, |view, cx| view.set_section_quick(section, cx));
                }
                return;
            }
            self.preferences = None;
        }

        let session = self.session.downgrade();
        let haloeth = cx.weak_entity();
        let window_size = size(px(960.), px(640.));
        let options = WindowOptions {
            // 居中于主窗口所在的屏幕，而不是主窗口本身。
            window_bounds: Some(WindowBounds::centered(window_size, cx)),
            window_min_size: Some(size(px(720.), px(520.))),
            titlebar: Some(TitlebarOptions {
                title: Some("偏好设置".into()),
                ..TitleBar::title_bar_options()
            }),
            ..TitleBar::window_options()
        };
        let opened = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| PreferencesWindow::new(window, session, haloeth, section, cx))
        });
        if let Ok((handle, view)) = opened {
            self.preferences = Some((handle, view.downgrade()));
        }
    }

    /// 旧设置入口：保留对话框，供内部与测试使用。
    #[allow(dead_code)]
    fn show_settings(&mut self, _: &ShowSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(SettingsTab::General, window, cx);
    }

    /// 打开偏好窗口（通用分页）。
    fn show_preferences(
        &mut self,
        _: &ShowPreferences,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_preferences(PrefSection::General, window, cx);
    }

    /// 打开目录授权对话框。
    fn show_access(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.session.downgrade();
        let haloeth = cx.weak_entity();
        let form = cx.new(|cx| AccessForm::new(window, session, haloeth, cx));

        window.open_dialog(cx, move |dialog, _, _| {
            dialog.border_2().title("可访问目录").overlay_closable(false).child(form.clone())
        });
    }

    /// 修改配置并持久化；用于主界面的快速开关。
    fn update_config(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut config::Config)) {
        self.session.update(cx, |session, cx| session.update_config(cx, f));
    }

    /// 把当前的按键偏好同步到输入缓冲区；设置保存后调用。
    pub(crate) fn sync_input_preferences(&mut self, cx: &mut Context<Self>) {
        let (send_key, line_numbers, hotkey) = {
            let config = self.session.read(cx).config();
            (config.ui.send_key, config.ui.line_numbers, config.ui.voice_hotkey.clone())
        };
        self.composer.update(cx, |composer, cx| {
            composer.set_send_key(send_key, cx);
            composer.set_line_numbers(line_numbers, cx);
        });
        if let Some(background) = &self.background {
            background.set_hotkey(background::Hotkey::parse(&hotkey));
        }
    }

    /// 处理语音识别结果：把文本放进输入框；按住说话触发的结果直接发送。
    fn apply_speech_result(
        &mut self,
        event: &SpeechEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SpeechEvent::Final(text) => {
                let auto = std::mem::take(&mut self.auto_submit_speech);
                let text = text.trim().to_owned();
                if text.is_empty() {
                    cx.notify();
                    return;
                }
                self.composer.update(cx, |composer, cx| {
                    composer.append_text(&text, window, cx);
                });
                if auto {
                    self.composer.update(cx, |composer, cx| composer.submit(window, cx));
                }
            }
            SpeechEvent::Cancelled => {
                self.auto_submit_speech = false;
            }
            SpeechEvent::Error(error) => {
                self.auto_submit_speech = false;
                self.audio_error = Some(format!("语音识别失败：{error}").into());
                cx.notify();
            }
            _ => {}
        }
    }

    /// 展开或收起左下角的快速设置面板。
    pub(crate) fn set_more_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.more_open != open {
            self.more_open = open;
            cx.notify();
        }
    }

    /// 切换审核模式。
    fn set_approval_mode(&mut self, mode: agent::approval::ApprovalMode, cx: &mut Context<Self>) {
        self.update_config(cx, |config| config.approval.mode = mode);
    }

    /// 切换当前会话里的功能状态；全局配置不受影响。
    fn toggle_feature(&mut self, kind: config::FeatureKind, name: &str, cx: &mut Context<Self>) {
        let current = self.session.read(cx).feature_state(kind, name);
        self.session.update(cx, |session, cx| session.set_feature_state(kind, name, !current, cx));
        self.save_active(cx);
    }

    /// 在独立窗口中查看图片。
    fn open_image_viewer(
        &mut self,
        path: String,
        caption: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(900.), px(700.)), cx)),
            window_min_size: Some(size(px(480.), px(360.))),
            titlebar: Some(TitlebarOptions {
                title: Some("图片".into()),
                ..TitleBar::title_bar_options()
            }),
            ..TitleBar::window_options()
        };
        let title = caption.clone();
        let opened = gpui_kit::open_window(options, cx, move |_, cx| {
            cx.new(|_| ImageViewer::new(path.clone(), title.clone()))
        });
        if let Err(error) = opened {
            tracing::warn!("打开图片窗口失败：{error}");
        }
    }

    /// 打开会话对话框
    fn show_sessions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let active = self.active.id.clone();
        let haloeth = cx.weak_entity();
        let form = cx.new(|cx| SessionsForm::new(window, store, &active, haloeth, cx));

        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .border_2()
                .title("会话")
                .overlay_closable(false)
                .child(form.clone())
                .footer(form.update(cx, |form, cx| form.render_footer(cx).into_any_element()))
        });
    }

    /// 当前会话标识符
    pub(crate) fn active_id(&self) -> &str {
        &self.active.id
    }

    /// 应用一个已加载的会话文件。
    fn activate(&mut self, file: SessionFile, cx: &mut Context<Self>) {
        self.conversation.update(cx, |conversation, cx| {
            conversation.restore(file.transcript.clone(), cx);
        });
        self.session.update(cx, |session, cx| {
            session.load_state(&file, cx);
            session.remember_session(&file.id, cx);
        });
        self.title = SharedString::from(file.title.clone());
        self.active = file;
        self.refresh_tray(cx);
    }

    /// 新建空会话但不保存当前会话（删除当前会话时使用）。
    fn create_blank(&mut self, cx: &mut Context<Self>) {
        // 新会话使用通用设置里的默认模型；当前会话的临时选择不继承。
        let (provider, model) = self
            .session
            .read(cx)
            .config()
            .selection()
            .map(|(provider, model)| (Some(provider.to_owned()), Some(model.to_owned())))
            .unwrap_or((None, None));
        let Ok(file) = self.store.create(provider, model) else {
            return;
        };
        self.activate(file, cx);
    }

    /// 删除会话；删除的是当前会话时切到下一个，没有剩余则新建。
    pub(crate) fn delete_session(
        &mut self,
        id: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.session.read(cx).status().is_busy() {
            return Err("运行中不能删除会话".to_owned());
        }
        let was_active = id == self.active.id;
        self.store.delete(id)?;
        if was_active {
            match self.store.list().first().map(|summary| summary.id.clone()) {
                Some(next) => match self.store.load(&next) {
                    Ok(file) => self.activate(file, cx),
                    Err(_) => self.create_blank(cx),
                },
                None => self.create_blank(cx),
            }
        }
        Ok(())
    }

    /// 重命名当前会话。
    pub(crate) fn rename_session(
        &mut self,
        title: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let file = self.store.rename(&self.active.id, title)?;
        self.active.title = file.title.clone();
        self.title = SharedString::from(file.title);
        self.refresh_tray(cx);
        cx.notify();
        Ok(())
    }

    /// 旧模型入口：保留对话框，供内部与测试使用。
    #[allow(dead_code)]
    fn show_models(&mut self, _: &ShowModels, window: &mut Window, cx: &mut Context<Self>) {
        let config = self.session.read(cx).config().clone();
        let session = self.session.downgrade();
        let form = cx.new(|cx| ModelsForm::new(window, &config, session, false, cx));

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

    /// 取出后台线程攒下的事件；处理交给调用方在实体更新之外完成。
    fn take_background_events(&mut self) -> Vec<background::BackgroundEvent> {
        let Some(background) = self.background.as_ref() else {
            return Vec::new();
        };
        let mut events = Vec::new();
        while let Some(event) = background.next_event() {
            events.push(event);
        }
        events
    }

    /// 按住说话开始：记住要自动发送，并把窗口带到前台。
    fn start_hold_to_talk(&mut self, cx: &mut Context<Self>) {
        self.auto_submit_speech = true;
        if !self.speech.read(cx).is_available(cx) {
            self.auto_submit_speech = false;
            self.audio_error = Some("按住说话需要先配置语音识别".into());
            cx.notify();
            return;
        }
        self.speech.update(cx, |speech, cx| speech.start(cx));
        self.show_main_window(cx);
    }

    /// 按住说话结束：停止录音，转写结果由 [`Self::apply_speech_result`] 自动发送。
    fn stop_hold_to_talk(&mut self, cx: &mut Context<Self>) {
        self.speech.update(cx, |speech, cx| speech.stop(cx));
    }

    /// 把主窗口带到前台；最小化时平台会先还原。
    fn show_main_window(&mut self, cx: &mut Context<Self>) {
        let _ = self.main_window.update(cx, |_, window, _| window.activate_window());
    }

    /// 把当前会话标识与标题推给托盘菜单；会话列表由后台线程在展开菜单时读取。
    fn refresh_tray(&mut self, _cx: &mut Context<Self>) {
        let Some(background) = &self.background else {
            return;
        };
        background.set_tray(background::TrayState {
            current_id: self.active.id.clone(),
            current: self.title.to_string(),
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

    /// 当前会话按钮，点击打开会话对话框
    fn render_session_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        Button::new("session")
            .ghost()
            .small()
            .max_w(rems(16.))
            .label(self.title.clone())
            .tooltip("会话")
            .accessibility_label(format!("当前会话：{}", self.title))
            .on_click(cx.listener(|this, _, window, cx| this.show_sessions(window, cx)))
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
                    .menu_with_icon("会话…", IconName::FolderOpen, Box::new(ShowSessions))
                    .menu_with_icon("清空会话", IconName::Eraser, Box::new(ClearSession))
                    .separator()
                    .menu_with_icon("偏好设置…", IconName::Settings, Box::new(ShowPreferences))
                    .menu_with_icon("关于 Haloeth…", IconName::Info, Box::new(ShowAbout))
                    .separator()
                    .menu_with_icon("退出", IconName::LogOut, Box::new(Quit))
            })
    }

    /// 标题栏
    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let background = self.session.read(cx).config().ui.run_in_background;
        TitleBar::new()
            .border_b_2()
            .on_close_window(move |_, window, _| {
                if background {
                    // 后台运行：只最小化窗口，托盘仍在。
                    window.minimize_window();
                } else {
                    window.remove_window();
                }
            })
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .child(self.render_brand(cx))
                    .child(self.render_main_menu())
                    .child(self.render_session_button(cx))
                    .child(self.menu_bar.clone()),
            )
    }

    /// 模型切换：底部栏的下拉触发器，按提供商分组列出全部模型；没有模型时不可展开
    fn render_model_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let config = self.session.read(cx).config().clone();
        let current = self
            .session
            .read(cx)
            .selection()
            .map(|(provider, model)| (provider.to_owned(), model.to_owned()));
        // 对话主模型只列同时支持文本输入与文本输出的模型。
        let mut groups: Vec<(String, Vec<String>)> = Vec::new();
        for selection in
            config.models_with(&[agent::model::Modality::Text], &[agent::model::Modality::Text])
        {
            match groups.last_mut() {
                Some((provider, models)) if provider == &selection.provider => {
                    models.push(selection.model)
                }
                _ => groups.push((selection.provider, vec![selection.model])),
            }
        }
        let label = current
            .as_ref()
            .map(|(_, model)| model.clone())
            .unwrap_or_else(|| "未选择模型".to_owned());
        let trigger = Button::new("model")
            .ghost()
            .small()
            .max_w(rems(10.))
            .label(label.clone())
            .tooltip("切换模型")
            .accessibility_label(match &current {
                Some((provider, model)) => format!("模型：{model}（{provider}）"),
                None => format!("模型：{label}"),
            });
        if groups.is_empty() {
            // 没有可用模型时不置灰：用 warning 色 + 下划线提示可以点击去配置。
            return tinted(
                Button::new("model").outline().border_2().xsmall().underline(),
                Tone::Warning,
                cx,
            )
            .label("未配置模型 · 点击设置")
            .tooltip("还没有可用的对话模型，点击打开模型配置")
            .accessibility_label("模型：未配置，点击设置")
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_preferences(PrefSection::Models, window, cx)
            }))
            .into_any_element();
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

    /// 左下角「更多」：快速查看与切换工具、MCP、Skill，并进入对应设置。
    fn render_more_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let config = self.session.read(cx).config().clone();
        // 快速面板只列全局「显示」的功能，状态取当前会话。
        let tools = agent::tools::builtin_tools(&agent::tools::BuiltinOptions::default());
        let tool_rows: Vec<(String, bool)> = tools
            .iter()
            .map(|tool| tool.spec().name.clone())
            .chain([
                "web_search".to_owned(),
                agent::tools::media::GENERATE_IMAGE.to_owned(),
                agent::tools::media::GENERATE_VIDEO.to_owned(),
                agent::tools::media::GENERATE_MUSIC.to_owned(),
            ])
            .filter(|name| config.feature_prefs.is_visible(config::FeatureKind::Tool, name))
            .map(|name| {
                let enabled = self.session.read(cx).feature_state(config::FeatureKind::Tool, &name);
                (name, enabled)
            })
            .collect();
        let mcp_rows: Vec<(String, bool)> = config
            .mcp_servers
            .iter()
            .filter(|server| {
                config.feature_prefs.is_visible(config::FeatureKind::Mcp, &server.name)
            })
            .map(|server| {
                let enabled =
                    self.session.read(cx).feature_state(config::FeatureKind::Mcp, &server.name);
                (server.name.clone(), enabled)
            })
            .collect();
        let skill_rows: Vec<(String, bool)> = agent::skill::discover(&config.skill_dirs())
            .iter()
            .filter(|skill| {
                config.feature_prefs.is_visible(config::FeatureKind::Skill, &skill.name)
            })
            .map(|skill| {
                let enabled =
                    self.session.read(cx).feature_state(config::FeatureKind::Skill, &skill.name);
                (skill.name.clone(), enabled)
            })
            .collect();
        let tool_summary = format!(
            "工具 · {} / {} 启用 →",
            tool_rows.iter().filter(|(_, enabled)| *enabled).count(),
            tool_rows.len()
        );
        let mcp_summary = format!(
            "MCP · {} / {} 启用 →",
            mcp_rows.iter().filter(|(_, enabled)| *enabled).count(),
            mcp_rows.len()
        );
        let skill_summary = format!(
            "Skill · {} / {} 启用 →",
            skill_rows.iter().filter(|(_, enabled)| *enabled).count(),
            skill_rows.len()
        );
        let haloeth = cx.weak_entity();
        let open = self.more_open;
        let trigger = Button::new("more")
            .ghost()
            .small()
            .compact()
            .label("⋯")
            .tooltip("快速设置：工具、MCP、Skill 与目录")
            .accessibility_label("快速设置");

        Popover::new("more-popover")
            // 触发按钮贴在窗口左下角，面板向上展开。
            .anchor(Anchor::BottomLeft)
            .offset(px(6.))
            .open(open)
            .on_open_change({
                let haloeth = haloeth.clone();
                move |open, _, cx| {
                    haloeth.update(cx, |this, cx| this.set_more_open(*open, cx)).ok();
                }
            })
            .trigger(trigger)
            .w(rems(19.))
            .content(move |_, _, cx| {
                let divider = || div().h(px(1.)).w_full().bg(cx.theme().border);
                let mut menu = v_flex()
                    .id("more-menu")
                    .test_support()
                    .w_full()
                    .gap_1()
                    .max_h(rems(24.))
                    .overflow_y_scroll();

                menu = menu.child(more_section(
                    "more-tools",
                    tool_summary.clone(),
                    PrefSection::Tools,
                    haloeth.clone(),
                ));
                for (name, enabled) in &tool_rows {
                    let haloeth = haloeth.clone();
                    let id = name.clone();
                    menu = menu.child(more_toggle(
                        ElementId::Name(format!("more-tool:{name}").into()),
                        name,
                        *enabled,
                        move |cx| {
                            haloeth
                                .update(cx, |this, cx| {
                                    this.toggle_feature(config::FeatureKind::Tool, &id, cx)
                                })
                                .ok();
                        },
                        cx,
                    ));
                }

                menu = menu.child(divider()).child(more_section(
                    "more-mcp",
                    mcp_summary.clone(),
                    PrefSection::Mcp,
                    haloeth.clone(),
                ));
                for (name, enabled) in &mcp_rows {
                    let haloeth = haloeth.clone();
                    let id = name.clone();
                    menu = menu.child(more_toggle(
                        ElementId::Name(format!("more-mcp:{name}").into()),
                        name,
                        *enabled,
                        move |cx| {
                            haloeth
                                .update(cx, |this, cx| {
                                    this.toggle_feature(config::FeatureKind::Mcp, &id, cx)
                                })
                                .ok();
                        },
                        cx,
                    ));
                }

                menu = menu.child(divider()).child(more_section(
                    "more-skills",
                    skill_summary.clone(),
                    PrefSection::Skill,
                    haloeth.clone(),
                ));
                for (name, enabled) in &skill_rows {
                    let haloeth = haloeth.clone();
                    let id = name.clone();
                    menu = menu.child(more_toggle(
                        ElementId::Name(format!("more-skill:{name}").into()),
                        name,
                        *enabled,
                        move |cx| {
                            haloeth
                                .update(cx, |this, cx| {
                                    this.toggle_feature(config::FeatureKind::Skill, &id, cx)
                                })
                                .ok();
                        },
                        cx,
                    ));
                }

                menu.child(divider())
                    .child(
                        Button::new("more-access")
                            .outline()
                            .border_2()
                            .xsmall()
                            .w_full()
                            .label("目录授权…")
                            .on_click({
                                let haloeth = haloeth.clone();
                                move |_, window, cx| {
                                    haloeth
                                        .update(cx, |this, cx| {
                                            this.set_more_open(false, cx);
                                            this.show_access(window, cx);
                                        })
                                        .ok();
                                }
                            }),
                    )
                    .child(
                        Button::new("more-settings")
                            .outline()
                            .border_2()
                            .xsmall()
                            .w_full()
                            .label("偏好设置…")
                            .on_click({
                                let haloeth = haloeth.clone();
                                move |_, window, cx| {
                                    haloeth
                                        .update(cx, |this, cx| {
                                            this.set_more_open(false, cx);
                                            this.open_preferences(PrefSection::General, window, cx);
                                        })
                                        .ok();
                                }
                            }),
                    )
            })
            .into_any_element()
    }

    /// 审核模式快速切换。
    fn render_approval_chip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.session.read(cx).config().approval.mode;
        let tone = approval_tone(mode);
        let haloeth = cx.weak_entity();
        let trigger = tinted(Button::new("approval-mode").outline().border_2().xsmall(), tone, cx)
            .label(mode.label())
            .tooltip("审核模式：点击切换")
            .accessibility_label(format!("审核模式：{}", mode.label()));
        trigger.dropdown_caret(true).dropdown_menu(move |menu, _, _| {
            let mut menu = menu;
            for value in [
                agent::approval::ApprovalMode::Manual,
                agent::approval::ApprovalMode::Rules,
                agent::approval::ApprovalMode::Model,
                agent::approval::ApprovalMode::Off,
            ] {
                let haloeth = haloeth.clone();
                menu = menu.item(
                    PopupMenuItem::new(format!("{} · {}", value.label(), value.description()))
                        .checked(value == mode)
                        .on_click(move |_, _, cx| {
                            haloeth.update(cx, |this, cx| this.set_approval_mode(value, cx)).ok();
                        }),
                );
            }
            menu
        })
    }

    /// 当前会话的目录授权快速查看与入口。
    fn render_access_chip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let grants = self.session.read(cx).grants().to_vec();
        let tone = if grants.is_empty() {
            Tone::Warning
        } else {
            Tone::Success
        };
        let label = if grants.is_empty() {
            "未授权目录".to_owned()
        } else {
            format!("{} 个目录", grants.len())
        };
        let haloeth = cx.weak_entity();
        tinted(Button::new("access").outline().border_2().xsmall(), tone, cx)
            .tooltip("当前会话可访问的目录")
            .accessibility_label(format!("目录授权：{label}"))
            .label(label)
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu;
                for grant in &grants {
                    let access = match grant.access {
                        agent::tool::Access::Read => "只读",
                        agent::tool::Access::Write => "读写",
                    };
                    menu = menu.item(PopupMenuItem::label(format!(
                        "{}（{access}）",
                        grant.path.display()
                    )));
                }
                menu.item(PopupMenuItem::new("管理目录授权…").on_click({
                    let haloeth = haloeth.clone();
                    move |_, window, cx| {
                        haloeth.update(cx, |this, cx| this.show_access(window, cx)).ok();
                    }
                }))
            })
    }

    /// 底部栏：左侧快速设置，右侧模型、会话状态与发送动作
    /// 输入框上方的工具条：当前会话的快速设置与模型 / 语音。
    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .id("composer-toolbar")
            .test_support()
            .w_full()
            .items_center()
            .justify_between()
            .gap_2()
            .px_2()
            .py_1()
            .border_t_2()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .items_center()
                    .gap_1()
                    .child(self.render_more_menu(cx))
                    .child(self.render_approval_chip(cx))
                    .child(self.render_access_chip(cx)),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_1()
                    .child(self.render_model_picker(cx))
                    .when_some(self.audio_error.clone(), |this, error| {
                        this.child(
                            div()
                                .id("speech-error")
                                .role(Role::Status)
                                .test_support()
                                .text_xs()
                                .text_color(cx.theme().danger)
                                .max_w(rems(12.))
                                .truncate()
                                .child(error),
                        )
                    })
                    .child(self.render_audio_controls(cx)),
            )
    }

    /// 底部状态栏：运行状态、用量、消息数与发送动作。
    fn render_bottom_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.session.read(cx).status();
        let message_count = self.conversation.read(cx).message_count();
        let total_tokens = self.session.read(cx).usage().total();
        let usage_by_model = self.session.read(cx).usage_by_model().clone();

        StatusBar::new()
            .border_t_2()
            .child(chip(
                "session-state",
                self.session.read(cx).status_text(),
                status_tone(status),
                cx,
            ))
            .when(total_tokens > 0, |this| {
                this.right(status_item_with_tooltip(
                    "token-usage",
                    format_tokens(total_tokens),
                    usage_tooltip(&usage_by_model),
                ))
            })
            .right(status_item("message-count", format!("{message_count} 条消息")))
            .right(self.render_submit_action(status, cx))
    }

    /// 语音识别与播报按钮。
    fn render_audio_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let capturing = self.speech.read(cx).status().is_capturing();
        let mic = tinted(
            Button::new("mic").outline().border_2().small().icon(IconName::Mic),
            if capturing {
                Tone::Success
            } else {
                Tone::Neutral
            },
            cx,
        )
        .tooltip(if capturing {
            "停止录音"
        } else {
            "语音输入"
        })
        .accessibility_label("语音输入")
        .on_click(cx.listener(|this, _, window, cx| this.toggle_microphone(window, cx)));

        let speaker = tinted(
            Button::new("tts-toggle").outline().border_2().small().icon(if self.auto_speak {
                IconName::Pause
            } else {
                IconName::Play
            }),
            if self.auto_speak {
                Tone::Success
            } else {
                Tone::Neutral
            },
            cx,
        )
        .tooltip(if self.auto_speak {
            "关闭自动播报"
        } else {
            "自动播报回复"
        })
        .accessibility_label("自动播报")
        .on_click(cx.listener(|this, _, window, cx| this.toggle_speaker(window, cx)));

        h_flex().items_center().gap_1().child(mic).child(speaker)
    }

    /// 当前 TTS 配置快照。
    fn tts_settings(&self, cx: &App) -> TtsSettings {
        let session = self.session.read(cx);
        let config = session.config();
        let tts = &config.audio.tts;
        let (base_url, api_key) = config
            .provider_named(tts.provider.trim())
            .map(|provider| (provider.endpoint(), provider.api_key.clone()))
            .unwrap_or_default();
        TtsSettings {
            backend: tts.backend,
            base_url,
            api_key,
            model: tts.model.clone(),
            voice: tts.voice.clone(),
            rate: tts.rate,
            volume: tts.volume,
            endpoint: tts.endpoint.clone(),
            headers: config
                .provider_named(tts.provider.trim())
                .map(|provider| provider.headers.clone())
                .unwrap_or_default(),
        }
    }

    /// 切换录音；未配置时跳到偏好设置。
    fn toggle_microphone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.speech.read(cx).is_available(cx) {
            self.speech.update(cx, |speech, cx| speech.toggle(cx));
        } else {
            self.open_preferences(PrefSection::Audio, window, cx);
        }
    }

    /// 试听当前语音播报配置。
    pub(crate) fn preview_tts(&mut self, cx: &mut Context<Self>) {
        let settings = self.tts_settings(cx);
        self.tts.speak("Haloeth 语音播报测试", settings);
    }

    /// 切换自动播报；未配置时跳到偏好设置。
    fn toggle_speaker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.auto_speak {
            self.auto_speak = false;
            self.tts.stop();
            cx.notify();
            return;
        }
        let settings = self.tts_settings(cx);
        if !TtsService::is_configured(&settings) {
            self.open_preferences(PrefSection::Audio, window, cx);
            return;
        }
        self.auto_speak = true;
        cx.notify();
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

        let send_key = self.composer.read(cx).send_key();
        Button::new("send")
            .primary()
            .small()
            .icon(IconName::Airplay)
            .tooltip(composer::submit_hint(send_key))
            .accessibility_label("发送")
            .disabled(!self.composer.read(cx).is_submittable())
            .on_click(cx.listener(|this, _, window, cx| {
                this.composer.update(cx, |composer, cx| composer.submit(window, cx));
            }))
            .into_any_element()
    }
}

/// 快速设置面板的分类入口：点击关闭面板并打开对应设置分页。
fn more_section(
    id: &'static str,
    label: String,
    section: PrefSection,
    haloeth: WeakEntity<Haloeth>,
) -> AnyElement {
    Button::new(id)
        .outline()
        .border_2()
        .xsmall()
        .w_full()
        .label(label)
        .tooltip("打开对应的设置")
        .on_click(move |_, window, cx| {
            haloeth
                .update(cx, |this, cx| {
                    this.set_more_open(false, cx);
                    this.open_preferences(section, window, cx);
                })
                .ok();
        })
        .into_any_element()
}

/// 快速设置面板的开关行：点击只切换状态，不关闭面板。
fn more_toggle(
    id: ElementId,
    name: &str,
    enabled: bool,
    on_click: impl Fn(&mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let button = Button::new(id)
        .outline()
        .border_2()
        .xsmall()
        .w_full()
        .label(format!(
            "{name} · {}",
            if enabled {
                "已启用"
            } else {
                "已停用"
            }
        ))
        .selected(enabled)
        .toggled(enabled);
    tinted(
        button,
        if enabled {
            Tone::Success
        } else {
            Tone::Danger
        },
        cx,
    )
    .on_click(move |_, _, cx| on_click(cx))
    .into_any_element()
}

/// 运行状态对应的展示色调。
fn status_tone(status: Status) -> Tone {
    match status {
        Status::Idle => Tone::Neutral,
        Status::Preparing | Status::Running => Tone::Accent,
        Status::AwaitingApproval | Status::Cancelling => Tone::Warning,
        Status::Unconfigured | Status::Unselected => Tone::Warning,
        Status::Failed => Tone::Danger,
    }
}

/// 审核模式对应的展示色调。
fn approval_tone(mode: agent::approval::ApprovalMode) -> Tone {
    match mode {
        agent::approval::ApprovalMode::Manual => Tone::Warning,
        agent::approval::ApprovalMode::Rules => Tone::Success,
        agent::approval::ApprovalMode::Model => Tone::Accent,
        agent::approval::ApprovalMode::Off => Tone::Danger,
    }
}

/// 状态项：带状态角色与无障碍名称的文本
/// 按模型拆分的用量提示。
fn usage_tooltip(usage: &std::collections::BTreeMap<String, agent::metrics::TokenUsage>) -> String {
    if usage.is_empty() {
        return "还没有模型用量".to_owned();
    }
    let mut lines = vec!["各模型 Token 用量：".to_owned()];
    let mut entries: Vec<_> = usage.iter().collect();
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.1.total()));
    for (model, usage) in entries {
        lines.push(format!("{model}：{}", format_tokens(usage.total())));
    }
    lines.join(
        "
",
    )
}

fn status_item_with_tooltip(
    id: &'static str,
    text: impl Into<SharedString>,
    tooltip: String,
) -> impl IntoElement {
    let label: SharedString = text.into();
    div()
        .id(id)
        .role(Role::Status)
        .test_support()
        .aria_label(label.clone())
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
        })
        .child(label)
}

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
            .on_action(
                cx.listener(|this, _: &ShowSessions, window, cx| this.show_sessions(window, cx)),
            )
            .on_action(cx.listener(Self::show_preferences))
            .on_action(cx.listener(Self::show_about))
            .child(self.render_title_bar(cx))
            .child(self.conversation.clone())
            .child(self.render_toolbar(cx))
            .child(self.composer.clone())
            .child(self.render_bottom_bar(cx))
    }
}

fn main() {
    logging::init();
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
    use agent::error::ModelError;
    use agent::message::{Message, StreamDelta, ToolCall};
    use agent::model::{GenerateRequest, Model, ModelBuilder};
    use gpui_kit::component::Root;
    use gpui_kit::component::speech::SpeechEvent;
    use gpui_kit::test::TestAppContextExt as _;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Entity, Pixels, Role, Size, TestAppContext, Window,
        point, px, size,
    };
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// 按脚本返回回复的模型，用于离线端到端测试。
    struct ScriptedModel {
        replies: Mutex<VecDeque<Message>>,
    }

    impl ScriptedModel {
        fn new(replies: Vec<Message>) -> Self {
            Self {
                replies: Mutex::new(replies.into()),
            }
        }
    }

    #[async_trait::async_trait]
    impl Model for ScriptedModel {
        fn model_id(&self) -> &str {
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
    fn tool_call(name: &str, arguments: serde_json::Value) -> Message {
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
    fn final_answer(value: serde_json::Value) -> Message {
        tool_call("final_answer", serde_json::json!({ "answer": value }))
    }

    /// 等待状态栏显示指定文本。
    async fn wait_for_status(cx: &mut TestAppContext, handle: AnyWindowHandle, status: &str) {
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
    fn open_shell(
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

    /// 等待偏好窗口出现并完成首帧。
    async fn settle_preferences(cx: &mut TestAppContext, handle: AnyWindowHandle) {
        cx.wait_for(handle, Duration::from_secs(1), |window, _| {
            window.try_find("preferences-window").is_some()
        })
        .await;
        std::thread::sleep(Duration::from_millis(200));
        cx.update_window(handle, |_, window, cx| window.render_frame(cx)).ok();
    }

    /// 读取当前偏好窗口句柄。
    fn preferences_handle(cx: &TestAppContext, haloeth: &Entity<Haloeth>) -> AnyWindowHandle {
        haloeth
            .read_with(cx, |haloeth, _| haloeth.preferences.as_ref().map(|(handle, _)| *handle))
            .expect("the preferences window should be open")
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
            assert_eq!(window.find("model-enabled:local/m-a").label(), Some("已启用"));
            assert_eq!(window.find("model-enabled:local/m-b").label(), Some("已启用"));

            // 禁用模型后状态切换，但行仍保留
            window.click("model-enabled:local/m-b", cx);
            window.render_frame(cx);
            assert_eq!(window.find("model-enabled:local/m-b").label(), Some("已停用"));

            // 行内删除模型，其余行不受影响
            window.click("model-remove:local/m-b", cx);
            window.render_frame(cx);
            assert_eq!(
                window.find("provider-toggle:local").label(),
                Some("提供商：local（1 个模型）")
            );
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
            let label =
                conversation.last_message_source().expect("the final answer should be visible");
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
        cx.update(|cx| haloeth.update(cx, |haloeth, cx| haloeth.save_active(cx)));
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

        let builder: ModelBuilder = Arc::new(|| {
            Box::new(ScriptedModel::new(vec![final_answer(serde_json::json!("done"))]))
        });
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
                    conversation.push(crate::conversation::ChatRole::User, "第一件事", cx);
                });
                haloeth.session.update(cx, |session, cx| session.submit("第一件事".to_owned(), cx));
            });
        })
        .unwrap();
        wait_for_status(cx, handle, "就绪").await;

        let first = cx.update(|cx| haloeth.read(cx).active_id().to_owned());
        let saved = cx
            .update(|cx| haloeth.read(cx).store.load(&first))
            .expect("the session should be saved");
        assert!(saved.message_count() >= 2, "用户与最终答案都应落盘");
        assert_eq!(saved.title, "第一件事", "标题取自第一条用户消息");

        // 新建会话后对话为空
        cx.update(|cx| haloeth.update(cx, |haloeth, cx| haloeth.create_session(cx)));
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
                dialog
                    .find(gpui_kit::ElementId::Name(format!("session-delete:{id}").into()))
                    .visible()
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
                    conversation.push(crate::conversation::ChatRole::User, "读文件", cx);
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
                Some(crate::conversation::ActivityStatus::Ok),
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
                window
                    .find("more-tool:shell")
                    .label()
                    .is_some_and(|label| label.contains("已停用"))
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
                    conversation.push(crate::conversation::ChatRole::User, "先做点事", cx);
                });
            });
        });
        cx.update_window(handle, |_, _, cx| {
            haloeth.update(cx, |haloeth, cx| haloeth.create_session(cx));
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
            haloeth.update(cx, |haloeth, cx| haloeth.create_session(cx));
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
                haloeth.open_preferences(crate::preferences::PrefSection::General, window, cx)
            });
        })
        .unwrap();
        let prefs = preferences_handle(cx, &haloeth);
        settle_preferences(cx, prefs).await;

        cx.update_window(prefs, |_, window, cx| {
            window.render_frame(cx);
            window.scroll("pref-scroll", gpui_kit::ScrollDelta::Lines(point(0., -30.)), cx);
            window.render_frame(cx);
            assert!(
                window.find("settings-reviewer").label().is_some_and(|l| l.contains("当前对话"))
            );
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
                haloeth.open_preferences(crate::preferences::PrefSection::General, window, cx)
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
                haloeth.open_preferences(crate::preferences::PrefSection::General, window, cx)
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
                haloeth.open_preferences(crate::preferences::PrefSection::General, window, cx)
            });
        })
        .unwrap();
        let prefs = preferences_handle(cx, &haloeth);
        settle_preferences(cx, prefs).await;

        cx.update_window(prefs, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("default-model").label().is_some_and(|label| label.contains("无")));
            haloeth.update(cx, |haloeth, cx| {
                haloeth.open_preferences(crate::preferences::PrefSection::Models, window, cx)
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
                haloeth.open_preferences(crate::preferences::PrefSection::Models, window, cx)
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
                haloeth.open_preferences(crate::preferences::PrefSection::Tools, window, cx)
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
                haloeth.open_preferences(crate::preferences::PrefSection::General, window, cx)
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
                haloeth.open_preferences(crate::preferences::PrefSection::General, window, cx)
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
                haloeth.open_preferences(crate::preferences::PrefSection::Models, window, cx)
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
                haloeth.open_preferences(crate::preferences::PrefSection::General, window, cx)
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
            _ => panic!("expected blocks"),
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

        let text = crate::usage_tooltip(&usage);

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
                haloeth.open_preferences(crate::preferences::PrefSection::General, window, cx)
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
        let viewer =
            windows.into_iter().find(|candidate| *candidate != handle).expect("viewer window");
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
                haloeth.open_preferences(crate::preferences::PrefSection::Tools, window, cx)
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
                haloeth.open_preferences(crate::preferences::PrefSection::Models, window, cx)
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
                haloeth.show_settings(&crate::ShowSettings, window, cx);
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
            assert_eq!(
                haloeth.composer.read(cx).send_key(),
                crate::config::SendKey::SecondaryEnter
            );
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
        let root =
            std::env::temp_dir().join(format!("haloeth-skill-form-{}", uuid::Uuid::new_v4()));
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
}
