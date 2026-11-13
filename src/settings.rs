//! 设置对话框：跨会话的配置。
//!
//! 这里只放跨对话的设置：外观、审核模型与网络策略、会话存储、MCP 服务器定义与 Skill 定义。
//! 当前对话的审核模式、目录授权、工具开关在主界面直接操作。
//!
//! 与模型面板一致，编辑先落在草稿配置上，保存后才写盘并应用到会话。

use agent::mcp::{McpServerConfig, McpTransport};
use agent::skill::{self, SKILL_FILE};
use gpui_kit::assets::IconName;
use gpui_kit::base::{TestSupportExt as _, h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Editor, EditorState, InputEvent, InputState};
use gpui_kit::component::speech::{SpeechEvent, SpeechState, SpeechStatus};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _,
};
use gpui_kit::prelude::{FluentBuilder as _, InteractiveElement as _};
use gpui_kit::*;

use crate::Haloeth;
use crate::audio::{ConfiguredRecognizer, SelectableMicrophone, input_devices};
use crate::config::{
    AudioBackend, Config, FeatureKind, MediaTool, ModelSelection, SendKey, ThemePreference,
};
use crate::mcp_form::McpForm;
use crate::session::Session;
use crate::skill_form::SkillForm;
use crate::ui::{Tone, cancel_button, field, input_frame, pick_folder, tinted};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};

/// 设置分页。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    /// 外观、审核、存储。
    #[default]
    General,
    /// 内置工具与搜索。
    Tools,
    /// MCP 服务器。
    Mcp,
    /// Skill 目录与定义。
    Skill,
    /// 无障碍：语音识别与语音播报。
    Audio,
}

impl SettingsTab {
    /// 分页标题。
    pub fn label(self) -> &'static str {
        match self {
            Self::General => "通用",
            Self::Tools => "工具",
            Self::Mcp => "MCP",
            Self::Skill => "Skill",
            Self::Audio => "无障碍",
        }
    }

    /// 分页标识。
    pub fn id(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Tools => "tools",
            Self::Mcp => "mcp",
            Self::Skill => "skill",
            Self::Audio => "audio",
        }
    }

    /// 当前分页的说明文字。
    pub fn description(self) -> &'static str {
        match self {
            Self::General => "外观、按键、审核模型、网络与会话存储设置",
            Self::Tools => "AI 可以使用的内置工具与搜索配置",
            Self::Mcp => "接入外部 MCP 服务器，为 AI 提供额外工具",
            Self::Skill => "Skill 根目录与技能包；AI 按需加载",
            Self::Audio => "语音识别与语音播报",
        }
    }
}

