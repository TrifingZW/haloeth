//! 应用外壳：主窗口、标题栏、工具栏、状态栏与后台集成的装配。
//!
//! 只负责窗口级状态与动作分发；对话、输入区与配置界面由 `ui`、`settings` 模块提供。

mod access;
#[cfg(test)]
mod remote_tests;
mod sessions;
#[cfg(test)]
mod tests;

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

use crate::audio::{ConfiguredRecognizer, TtsService, TtsSettings};
use crate::remote::{BridgeStatus, RemoteBridge, RemoteCommand};
use crate::session::{Session, Status};
use crate::session_store::{SessionFile, SessionStore};
use crate::settings::models::ModelsForm;
use crate::settings::preferences::{PrefSection, PreferencesWindow};
use crate::settings::{SettingsForm, SettingsTab};
use crate::ui::composer::{self, Composer, ComposerEvent};
use crate::ui::conversation::{ChatRole, Conversation, ConversationEvent};
use crate::ui::image_viewer::ImageViewer;
use crate::ui::{self, Tone, chip, tinted};
use crate::{background, config};

use agent::model::Modality;
use remote::proto::notify::{MessageAppended, SessionsChanged, StateChanged};
use remote::proto::{
    AppStateDto, ChatRoleDto, Command, ErrorCode, FeatureDto, FeatureKindDto, GrantDto,
    ModelRefDto, Notification, RemoteError, ServerInfoDto, SessionSummaryDto, SnapshotDto,
    StreamingDto, TranscriptReset, WelcomeDto,
};