/// 设置面板。
pub struct SettingsForm {
    draft: Config,
    session: WeakEntity<Session>,
    haloeth: WeakEntity<Haloeth>,
    tab: SettingsTab,
    /// 会话存储目录
    storage: Entity<InputState>,
    /// 新增 Skill 根目录
    skill_dir: Entity<InputState>,
    /// 视频 / 音乐任务地址
    video_endpoint: Entity<InputState>,
    music_endpoint: Entity<InputState>,
    /// 通用提示词
    system_prompt: Entity<EditorState>,
    /// 按住说话快捷键
    voice_hotkey: Entity<InputState>,
    /// 录制快捷键时接收按键的焦点
    hotkey_focus: FocusHandle,
    /// 是否正在录制快捷键
    capturing_hotkey: bool,
    /// 语音识别语言与系统语音名
    asr_language: Entity<InputState>,
    tts_voice: Entity<InputState>,
    asr_endpoint: Entity<InputState>,
    tts_endpoint: Entity<InputState>,
    /// 偏好页里的语音识别测试会话
    asr: Entity<SpeechState>,
    /// 识别测试结果或错误
    asr_result: Option<SharedString>,
    /// 已发现的 Skill
    discovered: Vec<skill::Skill>,
    /// 上一次保存成功的配置；用于即时生效模式去重。
    last_saved: Config,
    /// 改动后是否自动保存（偏好窗口使用）。
    auto_save: bool,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsForm {
    /// 构造设置面板。
    pub fn new(
        window: &mut Window,
        config: &Config,
        session: WeakEntity<Session>,
        haloeth: WeakEntity<Haloeth>,
        tab: SettingsTab,
        auto_save: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let storage =
            cx.new(|cx| InputState::new(window, cx).placeholder("会话目录（留空使用系统默认）"));
        storage.update(cx, |state, cx| {
            state.set_value(config.storage.sessions_dir.clone(), window, cx)
        });
        let skill_dir = cx.new(|cx| InputState::new(window, cx).placeholder("额外的 Skill 根目录"));
        let video_endpoint =
            cx.new(|cx| InputState::new(window, cx).placeholder("视频任务提交地址"));
        let music_endpoint =
            cx.new(|cx| InputState::new(window, cx).placeholder("音乐任务提交地址"));
        video_endpoint.update(cx, |state, cx| {
            state.set_value(config.media.video.endpoint.clone(), window, cx)
        });
        music_endpoint.update(cx, |state, cx| {
            state.set_value(config.media.music.endpoint.clone(), window, cx)
        });
        let system_prompt = cx.new(|cx| {
            EditorState::new(window, cx).placeholder("通用提示词，例如：始终用简体中文回答")
        });
        system_prompt
            .update(cx, |state, cx| state.set_value(config.ui.system_prompt.clone(), window, cx));
        let voice_hotkey =
            cx.new(|cx| InputState::new(window, cx).placeholder("按住说话快捷键，例如 ctrl+v"));
        voice_hotkey
            .update(cx, |state, cx| state.set_value(config.ui.voice_hotkey.clone(), window, cx));
        let asr_language = cx.new(|cx| {
            InputState::new(window, cx).placeholder("识别语言，例如 zh 或 en（留空自动）")
        });
        asr_language
            .update(cx, |state, cx| state.set_value(config.audio.asr.language.clone(), window, cx));
        let tts_voice =
            cx.new(|cx| InputState::new(window, cx).placeholder("系统语音名（留空默认）"));
        let asr_endpoint =
            cx.new(|cx| InputState::new(window, cx).placeholder("转写接口地址（留空自动）"));
        asr_endpoint
            .update(cx, |state, cx| state.set_value(config.audio.asr.endpoint.clone(), window, cx));
        let tts_endpoint =
            cx.new(|cx| InputState::new(window, cx).placeholder("播报接口地址（留空自动）"));
        tts_endpoint
            .update(cx, |state, cx| state.set_value(config.audio.tts.endpoint.clone(), window, cx));
        tts_voice
            .update(cx, |state, cx| state.set_value(config.audio.tts.voice.clone(), window, cx));
        let asr = {
            let recognizer_session = session.clone();
            let input_session = session.clone();
            cx.new(move |cx| {
                SpeechState::new(cx)
                    .recognizer(ConfiguredRecognizer::new(recognizer_session))
                    .input(SelectableMicrophone::new(input_session))
                    .system_fallback(false)
                    .stop_timeout(std::time::Duration::from_secs(60))
            })
        };

        let draft = config.clone();
        let discovered = skill::discover(&draft.skill_dirs());
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(&asr, window, |this, _, event: &SpeechEvent, _, cx| {
            match event {
                SpeechEvent::Final(text) => {
                    this.asr_result = Some(if text.trim().is_empty() {
                        "没有识别到内容".into()
                    } else {
                        text.clone()
                    });
                    cx.notify();
                }
                SpeechEvent::Error(error) => {
                    this.asr_result = Some(format!("识别失败：{error}").into());
                    cx.notify();
                }
                _ => {}
            }
        }));
        subscriptions.push(cx.observe(&asr, |_, _, cx| cx.notify()));
        if auto_save {
            subscriptions.push(cx.subscribe_in(
                &system_prompt,
                window,
                |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.save_if_due(cx);
                    }
                },
            ));
            for input in [
                &storage,
                &voice_hotkey,
                &video_endpoint,
                &music_endpoint,
                &asr_language,
                &tts_voice,
                &asr_endpoint,
                &tts_endpoint,
            ] {
                subscriptions.push(cx.subscribe_in(
                    input,
                    window,
                    |this, _, event: &InputEvent, _, cx| {
                        if matches!(event, InputEvent::Change) {
                            this.save_if_due(cx);
                        }
                    },
                ));
            }
        }
        Self {
            draft,
            session,
            haloeth,
            tab,
            storage,
            skill_dir,
            video_endpoint,
            music_endpoint,
            system_prompt,
            voice_hotkey,
            hotkey_focus: cx.focus_handle(),
            capturing_hotkey: false,
            asr_language,
            tts_voice,
            asr_endpoint,
            tts_endpoint,
            asr,
            asr_result: None,
            discovered,
            last_saved: config.clone(),
            auto_save,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    /// 切换分页。
    pub(crate) fn set_tab(&mut self, tab: SettingsTab, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }

    /// 从当前会话配置重新载入草稿；切换偏好分页时同步模型页的修改。
    pub(crate) fn reload(&mut self, config: &Config, window: &mut Window, cx: &mut Context<Self>) {
        self.draft = config.clone();
        self.last_saved = config.clone();
        self.discovered = skill::discover(&self.draft.skill_dirs());
        self.error = None;
        self.storage.update(cx, |state, cx| {
            state.set_value(config.storage.sessions_dir.clone(), window, cx)
        });
        self.video_endpoint.update(cx, |state, cx| {
            state.set_value(config.media.video.endpoint.clone(), window, cx)
        });
        self.music_endpoint.update(cx, |state, cx| {
            state.set_value(config.media.music.endpoint.clone(), window, cx)
        });
        self.asr_language
            .update(cx, |state, cx| state.set_value(config.audio.asr.language.clone(), window, cx));
        self.tts_voice
            .update(cx, |state, cx| state.set_value(config.audio.tts.voice.clone(), window, cx));
        self.asr_endpoint
            .update(cx, |state, cx| state.set_value(config.audio.asr.endpoint.clone(), window, cx));
        self.tts_endpoint
            .update(cx, |state, cx| state.set_value(config.audio.tts.endpoint.clone(), window, cx));
        self.voice_hotkey
            .update(cx, |state, cx| state.set_value(config.ui.voice_hotkey.clone(), window, cx));
        self.system_prompt
            .update(cx, |state, cx| state.set_value(config.ui.system_prompt.clone(), window, cx));
        self.capturing_hotkey = false;
        cx.notify();
    }

    /// 录制快捷键：点击「录制」后按下任意组合键就填入。
    fn capture_hotkey(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.capturing_hotkey {
            return;
        }
        let keystroke = &event.keystroke;
        let key = keystroke.key.to_ascii_lowercase();
        if key == "escape" {
            self.capturing_hotkey = false;
            cx.notify();
            return;
        }
        // 只按下修饰键时继续等待真正的按键。
        if matches!(
            key.as_str(),
            "control"
                | "ctrl"
                | "shift"
                | "alt"
                | "option"
                | "meta"
                | "platform"
                | "function"
                | "fn"
                | "capslock"
                | ""
        ) {
            return;
        }
        let modifiers = keystroke.modifiers;
        let mut parts: Vec<&str> = Vec::new();
        if modifiers.control {
            parts.push("ctrl");
        }
        if modifiers.alt {
            parts.push("alt");
        }
        if modifiers.shift {
            parts.push("shift");
        }
        let text = if parts.is_empty() {
            key.clone()
        } else {
            format!("{}+{key}", parts.join("+"))
        };
        // 单独的字母键会把正常打字也变成录音，只放行功能键。
        if parts.is_empty() && !is_function_key(&key) {
            self.error = Some("快捷键至少要包含 Ctrl、Alt 或 Shift".into());
            cx.notify();
            return;
        }
        if crate::background::Hotkey::parse(&text).is_none() {
            self.error = Some(format!("这个按键不能用作快捷键：{text}").into());
            cx.notify();
            return;
        }
        self.error = None;
        self.capturing_hotkey = false;
        self.draft.ui.voice_hotkey = text.clone();
        self.voice_hotkey.update(cx, |state, cx| state.set_value(text.clone(), window, cx));
        self.save_if_due(cx);
        cx.notify();
    }

    /// 即时生效：草稿有变化时写入配置并应用到会话。
    ///
    /// 不调用 `save()` 的会话落盘路径，避免每敲一个字都写会话文件。
    pub(crate) fn save_if_due(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.auto_save {
            return false;
        }
        let storage = self.storage.read(cx).value().trim().to_owned();
        if !storage.is_empty() && !std::path::Path::new(&storage).is_dir() {
            self.error = Some("会话目录必须是一个已存在的目录".into());
            cx.notify();
            return false;
        }
        self.draft.storage.sessions_dir = storage;
        // 审核模型的选择由「当前对话模型 / 单独指定」按钮决定；
        // 不能因为还没挑模型就把开关拨回去，否则点一下按钮立刻又变回来。
        self.draft.media.video.endpoint = self.video_endpoint.read(cx).value().trim().to_owned();
        self.draft.media.music.endpoint = self.music_endpoint.read(cx).value().trim().to_owned();
        self.draft.ui.system_prompt = self.system_prompt.read(cx).value().trim().to_owned();
        self.draft.ui.voice_hotkey = self.voice_hotkey.read(cx).value().trim().to_owned();
        self.draft.audio.asr.language = self.asr_language.read(cx).value().trim().to_owned();
        self.draft.audio.tts.voice = self.tts_voice.read(cx).value().trim().to_owned();
        self.draft.audio.asr.endpoint = self.asr_endpoint.read(cx).value().trim().to_owned();
        self.draft.audio.tts.endpoint = self.tts_endpoint.read(cx).value().trim().to_owned();
        if self.draft == self.last_saved {
            return false;
        }
        // 测试注入的会话不写真实配置文件。
        let persist = self
            .session
            .upgrade()
            .map(|session| session.read(cx).persists_config())
            .unwrap_or(true);
        if persist && let Err(error) = self.draft.save() {
            self.error = Some(format!("保存失败：{error}").into());
            cx.notify();
            return false;
        }
        self.error = None;
        let config = self.draft.clone();
        self.last_saved = config.clone();
        self.session.update(cx, |session, cx| session.apply_config(config, cx)).ok();
        self.haloeth.update(cx, |haloeth, cx| haloeth.sync_input_preferences(cx)).ok();
        true
    }

    /// 保存：校验、写盘、应用到会话。
    pub fn save(&mut self, cx: &mut Context<Self>) -> bool {
        let storage = self.storage.read(cx).value().trim().to_owned();
        if !storage.is_empty() && !std::path::Path::new(&storage).is_dir() {
            self.error = Some("会话目录必须是一个已存在的目录".into());
            if !self.auto_save {
                self.tab = SettingsTab::General;
            }
            cx.notify();
            return false;
        }
        for dir in &self.draft.skills.dirs {
            if !std::path::Path::new(dir.trim()).is_dir() {
                self.error = Some(format!("Skill 目录不存在：{dir}").into());
                if !self.auto_save {
                    self.tab = SettingsTab::Skill;
                }
                cx.notify();
                return false;
            }
        }
        self.draft.storage.sessions_dir = storage;
        // 审核模型的选择由「当前对话模型 / 单独指定」按钮决定；
        // 不能因为还没挑模型就把开关拨回去，否则点一下按钮立刻又变回来。
        self.draft.media.video.endpoint = self.video_endpoint.read(cx).value().trim().to_owned();
        self.draft.media.music.endpoint = self.music_endpoint.read(cx).value().trim().to_owned();
        self.draft.ui.system_prompt = self.system_prompt.read(cx).value().trim().to_owned();
        self.draft.ui.voice_hotkey = self.voice_hotkey.read(cx).value().trim().to_owned();
        self.draft.audio.asr.language = self.asr_language.read(cx).value().trim().to_owned();
        self.draft.audio.tts.voice = self.tts_voice.read(cx).value().trim().to_owned();
        self.draft.audio.asr.endpoint = self.asr_endpoint.read(cx).value().trim().to_owned();
        self.draft.audio.tts.endpoint = self.tts_endpoint.read(cx).value().trim().to_owned();

        let persist = self
            .session
            .upgrade()
            .map(|session| session.read(cx).persists_config())
            .unwrap_or(true);
        if persist && let Err(error) = self.draft.save() {
            self.error = Some(format!("保存失败：{error}").into());
            cx.notify();
            return false;
        }
        let config = self.draft.clone();
        self.session.update(cx, |session, cx| session.apply_config(config, cx)).ok();
        self.haloeth
            .update(cx, |haloeth, cx| {
                haloeth.sync_input_preferences(cx);
                haloeth.save_active(cx);
            })
            .ok();
        true
    }

    /// 切换审核模型的来源。
    fn toggle_reviewer(&mut self, cx: &mut Context<Self>) {
        let reviewer = &mut self.draft.approval.reviewer;
        reviewer.use_current = !reviewer.use_current;
        cx.notify();
    }

    /// 切换外观并立即预览。
    fn set_theme(&mut self, theme: ThemePreference, window: &mut Window, cx: &mut Context<Self>) {
        self.draft.ui.theme = theme;
        // 跟随系统时要读窗口外观，不能传 None，否则会被当成浅色。
        crate::ui::apply_theme(theme, Some(window), cx);
        cx.notify();
    }

    /// 切换发送键偏好；保存后由主界面同步到输入缓冲区。
    fn set_send_key(&mut self, send_key: SendKey, cx: &mut Context<Self>) {
        self.draft.ui.send_key = send_key;
        cx.notify();
    }

    /// 切换搜索。
    fn toggle_search(&mut self, cx: &mut Context<Self>) {
        self.draft.search.enabled = !self.draft.search.enabled;
        cx.notify();
    }

    /// 切换功能的「显示」开关；重新显示时清除会话覆盖，回到默认启用状态。
    fn toggle_visible(&mut self, kind: FeatureKind, name: &str, cx: &mut Context<Self>) {
        let hidden = self.draft.feature_prefs.toggle_hidden(kind, name);
        if !hidden {
            self.session
                .update(cx, |session, cx| session.clear_feature_override(kind, name, cx))
                .ok();
        }
        cx.notify();
    }

    /// 切换功能的「默认启用」开关。
    fn toggle_default(&mut self, kind: FeatureKind, name: &str, cx: &mut Context<Self>) {
        self.draft.feature_prefs.toggle_default(kind, name);
        cx.notify();
    }

    /// 删除 MCP 服务器。
    fn remove_server(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.draft.mcp_servers.len() {
            self.draft.mcp_servers.remove(index);
            cx.notify();
        }
    }

    /// 接收新建对话框提交的服务器；重名时返回错误。
    pub(crate) fn add_mcp_server(
        &mut self,
        server: McpServerConfig,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.draft.mcp_servers.iter().any(|existing| existing.name == server.name) {
            return Err(format!("服务器「{}」已存在", server.name));
        }
        self.draft.mcp_servers.push(server);
        self.error = None;
        cx.notify();
        Ok(())
    }

    /// 打开「新建 MCP 服务器」对话框。
    fn open_mcp_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = cx.weak_entity();
        let form = cx.new(|cx| McpForm::new(window, settings, cx));

        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .border_2()
                .title("新建 MCP 服务器")
                .overlay_closable(false)
                .child(form.clone())
                .footer(form.update(cx, |form, cx| form.render_footer(cx).into_any_element()))
        });
    }

    /// 添加一个 Skill 根目录。
    fn add_skill_dir(&mut self, cx: &mut Context<Self>) {
        let raw = self.skill_dir.read(cx).value().trim().to_owned();
        if raw.is_empty() {
            self.error = Some("请输入 Skill 目录".into());
            cx.notify();
            return;
        }
        let path =
            agent::approval::resolve_path(&raw, &std::env::current_dir().unwrap_or_default());
        if !path.is_dir() {
            self.error = Some(format!("目录不存在：{}", path.display()).into());
            cx.notify();
            return;
        }
        let path = path.display().to_string();
        if !self.draft.skills.dirs.contains(&path) {
            self.draft.skills.dirs.push(path);
            self.discovered = skill::discover(&self.draft.skill_dirs());
        }
        self.error = None;
        cx.notify();
    }

    /// 移除一个 Skill 根目录。
    fn remove_skill_dir(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.draft.skills.dirs.len() {
            self.draft.skills.dirs.remove(index);
            self.discovered = skill::discover(&self.draft.skill_dirs());
            cx.notify();
        }
    }

    /// 重新扫描 Skill 目录。
    fn rescan(&mut self, cx: &mut Context<Self>) {
        self.discovered = skill::discover(&self.draft.skill_dirs());
        self.error = None;
        cx.notify();
    }

    /// 接收新建 Skill 的结果：刷新列表，并把新 Skill 设为默认启用。
    pub(crate) fn add_created_skill(&mut self, name: &str, cx: &mut Context<Self>) {
        let mut pref = self.draft.feature_prefs.get(FeatureKind::Skill, name);
        pref.default_enabled = true;
        self.draft.feature_prefs.set(FeatureKind::Skill, name, pref);
        self.discovered = skill::discover(&self.draft.skill_dirs());
        self.error = None;
        cx.notify();
    }

    /// 删除 Skill：先确认，确认后删除目录。
    fn confirm_delete_skill(
        &mut self,
        name: String,
        root: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let form = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let form = form.clone();
            let name = name.clone();
            let root = root.clone();
            let button_name = name.clone();
            let button_root = root.clone();
            alert
                .border_2()
                .title(format!("删除 Skill「{name}」？"))
                .description(format!("将直接删除目录：{}", root.display()))
                .footer(
                    DialogFooter::new()
                        .child(
                            crate::ui::cancel_button("cancel", "取消", cx)
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("ok").danger().label("删除").on_click({
                            let form = form.clone();
                            move |_, window, cx| {
                                form.update(cx, |form, cx| {
                                    form.delete_skill(&button_name, &button_root, cx)
                                })
                                .ok();
                                window.close_dialog(cx);
                            }
                        })),
                )
                .on_ok({
                    let form = form.clone();
                    let name = name.clone();
                    let root = root.clone();
                    move |_, _, cx| {
                        form.update(cx, |form, cx| form.delete_skill(&name, &root, cx)).ok();
                        true
                    }
                })
        });
    }

    /// 删除 Skill 目录并刷新列表。
    fn delete_skill(&mut self, name: &str, root: &std::path::Path, cx: &mut Context<Self>) {
        match skill::delete_skill(root) {
            Ok(()) => {
                self.draft.feature_prefs.map_mut(FeatureKind::Skill).remove(name);
                self.discovered = skill::discover(&self.draft.skill_dirs());
                self.error = None;
            }
            Err(error) => {
                self.error = Some(format!("删除 Skill 失败：{error}").into());
            }
        }
        cx.notify();
    }

    /// 打开「新建 Skill」对话框。
    fn open_skill_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let roots = self.draft.skill_dirs();
        if roots.is_empty() {
            self.error = Some("还没有可用的 Skill 根目录".into());
            cx.notify();
            return;
        }
        let settings = cx.weak_entity();
        let form = cx.new(|cx| SkillForm::new(window, settings, roots, cx));

        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .border_2()
                .title("新建 Skill")
                .overlay_closable(false)
                .child(form.clone())
                .footer(form.update(cx, |form, cx| form.render_footer(cx).into_any_element()))
        });
    }

    /// 打开 Skill 目录。
    fn open_skill_dir(&mut self, path: &std::path::Path, cx: &mut Context<Self>) {
        let program = if cfg!(windows) {
            "explorer"
        } else if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        if let Err(error) = std::process::Command::new(program).arg(path).spawn() {
            self.error = Some(format!("无法打开目录：{error}").into());
            cx.notify();
        }
    }

    /// 面板底部：错误提示、取消与保存。
    pub fn render_footer(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let form = cx.entity();

        DialogFooter::new()
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .id("settings-error")
                        .role(Role::Status)
                        .test_support()
                        .aria_label(error.clone())
                        .flex_1()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .child(
                cancel_button("settings-cancel", "取消", cx)
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
            .child(Button::new("settings-save").primary().label("保存").on_click({
                let form = form.clone();
                move |_, window, cx| {
                    if form.update(cx, |form, cx| form.save(cx)) {
                        window.close_dialog(cx);
                    }
                }
            }))
    }

    /// 分页切换条。
    fn render_tabs(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = [
            SettingsTab::General,
            SettingsTab::Tools,
            SettingsTab::Mcp,
            SettingsTab::Skill,
            SettingsTab::Audio,
        ];
        let active = self.tab;
        h_flex().gap_1().flex_wrap().children(tabs.into_iter().map(move |tab| {
            let selected = tab == active;
            let button = Button::new(ElementId::Name(format!("settings-tab:{}", tab.id()).into()))
                .outline()
                .border_2()
                .xsmall()
                .label(tab.label())
                .selected(selected)
                .toggled(selected);
            if selected {
                tinted(button, Tone::Accent, cx).on_click(cx.listener(move |this, _, _, cx| {
                    this.set_tab(tab, cx);
                }))
            } else {
                button.on_click(cx.listener(move |this, _, _, cx| this.set_tab(tab, cx)))
            }
        }))
    }

    /// 新会话默认模型：未设置时显示「无」，点击可去模型页配置。
    fn render_default_model(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.draft.selection().map(|(provider, model)| ModelSelection {
            provider: provider.to_owned(),
            model: model.to_owned(),
        });
        let candidates = self
            .draft
            .models_with(&[agent::model::Modality::Text], &[agent::model::Modality::Text]);
        if candidates.is_empty() {
            let haloeth = self.haloeth.clone();
            return cancel_button("default-model", "无 · 前往模型设置", cx)
                .on_click(move |_, window, cx| {
                    let _ = haloeth.update(cx, |haloeth, cx| {
                        haloeth.open_preferences(
                            crate::preferences::PrefSection::Models,
                            window,
                            cx,
                        )
                    });
                })
                .into_any_element();
        }

        let label = current
            .as_ref()
            .map(|selection| format!("{} · {}", selection.provider, selection.model))
            .unwrap_or_else(|| "无".to_owned());
        let tint = if current.is_some() {
            Tone::Neutral
        } else {
            Tone::Danger
        };
        let button = tinted(Button::new("default-model").outline().border_2().xsmall(), tint, cx)
            .label(label);
        let form = cx.entity().downgrade();
        button
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.scrollable(true);
                {
                    let form = form.clone();
                    menu =
                        menu.item(PopupMenuItem::new("无（不预设）").on_click(move |_, _, cx| {
                            let _ = form.update(cx, |this, cx| {
                                this.draft.clear_selection();
                                cx.notify();
                            });
                        }));
                }
                for candidate in &candidates {
                    let form = form.clone();
                    let candidate = candidate.clone();
                    menu = menu.item(
                        PopupMenuItem::new(format!("{} · {}", candidate.provider, candidate.model))
                            .on_click(move |_, _, cx| {
                                let _ = form.update(cx, |this, cx| {
                                    let _ = this
                                        .draft
                                        .select_model(&candidate.provider, &candidate.model);
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            })
            .into_any_element()
    }

    /// 模型下拉框：按输入 / 输出模态过滤；`clear` 时提供「跟随当前模型」。
    fn render_model_choice(
        &self,
        id: &'static str,
        current: ModelSelection,
        input: agent::model::Modality,
        output: agent::model::Modality,
        clear: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let candidates = self.draft.models_with(&[input], &[output]);
        let label = if current.is_set() {
            format!("{} · {}", current.provider, current.model)
        } else {
            "选择审核模型…".to_owned()
        };
        let form = cx.entity().downgrade();
        Button::new(id)
            .outline()
            .border_2()
            .xsmall()
            .label(label)
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.scrollable(true);
                if clear {
                    let form = form.clone();
                    menu = menu.item(PopupMenuItem::new("跟随当前对话模型").on_click(
                        move |_, _, cx| {
                            let _ = form.update(cx, |this, cx| {
                                this.draft.approval.reviewer.provider.clear();
                                this.draft.approval.reviewer.model.clear();
                                this.draft.approval.reviewer.use_current = true;
                                cx.notify();
                            });
                        },
                    ));
                }
                if candidates.is_empty() {
                    return menu.item(PopupMenuItem::label("没有匹配模态的模型"));
                }
                for candidate in &candidates {
                    let form = form.clone();
                    let candidate = candidate.clone();
                    menu = menu.item(
                        PopupMenuItem::new(format!("{} · {}", candidate.provider, candidate.model))
                            .on_click(move |_, _, cx| {
                                let _ = form.update(cx, |this, cx| {
                                    this.draft.approval.reviewer.provider =
                                        candidate.provider.clone();
                                    this.draft.approval.reviewer.model = candidate.model.clone();
                                    this.draft.approval.reviewer.use_current = false;
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            })
            .into_any_element()
    }

    /// 通用：外观、审核模型、网络策略与存储。
    fn render_general(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let capturing = self.capturing_hotkey;
        let current = self.draft.ui.theme;
        let themes = [ThemePreference::System, ThemePreference::Light, ThemePreference::Dark];
        let storage_path = self.draft.sessions_root().display().to_string();

        v_flex()
            .gap_2()
            .child(section_title("外观", cx))
            .child(h_flex().gap_2().children(themes.into_iter().map(|theme| {
                let selected = current == theme;
                let button =
                    Button::new(ElementId::Name(format!("theme:{}", theme.as_str()).into()))
                        .outline()
                        .border_2()
                        .xsmall()
                        .label(theme.label())
                        .selected(selected)
                        .toggled(selected);
                if selected {
                    tinted(button, Tone::Accent, cx).on_click(
                        cx.listener(move |this, _, window, cx| this.set_theme(theme, window, cx)),
                    )
                } else {
                    button.on_click(
                        cx.listener(move |this, _, window, cx| this.set_theme(theme, window, cx)),
                    )
                }
            })))
            .child(section_title("按键", cx))
            .child(h_flex().gap_2().children(
                [SendKey::Enter, SendKey::SecondaryEnter].into_iter().map(|send_key| {
                    let selected = self.draft.ui.send_key == send_key;
                    let button = Button::new(ElementId::Name(
                        format!("send-key:{}", send_key.as_str()).into(),
                    ))
                    .outline()
                    .border_2()
                    .xsmall()
                    .label(send_key.label())
                    .selected(selected)
                    .toggled(selected);
                    if selected {
                        tinted(button, Tone::Accent, cx).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.set_send_key(send_key, cx);
                            },
                        ))
                    } else {
                        button.on_click(cx.listener(move |this, _, _, cx| {
                            this.set_send_key(send_key, cx);
                        }))
                    }
                }),
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("另一个组合键用于换行；保存后立即作用于输入框。"),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        tinted(
                            Button::new("settings-line-numbers").outline().border_2().xsmall(),
                            if self.draft.ui.line_numbers {
                                Tone::Success
                            } else {
                                Tone::Danger
                            },
                            cx,
                        )
                        .label(if self.draft.ui.line_numbers {
                            "输入框行号：显示"
                        } else {
                            "输入框行号：隐藏"
                        })
                        .selected(self.draft.ui.line_numbers)
                        .toggled(self.draft.ui.line_numbers)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.draft.ui.line_numbers = !this.draft.ui.line_numbers;
                            cx.notify();
                        })),
                    )
                    .child(
                        div().text_xs().text_color(muted).child("输入框上方拖动条可以调整高度。"),
                    ),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        tinted(
                            Button::new("settings-background").outline().border_2().xsmall(),
                            if self.draft.ui.run_in_background {
                                Tone::Success
                            } else {
                                Tone::Danger
                            },
                            cx,
                        )
                        .label(if self.draft.ui.run_in_background {
                            "关闭后：后台运行"
                        } else {
                            "关闭后：退出"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.draft.ui.run_in_background = !this.draft.ui.run_in_background;
                            cx.notify();
                        })),
                    )
                    .child(
                        Button::new("open-log-dir")
                            .outline()
                            .border_2()
                            .xsmall()
                            .label("打开日志目录")
                            .on_click(|_, _, _| {
                                let _ = crate::logging::open_log_dir();
                            }),
                    ),
            )
            .child(section_title("按住说话", cx))
            .child(field(
                "快捷键（留空关闭）",
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(input_frame(
                        &self.voice_hotkey,
                        "voice-hotkey",
                        true,
                        cx,
                    )))
                    .child(
                        Button::new("hotkey-capture")
                            .outline()
                            .border_2()
                            .xsmall()
                            .label(if capturing {
                                "请按下按键…"
                            } else {
                                "录制"
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.capturing_hotkey = !this.capturing_hotkey;
                                if this.capturing_hotkey {
                                    this.hotkey_focus.focus(window, cx);
                                }
                                cx.notify();
                            })),
                    ),
                cx,
            ))
            // 录制时由这个元素接收按键；按 Esc 取消。
            .child(div().id("hotkey-listener").test_support().track_focus(&self.hotkey_focus).when(
                capturing,
                |this| {
                    this.on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        this.capture_hotkey(event, window, cx);
                    }))
                },
            ))
            .child(div().text_xs().text_color(muted).child(if capturing {
                "正在录制：按下想要的组合键，Esc 取消。"
            } else {
                "全局生效：按住快捷键录音，松开后转写并自动发送；默认 ctrl+v。"
            }))
            .child(section_title("通用提示词", cx))
            .child(
                div()
                    .w_full()
                    .border_2()
                    .border_color(cx.theme().input)
                    .rounded(cx.theme().radius)
                    .child(
                        Editor::new(&self.system_prompt)
                            .h(rems(4.))
                            .appearance(false)
                            .bordered(false),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("会附加到每个对话的系统提示，例如指定回答语言。"),
            )
            .child(section_title("新会话默认模型", cx))
            .child(self.render_default_model(cx))
            .child(section_title("审核", cx))
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_xs().text_color(muted).child("默认审核模式"))
                    .children(
                        [
                            agent::approval::ApprovalMode::Manual,
                            agent::approval::ApprovalMode::Rules,
                            agent::approval::ApprovalMode::Model,
                            agent::approval::ApprovalMode::Off,
                        ]
                        .into_iter()
                        .map(|mode| {
                            let selected = self.draft.approval.mode == mode;
                            let button = Button::new(ElementId::Name(
                                format!("pref-mode:{}", mode.as_str()).into(),
                            ))
                            .outline()
                            .border_2()
                            .xsmall()
                            .label(mode.label())
                            .selected(selected)
                            .toggled(selected);
                            if selected {
                                tinted(button, Tone::Accent, cx).on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.draft.approval.mode = mode;
                                        cx.notify();
                                    },
                                ))
                            } else {
                                button.on_click(cx.listener(move |this, _, _, cx| {
                                    this.draft.approval.mode = mode;
                                    cx.notify();
                                }))
                            }
                        }),
                    ),
            )
            .child(div().text_xs().text_color(muted).child(self.draft.approval.mode.description()))
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        tinted(
                            Button::new("settings-network").outline().border_2().xsmall(),
                            if self.draft.approval.allow_network {
                                Tone::Success
                            } else {
                                Tone::Warning
                            },
                            cx,
                        )
                        .label(if self.draft.approval.allow_network {
                            "公网访问：放行"
                        } else {
                            "公网访问：询问"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.draft.approval.allow_network = !this.draft.approval.allow_network;
                            cx.notify();
                        })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("仅在「规则自动」下生效；本地与内网地址始终询问。"),
                    ),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("settings-reviewer")
                            .outline()
                            .border_2()
                            .xsmall()
                            .label(if self.draft.approval.reviewer.use_current {
                                "审核模型：当前对话模型"
                            } else {
                                "审核模型：单独指定"
                            })
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_reviewer(cx))),
                    )
                    .when(!self.draft.approval.reviewer.use_current, |this| {
                        this.child(self.render_model_choice(
                            "reviewer-model",
                            ModelSelection {
                                provider: self.draft.approval.reviewer.provider.clone(),
                                model: self.draft.approval.reviewer.model.clone(),
                            },
                            agent::model::Modality::Text,
                            agent::model::Modality::Text,
                            true,
                            cx,
                        ))
                    }),
            )
            .child(section_title("会话存储", cx))
            .child(div().text_xs().text_color(muted).child(format!("当前位置：{storage_path}")))
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(input_frame(&self.storage, "storage-dir", true, cx).flex_1())
                    .child(
                        Button::new("storage-pick")
                            .outline()
                            .border_2()
                            .xsmall()
                            .icon(IconName::FolderOpen)
                            .label("选择…")
                            .tooltip("选择会话目录")
                            .on_click(cx.listener(|this, _, window, cx| {
                                let storage = this.storage.clone();
                                pick_folder(
                                    window,
                                    cx,
                                    "选择会话目录",
                                    move |_, path, window, cx| {
                                        storage.update(cx, |state, cx| {
                                            state.set_value(path, window, cx)
                                        });
                                    },
                                );
                            })),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("修改后新会话写入新位置，旧目录中的会话不会自动迁移。"),
            )
    }

    /// 无障碍：语音识别与语音播报。
    fn render_audio(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .gap_2()
            .child(section_title("语音识别（ASR）", cx))
            .child(self.render_backend_row(true, cx))
            .child(self.render_audio_model_row(true, cx))
            .child(self.render_asr_device(cx))
            .child(field("识别语言", input_frame(&self.asr_language, "asr-language", true, cx), cx))
            .child(field(
                "转写接口地址",
                input_frame(&self.asr_endpoint, "asr-endpoint", true, cx),
                cx,
            ))
            .child(self.render_asr_test(cx))
            .child(section_title("语音播报（TTS）", cx))
            .child(self.render_backend_row(false, cx))
            .child(self.render_audio_model_row(false, cx))
            .child(field(
                "语音名（云端模型使用；系统语音使用默认）",
                input_frame(&self.tts_voice, "tts-voice", true, cx),
                cx,
            ))
            .child(field(
                "播报接口地址",
                input_frame(&self.tts_endpoint, "tts-endpoint", true, cx),
                cx,
            ))
            .child(
                Button::new("tts-test").outline().border_2().xsmall().label("试听播报").on_click(
                    cx.listener(|this, _, _, cx| {
                        let _ = this.haloeth.update(cx, |haloeth, cx| haloeth.preview_tts(cx));
                    }),
                ),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("tts-rate-down")
                            .outline()
                            .border_2()
                            .xsmall()
                            .label("语速 −")
                            .on_click(cx.listener(|this, _, _, cx| this.adjust_tts_rate(-1, cx))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("当前语速：{}", self.draft.audio.tts.rate)),
                    )
                    .child(
                        Button::new("tts-rate-up")
                            .outline()
                            .border_2()
                            .xsmall()
                            .label("语速 +")
                            .on_click(cx.listener(|this, _, _, cx| this.adjust_tts_rate(1, cx))),
                    )
                    .child(
                        Button::new("tts-volume-down")
                            .outline()
                            .border_2()
                            .xsmall()
                            .label("音量 −")
                            .on_click(
                                cx.listener(|this, _, _, cx| this.adjust_tts_volume(-10, cx)),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("音量：{}", self.draft.audio.tts.volume)),
                    )
                    .child(
                        Button::new("tts-volume-up")
                            .outline()
                            .border_2()
                            .xsmall()
                            .label("音量 +")
                            .on_click(cx.listener(|this, _, _, cx| this.adjust_tts_volume(10, cx))),
                    ),
            )
    }

    /// 麦克风设备选择。
    fn render_asr_device(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.draft.audio.asr.input_device.clone();
        let label = if current.trim().is_empty() {
            "系统默认麦克风".to_owned()
        } else {
            current.clone()
        };
        let devices = input_devices();
        let form = cx.entity().downgrade();
        h_flex()
            .items_center()
            .gap_2()
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child("麦克风"))
            .child(
                Button::new("asr-device").outline().border_2().xsmall().label(label).dropdown_menu(
                    move |menu, _, _| {
                        let mut menu = menu.scrollable(true);
                        {
                            let form = form.clone();
                            menu = menu.item(PopupMenuItem::new("系统默认").on_click(
                                move |_, _, cx| {
                                    let _ = form.update(cx, |this, cx| {
                                        this.draft.audio.asr.input_device.clear();
                                        cx.notify();
                                    });
                                },
                            ));
                        }
                        for device in &devices {
                            let form = form.clone();
                            let device = device.clone();
                            menu = menu.item(PopupMenuItem::new(device.clone()).on_click(
                                move |_, _, cx| {
                                    let _ = form.update(cx, |this, cx| {
                                        this.draft.audio.asr.input_device = device.clone();
                                        cx.notify();
                                    });
                                },
                            ));
                        }
                        menu
                    },
                ),
            )
    }

    /// 在偏好页里直接测试语音识别。
    fn render_asr_test(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.asr.read(cx).status();
        let active = status.is_active();
        h_flex()
            .items_center()
            .gap_2()
            .child(
                tinted(
                    Button::new("asr-test").outline().border_2().xsmall(),
                    if active {
                        Tone::Success
                    } else {
                        Tone::Neutral
                    },
                    cx,
                )
                .label(if active {
                    "停止测试"
                } else {
                    "测试麦克风"
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.asr.update(cx, |asr, cx| asr.toggle(cx));
                })),
            )
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(msg(status)))
            .when_some(self.asr_result.clone(), |this, result| {
                this.child(div().text_xs().text_color(cx.theme().foreground).child(result))
            })
    }

    /// ASR / TTS 后端选择。
    fn render_backend_row(&self, asr: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let current = if asr {
            self.draft.audio.asr.backend
        } else {
            self.draft.audio.tts.backend
        };
        let prefix = if asr {
            "asr"
        } else {
            "tts"
        };
        h_flex().gap_2().children(
            [AudioBackend::Off, AudioBackend::Cloud, AudioBackend::System].into_iter().map(
                |backend| {
                    let selected = current == backend;
                    let button = Button::new(ElementId::Name(
                        format!("{prefix}-backend:{}", backend.as_str()).into(),
                    ))
                    .outline()
                    .border_2()
                    .xsmall()
                    .label(backend.label())
                    .selected(selected)
                    .toggled(selected);
                    if selected {
                        tinted(button, Tone::Accent, cx).on_click(cx.listener(
                            move |this, _, _, cx| this.set_audio_backend(asr, backend, cx),
                        ))
                    } else {
                        button.on_click(cx.listener(move |this, _, _, cx| {
                            this.set_audio_backend(asr, backend, cx)
                        }))
                    }
                },
            ),
        )
    }

    /// ASR / TTS 模型选择。
    fn render_audio_model_row(&self, asr: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let (input, output) = if asr {
            (agent::model::Modality::Audio, agent::model::Modality::Text)
        } else {
            (agent::model::Modality::Text, agent::model::Modality::Audio)
        };
        let candidates = self.draft.models_with(&[input], &[output]);
        let selection = if asr {
            ModelSelection {
                provider: self.draft.audio.asr.provider.clone(),
                model: self.draft.audio.asr.model.clone(),
            }
        } else {
            ModelSelection {
                provider: self.draft.audio.tts.provider.clone(),
                model: self.draft.audio.tts.model.clone(),
            }
        };
        let label = if selection.is_set() {
            format!("{} · {}", selection.provider, selection.model)
        } else {
            "未选择模型".to_owned()
        };
        let prefix = if asr {
            "asr"
        } else {
            "tts"
        };
        let form = cx.entity().downgrade();
        Button::new(ElementId::Name(format!("{prefix}-model").into()))
            .outline()
            .border_2()
            .xsmall()
            .label(label)
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.scrollable(true);
                if candidates.is_empty() {
                    return menu.item(PopupMenuItem::label("没有匹配模态的模型"));
                }
                for candidate in &candidates {
                    let form = form.clone();
                    let candidate = candidate.clone();
                    menu = menu.item(
                        PopupMenuItem::new(format!("{} · {}", candidate.provider, candidate.model))
                            .on_click(move |_, _, cx| {
                                form.update(cx, |this, cx| {
                                    this.set_audio_model(asr, candidate.clone(), cx)
                                })
                                .ok();
                            }),
                    );
                }
                menu
            })
    }

    /// 修改语音后端。
    fn set_audio_backend(&mut self, asr: bool, backend: AudioBackend, cx: &mut Context<Self>) {
        if asr {
            self.draft.audio.asr.backend = backend;
        } else {
            self.draft.audio.tts.backend = backend;
        }
        cx.notify();
    }

    /// 修改语音模型。
    fn set_audio_model(&mut self, asr: bool, selection: ModelSelection, cx: &mut Context<Self>) {
        if asr {
            self.draft.audio.asr.provider = selection.provider;
            self.draft.audio.asr.model = selection.model;
        } else {
            self.draft.audio.tts.provider = selection.provider;
            self.draft.audio.tts.model = selection.model;
        }
        cx.notify();
    }

    /// 调整语速。
    fn adjust_tts_rate(&mut self, delta: i32, cx: &mut Context<Self>) {
        self.draft.audio.tts.rate = (self.draft.audio.tts.rate + delta).clamp(-10, 10);
        cx.notify();
    }

    /// 调整音量。
    fn adjust_tts_volume(&mut self, delta: i32, cx: &mut Context<Self>) {
        let volume = self.draft.audio.tts.volume as i32 + delta;
        self.draft.audio.tts.volume = volume.clamp(0, 100) as u8;
        cx.notify();
    }

    /// 两个全局开关：显示 / 默认启用。
    fn render_feature_toggles(
        &self,
        kind: FeatureKind,
        name: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(tool) = MediaTool::from_tool_name(name)
            && !self.draft.media.ready(&self.draft, tool)
        {
            let haloeth = self.haloeth.clone();
            return tinted(
                Button::new(ElementId::Name(format!("media-unready:{name}").into()))
                    .outline()
                    .border_2()
                    .xsmall(),
                Tone::Warning,
                cx,
            )
            .label("未配置模型")
            .tooltip("先在下方选择可用的生成模型")
            .on_click(move |_, window, cx| {
                let haloeth = haloeth.clone();
                window.defer(cx, move |window, cx| {
                    let _ = haloeth.update(cx, |haloeth, cx| {
                        haloeth.open_preferences(
                            crate::preferences::PrefSection::Models,
                            window,
                            cx,
                        )
                    });
                });
            })
            .into_any_element();
        }
        let pref = self.draft.feature_prefs.get(kind, name);
        let visible = !pref.hidden;
        let enabled = pref.default_enabled;
        let visible_name = name.to_owned();
        let default_name = name.to_owned();
        let visible_id =
            ElementId::Name(format!("feature-visible:{}:{name}", kind.prefix()).into());
        let default_id =
            ElementId::Name(format!("feature-default:{}:{name}", kind.prefix()).into());

        h_flex()
            .gap_1()
            .child(
                tinted(
                    Button::new(visible_id).outline().border_2().xsmall(),
                    if visible {
                        Tone::Success
                    } else {
                        Tone::Danger
                    },
                    cx,
                )
                .label(if visible {
                    "显示"
                } else {
                    "已隐藏"
                })
                .selected(visible)
                .toggled(visible)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_visible(kind, &visible_name, cx);
                })),
            )
            .child(
                tinted(
                    Button::new(default_id).outline().border_2().xsmall(),
                    if enabled {
                        Tone::Accent
                    } else {
                        Tone::Neutral
                    },
                    cx,
                )
                .label(if enabled {
                    "默认启用"
                } else {
                    "默认停用"
                })
                .selected(enabled)
                .toggled(enabled)
                .disabled(!visible)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_default(kind, &default_name, cx);
                })),
            )
            .into_any_element()
    }

    /// 工具：内置工具与搜索。
    fn render_tools(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let tools = agent::tools::builtin_tools(&agent::tools::BuiltinOptions::default());
        let rows: Vec<AnyElement> = tools
            .iter()
            .map(|tool| {
                let name = tool.spec().name.clone();
                let description = tool.spec().description.clone();
                let toggles = self.render_feature_toggles(FeatureKind::Tool, &name, cx);
                h_flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .rounded(cx.theme().radius)
                    .border_2()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .child(name),
                            )
                            .child(div().text_xs().text_color(muted).truncate().child(description)),
                    )
                    .child(toggles)
                    .into_any_element()
            })
            .collect();
        let search_enabled = self.draft.search.enabled;
        let search_toggles = self.render_feature_toggles(FeatureKind::Tool, "web_search", cx);

        v_flex()
            .gap_2()
            .child(section_title("内置工具", cx))
            .child(
                div().text_xs().text_color(muted).child(
                    "「显示」决定是否在任何会话中出现；「默认启用」决定新建会话的初始状态。",
                ),
            )
            .children(rows)
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .rounded(cx.theme().radius)
                    .border_2()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .child("web_search"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted)
                                    .truncate()
                                    .child("搜索使用提供商自带的搜索接口；未配置提供商时不可用。"),
                            ),
                    )
                    .child(search_toggles)
                    .child(
                        tinted(
                            Button::new("settings-search").outline().border_2().xsmall(),
                            if search_enabled {
                                Tone::Success
                            } else {
                                Tone::Danger
                            },
                            cx,
                        )
                        .label(if search_enabled {
                            "搜索接口：开"
                        } else {
                            "搜索接口：关"
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_search(cx))),
                    ),
            )
            .child(section_title("生成工具", cx))
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("产物路径由模型在工具参数里按用户要求填写，写入前照常审核；智谱走 images 接口，百炼自动使用原生异步任务。"),
            )
            .children(MediaTool::ALL.into_iter().map(|kind| {
                let media = self.draft.media.tool(kind).clone();
                let label = if media.is_configured() {
                    format!("{} · {} / {}", kind.label(), media.provider, media.model)
                } else {
                    format!("{} · 未配置模型", kind.label())
                };
                let candidates = self.draft.models_with(&[kind.input()], &[kind.output()]);
                let toggles = self.render_feature_toggles(FeatureKind::Tool, kind.tool_name(), cx);
                let form = cx.entity().downgrade();
                let mut row = h_flex().items_center().gap_2();
                row = row.child(toggles);
                row = row.child(
                    Button::new(ElementId::Name(
                        format!("media-model:{}", kind.tool_name()).into(),
                    ))
                    .outline()
                    .border_2()
                    .xsmall()
                    .label(label)
                    .dropdown_menu(move |menu, _, _| {
                        let mut menu = menu.scrollable(true);
                        if candidates.is_empty() {
                            return menu.item(PopupMenuItem::label("没有匹配输出模态的模型"));
                        }
                        for selection in &candidates {
                            let form = form.clone();
                            let selection = selection.clone();
                            menu = menu.item(
                                PopupMenuItem::new(format!(
                                    "{} · {}",
                                    selection.provider, selection.model
                                ))
                                .on_click(move |_, _, cx| {
                                    form.update(cx, |this, cx| {
                                        this.set_media_model(kind, selection.clone(), cx)
                                    })
                                    .ok();
                                }),
                            );
                        }
                        menu
                    }),
                );
                if matches!(kind, MediaTool::Video | MediaTool::Music) {
                    let state = if matches!(kind, MediaTool::Video) {
                        &self.video_endpoint
                    } else {
                        &self.music_endpoint
                    };
                    let id = match kind {
                        MediaTool::Video => "video-endpoint",
                        _ => "music-endpoint",
                    };
                    row = row.child(input_frame(state, id, true, cx).flex_1());
                }
                row = row.child(
                    Button::new(ElementId::Name(
                        format!("media-advanced:{}", kind.tool_name()).into(),
                    ))
                    .ghost()
                    .xsmall()
                    .label("高级…")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_media_form(kind, window, cx)
                    })),
                );
                row.into_any_element()
            }))
    }

    /// 应用生成工具的高级设置（保留已选的提供商与模型）。
    pub(crate) fn set_media_advanced(
        &mut self,
        kind: MediaTool,
        advanced: crate::config::MediaToolConfig,
        cx: &mut Context<Self>,
    ) {
        let media = self.draft.media.tool_mut(kind);
        let provider = media.provider.clone();
        let model = media.model.clone();
        *media = crate::config::MediaToolConfig {
            provider,
            model,
            ..advanced
        };
        cx.notify();
    }

    /// 打开生成工具高级设置。
    fn open_media_form(&mut self, kind: MediaTool, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.draft.media.tool(kind).clone();
        let settings = cx.weak_entity();
        let form =
            cx.new(|cx| crate::media_form::MediaForm::new(window, settings, kind, current, cx));
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .border_2()
                .title(format!("{} 高级设置", kind.label()))
                .overlay_closable(false)
                .child(form.clone())
                .footer(form.update(cx, |form, cx| form.render_footer(cx).into_any_element()))
        });
    }

    /// 设置生成工具使用的模型。
    fn set_media_model(
        &mut self,
        kind: MediaTool,
        selection: ModelSelection,
        cx: &mut Context<Self>,
    ) {
        let media = self.draft.media.tool_mut(kind);
        media.provider = selection.provider;
        media.model = selection.model;
        cx.notify();
    }

    /// MCP：服务器定义。
    fn render_mcp(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let rows: Vec<AnyElement> = self
            .draft
            .mcp_servers
            .iter()
            .enumerate()
            .map(|(index, server)| {
                let name = server.name.clone();
                let detail = match &server.transport {
                    McpTransport::Stdio {
                        command,
                        args,
                        ..
                    } => format!("stdio · {command} {}", args.join(" ")),
                    McpTransport::Http {
                        url,
                        ..
                    } => format!("http · {url}"),
                };
                let toggles = self.render_feature_toggles(FeatureKind::Mcp, &name, cx);
                h_flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .rounded(cx.theme().radius)
                    .border_2()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_sm().child(name.clone()))
                            .child(div().text_xs().text_color(muted).truncate().child(detail)),
                    )
                    .child(toggles)
                    .child(
                        Button::new(ElementId::Name(format!("mcp-remove:{name}").into()))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Trash)
                            .tooltip("删除服务器")
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.remove_server(index, cx)),
                            ),
                    )
                    .into_any_element()
            })
            .collect();

        v_flex()
            .gap_2()
            .child(section_title("MCP 服务器", cx))
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("stdio 服务器是以当前用户身份运行的本地程序，添加即表示信任其命令。"),
            )
            .children(rows)
            .child(
                Button::new("mcp-new")
                    .outline()
                    .border_2()
                    .xsmall()
                    .icon(IconName::Plus)
                    .label("新建服务器…")
                    .on_click(cx.listener(|this, _, window, cx| this.open_mcp_form(window, cx))),
            )
    }

    /// Skill：根目录、扫描结果与新建。
    fn render_skills(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let default_dir = self
            .draft
            .default_skill_dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_else(|| "未找到系统配置目录".to_owned());

        let dir_rows: Vec<AnyElement> = self
            .draft
            .skills
            .dirs
            .iter()
            .enumerate()
            .map(|(index, dir)| {
                let available = std::path::Path::new(dir).is_dir();
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(if available {
                                muted
                            } else {
                                cx.theme().danger
                            })
                            .child(dir.clone()),
                    )
                    .child(
                        Button::new(ElementId::Name(format!("skill-dir-remove:{index}").into()))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Trash)
                            .tooltip("移除目录")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_skill_dir(index, cx);
                            })),
                    )
                    .into_any_element()
            })
            .collect();

        let skill_rows: Vec<AnyElement> = self
            .discovered
            .iter()
            .map(|skill| {
                let name = skill.name.clone();
                let open_name = name.clone();
                let delete_name = name.clone();
                let description = skill.description.clone();
                let root = skill.root.clone();
                let delete_root = root.clone();
                let toggles = self.render_feature_toggles(FeatureKind::Skill, &name, cx);
                h_flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .rounded(cx.theme().radius)
                    .border_2()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_sm().child(name.clone()))
                            .child(div().text_xs().text_color(muted).truncate().child(description)),
                    )
                    .child(toggles)
                    .child(
                        Button::new(ElementId::Name(format!("skill-open:{open_name}").into()))
                            .ghost()
                            .xsmall()
                            .icon(IconName::FolderOpen)
                            .tooltip("打开目录")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_skill_dir(&root, cx);
                            })),
                    )
                    .child(
                        Button::new(ElementId::Name(format!("skill-delete:{delete_name}").into()))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Trash)
                            .tooltip("删除 Skill")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_delete_skill(
                                    delete_name.clone(),
                                    delete_root.clone(),
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .into_any_element()
            })
            .collect();

        v_flex()
            .gap_2()
            .child(section_title("Skill 目录", cx))
            .child(div().text_xs().text_color(muted).child(format!("默认目录：{default_dir}")))
            .children(dir_rows)
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(input_frame(&self.skill_dir, "skill-dir", true, cx).flex_1())
                    .child(
                        Button::new("skill-dir-pick")
                            .outline()
                            .border_2()
                            .xsmall()
                            .icon(IconName::FolderOpen)
                            .label("选择…")
                            .tooltip("选择 Skill 根目录")
                            .on_click(cx.listener(|this, _, window, cx| {
                                let skill_dir = this.skill_dir.clone();
                                pick_folder(
                                    window,
                                    cx,
                                    "选择 Skill 根目录",
                                    move |_, path, window, cx| {
                                        skill_dir
                                            .update(cx, |state, cx| state.set_value(path, window, cx));
                                    },
                                );
                            })),
                    )
                    .child(
                        Button::new("skill-dir-add")
                            .outline()
                            .border_2()
                            .xsmall()
                            .icon(IconName::Plus)
                            .label("添加目录")
                            .on_click(cx.listener(|this, _, _, cx| this.add_skill_dir(cx))),
                    )
                    .child(
                        Button::new("skill-rescan")
                            .ghost()
                            .xsmall()
                            .icon(IconName::RefreshCw)
                            .label("重新扫描")
                            .on_click(cx.listener(|this, _, _, cx| this.rescan(cx))),
                    ),
            )
            .child(section_title("已发现的 Skill", cx))
            .child(
                div().text_xs().text_color(muted).child(format!(
                    "Skill 是包含 {SKILL_FILE} 的目录；正文只是指令，附带脚本仍通过 Shell 工具执行并走审核。"
                )),
            )
            .when(skill_rows.is_empty(), |this| {
                this.child(
                    div()
                        .id("skills-empty")
                        .role(Role::Status)
                        .test_support()
                        .aria_label("尚未发现 Skill")
                        .text_xs()
                        .text_color(muted)
                        .child("尚未发现 Skill。"),
                )
            })
            .children(skill_rows)
            .child(
                Button::new("skill-new")
                    .outline()
                    .border_2()
                    .xsmall()
                    .icon(IconName::Plus)
                    .label("新建 Skill…")
                    .on_click(cx.listener(|this, _, window, cx| this.open_skill_form(window, cx))),
            )
    }
}

impl SettingsForm {
    /// 渲染当前分页内容；偏好窗口与对话框共用。
    pub(crate) fn render_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        match self.tab {
            SettingsTab::General => self.render_general(cx).into_any_element(),
            SettingsTab::Tools => self.render_tools(cx).into_any_element(),
            SettingsTab::Mcp => self.render_mcp(cx).into_any_element(),
            SettingsTab::Skill => self.render_skills(cx).into_any_element(),
            SettingsTab::Audio => self.render_audio(cx).into_any_element(),
        }
    }
}

impl Render for SettingsForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let tab = self.tab;
        let page = self.render_page(cx);

        let base = v_flex()
            .id("settings-panel")
            .test_support()
            .gap_4()
            .when(!self.auto_save, |this| this.max_h(rems(26.)).overflow_y_scroll());
        if self.auto_save {
            return base.child(page);
        }
        base.child(self.render_tabs(cx))
            .child(
                div()
                    .id("settings-description")
                    .role(Role::Status)
                    .test_support()
                    .aria_label(tab.description())
                    .text_sm()
                    .text_color(muted)
                    .child(tab.description()),
            )
            .child(page)
    }
}

/// 语音测试状态文案。
fn msg(status: SpeechStatus) -> &'static str {
    match status {
        SpeechStatus::Idle => "未在录音",
        SpeechStatus::Connecting => "正在打开麦克风…",
        SpeechStatus::Recording => "录音中，再次点击停止",
        SpeechStatus::Stopping => "正在识别…",
    }
}

/// 小节标题。
fn section_title(title: &str, cx: &App) -> impl IntoElement {
    div()
        .text_sm()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(cx.theme().foreground)
        .child(title.to_owned())
}

/// 是否是 F1–F24：功能键单独使用不会和打字冲突。
fn is_function_key(key: &str) -> bool {
    key.strip_prefix('f')
        .and_then(|number| number.parse::<u32>().ok())
        .is_some_and(|number| (1..=24).contains(&number))
}

#[cfg(test)]
mod tests {
    use super::SettingsTab;

    #[test]
    fn tabs_have_stable_ids() {
        assert_eq!(SettingsTab::General.id(), "general");
        assert_eq!(SettingsTab::Tools.id(), "tools");
        assert_eq!(SettingsTab::Mcp.id(), "mcp");
        assert_eq!(SettingsTab::Skill.id(), "skill");
    }
}