use self::access::AccessForm;
use self::sessions::SessionsForm;

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
pub(crate) struct Haloeth {
    menu_bar: Entity<AppMenuBar>,
    conversation: Entity<Conversation>,
    composer: Entity<Composer>,
    session: Entity<Session>,
    /// 远程桥：命令通道、通知投影与配对设备。
    remote: RemoteBridge,
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
        let (remote, remote_commands) = RemoteBridge::new();
        let conversation = cx.new(Conversation::new);
        let session = cx.new(|_| Session::new(config, conversation.clone()));
        session.update(cx, |session, cx| session.set_config_persistence(persist_config, cx));
        session.update(cx, |session, cx| session.set_remote_tap(Some(remote.tap()), cx));
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
                if !this.submit_message(text, cx) {
                    this.composer.update(cx, |composer, cx| {
                        composer.append_text(text, window, cx);
                    });
                }
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
                .is_some_and(|this| this.read(cx).session.read(cx).config().ui.run_in_background);
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
                                let _ = this.update(cx, Haloeth::start_hold_to_talk);
                            }
                            background::BackgroundEvent::TalkStop => {
                                let _ = this.update(cx, Haloeth::stop_hold_to_talk);
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
            remote,
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
        this.spawn_remote_pump(remote_commands, cx);
        let remote_config = this.session.read(cx).config().remote.clone();
        this.remote.sync(&remote_config, &crate::remote::certificate_dir(persist_config));
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
        let mut file = if let Some(file) = restored {
            file
        } else {
            let (provider, model) =
                session.read(cx).config().selection().map_or((None, None), |(provider, model)| {
                    (Some(provider.to_owned()), Some(model.to_owned()))
                });
            store.create(provider, model).unwrap_or_else(|_| SessionFile::new(None, None))
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
        self.conversation.update(cx, Conversation::clear);
        self.session.update(cx, Session::clear_context);
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
        self.publish_remote_reset(cx);
        self.publish_remote_state(cx);
        self.refresh_tray(cx);
    }

    /// 新建空会话但不保存当前会话（删除当前会话时使用）。
    fn create_blank(&mut self, cx: &mut Context<Self>) {
        // 新会话使用通用设置里的默认模型；当前会话的临时选择不继承。
        let (provider, model) =
            self.session.read(cx).config().selection().map_or((None, None), |(provider, model)| {
                (Some(provider.to_owned()), Some(model.to_owned()))
            });
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
        // 会话附件随之清理。
        let attachments =
            crate::remote::attachments_dir(self.session.read(cx).persists_config()).join(id);
        let _ = std::fs::remove_dir_all(attachments);
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
                    move |_, _, cx| form.update(cx, ModelsForm::save)
                })
        });
    }

    /// 打开关于对话框
    fn show_about(&mut self, _: &ShowAbout, window: &mut Window, cx: &mut Context<Self>) {
        let version = env!("CARGO_PKG_VERSION");
        let config_path = config::Config::path()
            .map_or_else(|| "未知".to_owned(), |path| path.display().to_string());

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
        self.speech.update(cx, SpeechState::start);
        self.show_main_window(cx);
    }

    /// 按住说话结束：停止录音，转写结果由 [`Self::apply_speech_result`] 自动发送。
    fn stop_hold_to_talk(&mut self, cx: &mut Context<Self>) {
        self.speech.update(cx, SpeechState::stop);
    }

    /// 把主窗口带到前台；最小化时平台会先还原。
    fn show_main_window(&mut self, cx: &mut Context<Self>) {
        let _ = self.main_window.update(cx, |_, window, _| window.activate_window());
    }

    /// 把当前会话标识与标题推给托盘，并同步远程端的会话列表。
    fn refresh_tray(&mut self, cx: &mut Context<Self>) {
        self.publish_remote_sessions(cx);
        let Some(background) = &self.background else {
            return;
        };
        background.set_tray(background::TrayState {
            current_id: self.active.id.clone(),
            current: self.title.to_string(),
        });
    }

    /// 提交一条用户消息；运行中返回 `false`，文本由调用方处理。
    pub(crate) fn submit_message(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        if self.session.read(cx).status().is_busy() {
            return false;
        }
        self.conversation.update(cx, |conversation, cx| {
            conversation.push(ChatRole::User, text.to_owned(), cx);
        });
        self.session.update(cx, |session, cx| session.submit(text.to_owned(), cx));
        self.save_active(cx);
        self.remote.publish(Notification::MessageAppended(MessageAppended {
            role: ChatRoleDto::User,
            text: text.to_owned(),
        }));
        true
    }

    /// 为附件目录补充会话级只读授权；桌面与远程行为一致。
    fn ensure_attachment_grant(&mut self, dir: &std::path::Path, cx: &mut Context<Self>) {
        let base = std::env::current_dir().unwrap_or_default();
        let dir = agent::approval::resolve_path(&dir.display().to_string(), &base);
        let mut grants = self.session.read(cx).grants().to_vec();
        if grants.iter().any(|grant| grant.path == dir) {
            return;
        }
        grants.push(agent::approval::DirectoryGrant::new(dir, agent::tool::Access::Read));
        self.session.update(cx, |session, cx| session.set_grants(grants, cx));
        self.save_active(cx);
        self.publish_remote_state(cx);
    }

    /// 把拖入的文件复制到会话附件目录，并在输入框中引用它们。
    pub(crate) fn attach_paths(
        &mut self,
        paths: &[std::path::PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        let persist = self.session.read(cx).persists_config();
        let dir = crate::remote::attachments_dir(persist).join(&self.active.id);
        if let Err(error) = std::fs::create_dir_all(&dir) {
            tracing::warn!("创建附件目录失败：{error}");
            return;
        }
        let mut attached = Vec::new();
        for path in paths {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let tag = uuid::Uuid::new_v4().simple().to_string();
            let target = dir.join(format!("{}-{name}", &tag[..8]));
            match std::fs::copy(path, &target) {
                Ok(_) => attached.push(target.display().to_string()),
                Err(error) => tracing::warn!("复制附件失败：{error}"),
            }
        }
        if attached.is_empty() {
            return;
        }
        self.ensure_attachment_grant(&dir, cx);
        let references =
            attached.iter().map(|path| format!("[附件] {path}")).collect::<Vec<_>>().join("\n");
        self.composer.update(cx, |composer, cx| {
            composer.append_text(&references, window, cx);
        });
    }

    /// 把远程命令泵挂到界面执行器；命令在顶层逐条处理。
    fn spawn_remote_pump(
        &mut self,
        mut commands: tokio::sync::mpsc::UnboundedReceiver<RemoteCommand>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            while let Some((command, reply)) = commands.recv().await {
                match this.update(cx, |haloeth, cx| haloeth.apply_remote_command(command, cx)) {
                    Ok(result) => {
                        let _ = reply.send(result);
                    }
                    Err(_) => break,
                }
            }
        })
        .detach();
    }

    /// 应用一条远程命令；在界面线程上执行。
    pub(crate) fn apply_remote_command(
        &mut self,
        command: Command,
        cx: &mut Context<Self>,
    ) -> Result<serde_json::Value, RemoteError> {
        use serde_json::{Value, json};
        match command {
            Command::Welcome => {
                Ok(serde_json::to_value(self.remote_welcome(cx)).unwrap_or(Value::Null))
            }
            Command::Snapshot => {
                Ok(serde_json::to_value(self.remote_snapshot(cx)).unwrap_or(Value::Null))
            }
            Command::Ping => Ok(json!({ "t": crate::remote::now_millis() })),
            Command::StateGet => Ok(json!({ "state": self.remote_state(cx) })),
            Command::Resume {
                session_id,
                last_seq,
            } => match self.remote.replay(&session_id, last_seq, &self.active.id) {
                Some(published) => {
                    let frames: Vec<remote::proto::NotificationFrame> = published
                        .iter()
                        .map(|item| {
                            remote::proto::NotificationFrame::new(item.seq, &item.notification)
                        })
                        .collect();
                    Ok(json!({ "notifications": frames }))
                }
                None => Ok(json!({ "snapshot": self.remote_snapshot(cx) })),
            },
            Command::AttachmentTarget => {
                let persist = self.session.read(cx).persists_config();
                let dir = crate::remote::attachments_dir(persist).join(&self.active.id);
                std::fs::create_dir_all(&dir)
                    .map_err(|error| RemoteError::internal(format!("创建附件目录失败：{error}")))?;
                self.ensure_attachment_grant(&dir, cx);
                Ok(json!({ "dir": dir.display().to_string() }))
            }
            Command::MessageSend {
                text,
                attachments,
            } => {
                let original = text.clone();
                let mut composed = text;
                if !attachments.is_empty() {
                    for path in &attachments {
                        let file = std::path::Path::new(path);
                        if !file.is_file() {
                            return Err(RemoteError::invalid_params(format!("附件不存在：{path}")));
                        }
                        if let Some(parent) = file.parent() {
                            self.ensure_attachment_grant(parent, cx);
                        }
                    }
                    composed = compose_attachment_text(&original, &attachments);
                }
                if self.submit_message(&composed, cx) {
                    Ok(Value::Null)
                } else {
                    Err(RemoteError::busy(original))
                }
            }
            Command::RunCancel => {
                self.session.update(cx, |session, cx| session.cancel(cx));
                Ok(Value::Null)
            }
            Command::ApprovalResolve {
                id,
                decision,
            } => {
                let decision = crate::remote::decision_from(decision);
                self.session.update(cx, |session, cx| session.resolve_approval(&id, decision, cx));
                self.save_active(cx);
                Ok(Value::Null)
            }
            Command::SessionNew => {
                if self.session.read(cx).status().is_busy() {
                    return Err(RemoteError::new(ErrorCode::Busy, "运行中"));
                }
                self.create_session(cx);
                Ok(json!({ "id": self.active.id }))
            }
            Command::SessionOpen {
                id,
            } => {
                if self.session.read(cx).status().is_busy() {
                    return Err(RemoteError::new(ErrorCode::Busy, "运行中"));
                }
                if id != self.active.id {
                    let Ok(file) = self.store.load(&id) else {
                        return Err(RemoteError::not_found(format!("会话不存在：{id}")));
                    };
                    self.save_active(cx);
                    self.activate(file, cx);
                }
                Ok(json!({ "id": self.active.id }))
            }
            Command::SessionRename {
                title,
            } => {
                self.rename_session(&title, cx).map_err(RemoteError::internal)?;
                Ok(Value::Null)
            }
            Command::SessionDelete {
                id,
            } => {
                if self.session.read(cx).status().is_busy() {
                    return Err(RemoteError::new(ErrorCode::Busy, "运行中"));
                }
                self.delete_session(&id, cx).map_err(RemoteError::internal)?;
                Ok(Value::Null)
            }
            Command::ModelSelect {
                provider,
                model,
            } => {
                let allowed = self
                    .session
                    .read(cx)
                    .config()
                    .models_with(&[Modality::Text], &[Modality::Text]);
                if !allowed.iter().any(|entry| entry.provider == provider && entry.model == model) {
                    return Err(RemoteError::unknown_model(format!("{provider}/{model}")));
                }
                self.session.update(cx, |session, cx| session.select_model(&provider, &model, cx));
                self.save_active(cx);
                self.publish_remote_state(cx);
                Ok(Value::Null)
            }
            Command::FeatureSet {
                key,
                enabled,
            } => {
                let Some((prefix, name)) = key.split_once(':') else {
                    return Err(RemoteError::unknown_feature(key));
                };
                let kind = match prefix {
                    "tool" => config::FeatureKind::Tool,
                    "mcp" => config::FeatureKind::Mcp,
                    "skill" => config::FeatureKind::Skill,
                    _ => return Err(RemoteError::unknown_feature(key)),
                };
                if name.is_empty() {
                    return Err(RemoteError::unknown_feature(key));
                }
                self.session
                    .update(cx, |session, cx| session.set_feature_state(kind, name, enabled, cx));
                self.save_active(cx);
                self.publish_remote_state(cx);
                Ok(Value::Null)
            }
            Command::GrantAdd {
                path,
                access,
            } => {
                let base = std::env::current_dir().unwrap_or_default();
                let path = agent::approval::resolve_path(&path, &base);
                if !path.is_dir() {
                    return Err(RemoteError::invalid_params(format!(
                        "目录不存在：{}",
                        path.display()
                    )));
                }
                let access = crate::remote::access_from(access);
                let mut grants = self.session.read(cx).grants().to_vec();
                if let Some(existing) = grants.iter_mut().find(|grant| grant.path == path) {
                    existing.access = access;
                } else {
                    grants.push(agent::approval::DirectoryGrant::new(path, access));
                }
                self.session.update(cx, |session, cx| session.set_grants(grants, cx));
                self.save_active(cx);
                self.publish_remote_state(cx);
                Ok(Value::Null)
            }
            Command::GrantRemove {
                path,
            } => {
                let base = std::env::current_dir().unwrap_or_default();
                let target = agent::approval::resolve_path(&path, &base);
                let mut grants = self.session.read(cx).grants().to_vec();
                grants.retain(|grant| grant.path != target);
                self.session.update(cx, |session, cx| session.set_grants(grants, cx));
                self.save_active(cx);
                self.publish_remote_state(cx);
                Ok(Value::Null)
            }
            Command::ApprovalMode {
                mode,
            } => {
                self.set_approval_mode(crate::remote::mode_from(mode), cx);
                self.publish_remote_state(cx);
                Ok(Value::Null)
            }
        }
    }

    /// 配置变更后同步远程服务与配对设备，并回收最近在线时间。
    pub(crate) fn sync_remote(&mut self, cx: &mut Context<Self>) {
        let mut config = self.session.read(cx).config().clone();
        if self.remote.merge_last_seen(&mut config.remote.devices) {
            if self.session.read(cx).persists_config() {
                let _ = config.save();
            }
            self.session.update(cx, |session, cx| session.apply_config(config, cx));
        }
        let remote = self.session.read(cx).config().remote.clone();
        let cert_dir = crate::remote::certificate_dir(self.session.read(cx).persists_config());
        self.remote.sync(&remote, &cert_dir);
    }

    /// 远程服务状态；偏好页展示用。
    pub(crate) fn remote_status(&self) -> BridgeStatus {
        self.remote.status()
    }

    /// 远程连接数。
    pub(crate) fn remote_connections(&self) -> usize {
        self.remote.connections()
    }

    /// 当前自签证书指纹（仅在启用 TLS 且服务运行时有值）。
    pub(crate) fn remote_tls_fingerprint(&self) -> Option<String> {
        self.remote.tls_fingerprint()
    }

    /// 设备最近在线时间（本次进程内）。
    pub(crate) fn remote_last_seen(&self, device_id: &str) -> Option<u64> {
        self.remote.last_seen(device_id)
    }

    /// 握手响应体。
    fn remote_welcome(&self, cx: &App) -> WelcomeDto {
        WelcomeDto {
            protocol: remote::PROTOCOL_VERSION,
            server: ServerInfoDto {
                name: "haloeth".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                platform: std::env::consts::OS.to_owned(),
            },
            state: self.remote_state(cx),
            sessions: self.remote_sessions(),
        }
    }

    /// 会话摘要列表。
    fn remote_sessions(&self) -> Vec<SessionSummaryDto> {
        self.store
            .list()
            .into_iter()
            .map(|summary| SessionSummaryDto {
                id: summary.id,
                title: summary.title,
                updated_at: summary.updated_at,
                message_count: summary.message_count,
            })
            .collect()
    }

    /// 全局状态快照。
    fn remote_state(&self, cx: &App) -> AppStateDto {
        let session = self.session.read(cx);
        let config = session.config();
        AppStateDto {
            active_session_id: self.active.id.clone(),
            title: self.active.title.clone(),
            status: crate::remote::status_dto(session.status()),
            activity: session.activity_label().map(str::to_owned),
            selection: session.selection().map(|(provider, model)| ModelRefDto {
                provider: provider.to_owned(),
                model: model.to_owned(),
            }),
            models: config
                .models_with(&[Modality::Text], &[Modality::Text])
                .into_iter()
                .map(|selection| ModelRefDto {
                    provider: selection.provider,
                    model: selection.model,
                })
                .collect(),
            features: self.remote_features(cx),
            grants: session
                .grants()
                .iter()
                .map(|grant| GrantDto {
                    path: grant.path.display().to_string(),
                    access: crate::remote::access_dto(grant.access),
                })
                .collect(),
            approval_mode: crate::remote::mode_dto(config.approval.mode),
            usage: crate::remote::usage_dto(session.usage(), session.usage_by_model()),
        }
    }

    /// 功能开关；与快速面板同源。
    fn remote_features(&self, cx: &App) -> Vec<FeatureDto> {
        let (tools, mcp, skills) = self.feature_rows(cx);
        let mut features = feature_dtos(config::FeatureKind::Tool, &tools);
        features.extend(feature_dtos(config::FeatureKind::Mcp, &mcp));
        features.extend(feature_dtos(config::FeatureKind::Skill, &skills));
        features
    }

    /// 快速面板与远程状态共用同一份功能清单。
    fn feature_rows(&self, cx: &App) -> FeatureRows {
        let config = self.session.read(cx).config().clone();
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
        (tool_rows, mcp_rows, skill_rows)
    }

    /// 连接时的实时快照；`entries` 不含打开中的流式消息。
    fn remote_snapshot(&self, cx: &App) -> SnapshotDto {
        let conversation = self.conversation.read(cx);
        let mut entries = conversation.snapshot(cx);
        let streaming = conversation.streaming_snapshot();
        if streaming.is_some() {
            // 流式消息总是最后一行，通过 `streaming` 单独递送。
            entries.pop();
        }
        SnapshotDto {
            session_id: self.active.id.clone(),
            entries: entries.iter().map(crate::remote::entry_dto).collect(),
            streaming: streaming.map(|(role, text)| StreamingDto {
                role: crate::remote::role_dto(role),
                text,
            }),
            watermark: self.remote.watermark(),
        }
    }

    /// 推送当前会话快照；会话切换与恢复时调用。
    fn publish_remote_reset(&self, cx: &App) {
        let snapshot = self.remote_snapshot(cx);
        self.remote.publish(Notification::TranscriptReset(TranscriptReset {
            session_id: snapshot.session_id,
            entries: snapshot.entries,
            streaming: snapshot.streaming,
        }));
    }

    /// 推送全局状态。
    fn publish_remote_state(&self, cx: &App) {
        self.remote.publish(Notification::StateChanged(StateChanged {
            state: self.remote_state(cx),
        }));
    }

    /// 推送会话列表。
    fn publish_remote_sessions(&self, _cx: &App) {
        self.remote.publish(Notification::SessionsChanged(SessionsChanged {
            summaries: self.remote_sessions(),
        }));
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
        let label =
            current.as_ref().map_or_else(|| "未选择模型".to_owned(), |(_, model)| model.clone());
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
        // 快速面板只列全局「显示」的功能，状态取当前会话。
        let (tool_rows, mcp_rows, skill_rows) = self.feature_rows(cx);
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
            .map_or_default(|provider| (provider.endpoint(), provider.api_key.clone()));
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
                .map_or_default(|provider| provider.headers.clone()),
        }
    }

    /// 切换录音；未配置时跳到偏好设置。
    fn toggle_microphone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.speech.read(cx).is_available(cx) {
            self.speech.update(cx, SpeechState::toggle);
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
                    this.session.update(cx, Session::cancel);
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
/// 功能清单：工具、MCP 与 Skill 的名称与启用状态。
type FeatureRows = (Vec<(String, bool)>, Vec<(String, bool)>, Vec<(String, bool)>);

/// 把附件路径作为引用行前置到用户消息；模型据此按路径读取。
fn compose_attachment_text(text: &str, attachments: &[String]) -> String {
    let mut composed = String::new();
    for path in attachments {
        composed.push_str(&format!("[附件] {path}\n"));
    }
    composed.push_str(text);
    composed
}

/// 功能行映射为线上状态。
fn feature_dtos(kind: config::FeatureKind, rows: &[(String, bool)]) -> Vec<FeatureDto> {
    let dto_kind = match kind {
        config::FeatureKind::Tool => FeatureKindDto::Tool,
        config::FeatureKind::Mcp => FeatureKindDto::Mcp,
        config::FeatureKind::Skill => FeatureKindDto::Skill,
    };
    rows.iter()
        .map(|(name, enabled)| FeatureDto {
            key: format!("{}:{name}", kind.prefix()),
            kind: dto_kind,
            name: name.clone(),
            enabled: *enabled,
        })
        .collect()
}

/// 当前时间（Unix 毫秒）。
fn status_tone(status: Status) -> Tone {
    match status {
        Status::Idle => Tone::Neutral,
        Status::Preparing | Status::Running => Tone::Accent,
        Status::AwaitingApproval
        | Status::Cancelling
        | Status::Unconfigured
        | Status::Unselected => Tone::Warning,
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
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.attach_paths(paths.paths(), window, cx);
            }))
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

/// 启动应用：初始化日志、注册退出动作并打开主窗口。
pub(crate) fn run() {
    crate::logging::init();
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
