//! Haloeth 主视图：标题栏、对话区、输入缓冲区与状态栏的组合。
//!
//! 应用内所有边框统一 2px，一律使用 GPUI 的 `border_2` 系列缩放方法；
//! 组件自带的 1px 边框在调用处覆盖。

mod access;
mod composer;
mod config;
mod conversation;
mod models;
mod session;
mod session_store;
mod sessions;
mod settings;
mod ui;

use access::AccessForm;
use composer::{Composer, ComposerEvent};
use conversation::{ChatRole, Conversation, ConversationEvent};
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
use session_store::{SessionFile, SessionStore};
use sessions::SessionsForm;
use settings::{SettingsForm, SettingsTab};
use ui::{Tone, chip, tinted};

actions!(
    haloeth,
    [NewSession, ClearSession, ShowSessions, ShowModels, ShowSettings, ShowAbout, Quit]
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
    /// 上一次观察到的运行状态，用于在终态触发保存
    last_status: Status,
    _composer_subscription: Subscription,
    _conversation_subscription: Subscription,
    _conversation_observation: Subscription,
    _composer_observation: Subscription,
    _session_observation: Subscription,
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
        let composer = cx.new(|cx| Composer::new(window, cx));

        // 启动恢复：上次会话 → 最近更新的会话 → 新建。
        let active = Self::restore_session(&store, cx, &session, &conversation);

        let subscription = cx.subscribe(&composer, |this, _composer, event: &ComposerEvent, cx| {
            let ComposerEvent::Submit(text) = event;
            this.conversation.update(cx, |conversation, cx| {
                conversation.push(ChatRole::User, text.clone(), cx);
            });
            this.session.update(cx, |session, cx| session.submit(text.to_string(), cx));
            this.save_active(cx);
        });
        // 审批卡片上的按钮回到会话，由会话把决定交给挂起中的审批
        let conversation_subscription =
            cx.subscribe(&conversation, |this, _, event: &ConversationEvent, cx| {
                let ConversationEvent::ApprovalDecision {
                    id,
                    decision,
                } = event;
                this.session.update(cx, |session, cx| {
                    session.resolve_approval(id, *decision, cx);
                });
                this.save_active(cx);
            });
        // 底部栏的消息数、会话状态与发送按钮可用性跟随各自状态变化
        let observation = cx.observe(&conversation, |_, _, cx| cx.notify());
        let composer_observation = cx.observe(&composer, |_, _, cx| cx.notify());
        let appearance_subscription = cx.observe_window_appearance(window, |this, window, cx| {
            let preference = this.session.read(cx).config().ui.theme;
            if preference == config::ThemePreference::System {
                ui::apply_theme(preference, Some(window), cx);
            }
        });
        let session_observation = cx.observe(&session, |this, session, cx| {
            let status = session.read(cx).status();
            // 运行进入终态或等待审批时落盘，避免流式增量逐帧写文件。
            if status != this.last_status {
                this.last_status = status;
                if matches!(status, Status::Idle | Status::Failed | Status::AwaitingApproval) {
                    this.save_active(cx);
                }
            }
            cx.notify();
        });

        // 呼出即输入：窗口就绪后把焦点交给输入缓冲区
        let buffer = composer.read(cx).buffer().clone();
        window.defer(cx, move |window, cx| {
            buffer.update(cx, |buffer, cx| buffer.focus(window, cx));
        });

        let initial_title = SharedString::from(active.title.clone());
        Self {
            menu_bar: AppMenuBar::new(cx),
            conversation,
            composer,
            session,
            store,
            active,
            title: initial_title,
            last_status: Status::Idle,
            _composer_subscription: subscription,
            _conversation_subscription: conversation_subscription,
            _conversation_observation: observation,
            _composer_observation: composer_observation,
            _session_observation: session_observation,
            _appearance_subscription: appearance_subscription,
        }
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
        let (context, usage, selection, grants, rules, last_session) = {
            let session = self.session.read(cx);
            (
                session.context().clone(),
                session.usage(),
                session
                    .selection()
                    .map(|(provider, model)| (provider.to_owned(), model.to_owned())),
                session.grants().to_vec(),
                session.always_allow().to_vec(),
                session.config().storage.last_session.clone(),
            )
        };
        self.active.transcript = transcript;
        self.active.context = context;
        self.active.usage = usage;
        self.active.grants = grants;
        self.active.rules = rules;
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
    fn create_session(&mut self, cx: &mut Context<Self>) {
        if self.session.read(cx).status().is_busy() {
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

    /// 打开设置对话框，并定位到指定分页。
    fn open_settings(&mut self, tab: SettingsTab, window: &mut Window, cx: &mut Context<Self>) {
        let config = self.session.read(cx).config().clone();
        let session = self.session.downgrade();
        let haloeth = cx.weak_entity();
        let form = cx.new(|cx| SettingsForm::new(window, &config, session, haloeth, tab, cx));

        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .border_2()
                .title("设置")
                .overlay_closable(false)
                .child(form.clone())
                .footer(form.update(cx, |form, cx| form.render_footer(cx).into_any_element()))
        });
    }

    /// 打开设置对话框（通用分页）。
    fn show_settings(&mut self, _: &ShowSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(SettingsTab::General, window, cx);
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

    /// 切换审核模式。
    fn set_approval_mode(&mut self, mode: agent::approval::ApprovalMode, cx: &mut Context<Self>) {
        self.update_config(cx, |config| config.approval.mode = mode);
    }

    /// 切换内置工具。
    fn toggle_tool(&mut self, name: &str, cx: &mut Context<Self>) {
        let name = name.to_owned();
        self.update_config(cx, move |config| {
            if let Some(index) = config.tools.disabled.iter().position(|entry| entry == &name) {
                config.tools.disabled.remove(index);
            } else {
                config.tools.disabled.push(name);
            }
        });
    }

    /// 切换 MCP 服务器。
    fn toggle_mcp(&mut self, name: &str, cx: &mut Context<Self>) {
        let name = name.to_owned();
        self.update_config(cx, move |config| {
            if let Some(server) = config.mcp_servers.iter_mut().find(|server| server.name == name) {
                server.enabled = !server.enabled;
            }
        });
    }

    /// 切换 Skill。
    fn toggle_skill(&mut self, name: &str, cx: &mut Context<Self>) {
        let discovered: Vec<String> =
            agent::skill::discover(&self.session.read(cx).config().skill_dirs())
                .into_iter()
                .map(|skill| skill.name)
                .collect();
        let name = name.to_owned();
        self.update_config(cx, move |config| {
            if config.skills.enabled.is_empty() {
                config.skills.enabled = discovered;
            }
            if let Some(index) = config.skills.enabled.iter().position(|entry| entry == &name) {
                config.skills.enabled.remove(index);
            } else {
                config.skills.enabled.push(name);
            }
        });
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
    }

    /// 新建空会话但不保存当前会话（删除当前会话时使用）。
    fn create_blank(&mut self, cx: &mut Context<Self>) {
        let (provider, model) = self
            .session
            .read(cx)
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
        cx.notify();
        Ok(())
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

    /// 当前会话按钮，点击打开会话对话框
    fn render_session_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        Button::new("session")
            .ghost()
            .small()
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
                    .menu_with_icon("模型…", IconName::BrainCircuit, Box::new(ShowModels))
                    .menu_with_icon("设置…", IconName::Settings, Box::new(ShowSettings))
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
                .child(self.render_session_button(cx))
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
            .max_w(rems(10.))
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

    /// 左下角「更多」：快速查看与切换工具、MCP、Skill，并进入对应设置。
    fn render_more_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let config = self.session.read(cx).config().clone();
        let tools = agent::tools::builtin_tools(&agent::tools::BuiltinOptions::default());
        let tool_rows: Vec<(String, bool)> = tools
            .iter()
            .map(|tool| {
                let name = tool.spec().name.clone();
                let enabled = !config.tools.disabled.iter().any(|entry| entry == &name);
                (name, enabled)
            })
            .collect();
        let mcp_rows: Vec<(String, bool)> =
            config.mcp_servers.iter().map(|server| (server.name.clone(), server.enabled)).collect();
        let skill_rows: Vec<(String, bool)> = agent::skill::discover(&config.skill_dirs())
            .iter()
            .map(|skill| {
                let enabled = config.skills.enabled.is_empty()
                    || config.skills.enabled.iter().any(|entry| entry == &skill.name);
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

        Button::new("more")
            .ghost()
            .small()
            .compact()
            .label("⋯")
            .tooltip("快速设置：工具、MCP、Skill 与目录")
            .accessibility_label("快速设置")
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.scrollable(true);

                menu = menu.item(PopupMenuItem::new(tool_summary.clone()).on_click({
                    let haloeth = haloeth.clone();
                    move |_, window, cx| {
                        haloeth
                            .update(cx, |this, cx| {
                                this.open_settings(SettingsTab::Tools, window, cx)
                            })
                            .ok();
                    }
                }));
                for (name, enabled) in &tool_rows {
                    let haloeth = haloeth.clone();
                    let id = name.clone();
                    menu = menu.item(
                        PopupMenuItem::new(format!(
                            "{name}  ·  {}",
                            if *enabled {
                                "已启用"
                            } else {
                                "已停用"
                            }
                        ))
                        .checked(*enabled)
                        .on_click(move |_, _, cx| {
                            haloeth.update(cx, |this, cx| this.toggle_tool(&id, cx)).ok();
                        }),
                    );
                }

                menu = menu.separator();
                menu = menu.item(PopupMenuItem::new(mcp_summary.clone()).on_click({
                    let haloeth = haloeth.clone();
                    move |_, window, cx| {
                        haloeth
                            .update(cx, |this, cx| this.open_settings(SettingsTab::Mcp, window, cx))
                            .ok();
                    }
                }));
                for (name, enabled) in &mcp_rows {
                    let haloeth = haloeth.clone();
                    let id = name.clone();
                    menu = menu.item(
                        PopupMenuItem::new(format!(
                            "{name}  ·  {}",
                            if *enabled {
                                "已启用"
                            } else {
                                "已停用"
                            }
                        ))
                        .checked(*enabled)
                        .on_click(move |_, _, cx| {
                            haloeth.update(cx, |this, cx| this.toggle_mcp(&id, cx)).ok();
                        }),
                    );
                }

                menu = menu.separator();
                menu = menu.item(PopupMenuItem::new(skill_summary.clone()).on_click({
                    let haloeth = haloeth.clone();
                    move |_, window, cx| {
                        haloeth
                            .update(cx, |this, cx| {
                                this.open_settings(SettingsTab::Skill, window, cx)
                            })
                            .ok();
                    }
                }));
                for (name, enabled) in &skill_rows {
                    let haloeth = haloeth.clone();
                    let id = name.clone();
                    menu = menu.item(
                        PopupMenuItem::new(format!(
                            "{name}  ·  {}",
                            if *enabled {
                                "已启用"
                            } else {
                                "已停用"
                            }
                        ))
                        .checked(*enabled)
                        .on_click(move |_, _, cx| {
                            haloeth.update(cx, |this, cx| this.toggle_skill(&id, cx)).ok();
                        }),
                    );
                }

                menu = menu.separator();
                menu = menu.item(PopupMenuItem::new("目录授权…").on_click({
                    let haloeth = haloeth.clone();
                    move |_, window, cx| {
                        haloeth.update(cx, |this, cx| this.show_access(window, cx)).ok();
                    }
                }));
                menu.item(PopupMenuItem::new("设置…").on_click({
                    let haloeth = haloeth.clone();
                    move |_, window, cx| {
                        haloeth
                            .update(cx, |this, cx| {
                                this.open_settings(SettingsTab::General, window, cx)
                            })
                            .ok();
                    }
                }))
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
                menu =
                    menu.item(PopupMenuItem::new(value.label()).checked(value == mode).on_click(
                        move |_, _, cx| {
                            haloeth.update(cx, |this, cx| this.set_approval_mode(value, cx)).ok();
                        },
                    ));
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
    fn render_bottom_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.session.read(cx).status();
        let message_count = self.conversation.read(cx).message_count();
        let total_tokens = self.session.read(cx).usage().total();
        let quick = h_flex()
            .items_center()
            .gap_1()
            .child(self.render_more_menu(cx))
            .child(self.render_approval_chip(cx))
            .child(self.render_access_chip(cx));

        StatusBar::new()
            .border_t_2()
            .child(quick)
            .right(self.render_model_picker(cx))
            .right(chip("session-state", status.label(), status_tone(status), cx))
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
            .tooltip(composer::SUBMIT_HINT)
            .accessibility_label("发送")
            .disabled(!self.composer.read(cx).is_submittable())
            .on_click(cx.listener(|this, _, window, cx| {
                this.composer.update(cx, |composer, cx| composer.submit(window, cx));
            }))
            .into_any_element()
    }
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
            .on_action(cx.listener(Self::show_models))
            .on_action(cx.listener(Self::show_settings))
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
    use agent::error::ModelError;
    use agent::message::{Message, StreamDelta, ToolCall};
    use agent::model::{GenerateRequest, Model, ModelBuilder};
    use gpui_kit::component::Root;
    use gpui_kit::test::TestAppContextExt as _;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{
        AnyWindowHandle, AppContext as _, Entity, Pixels, Role, Size, TestAppContext, Window,
        point, px,
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
        let (_haloeth, handle) = open_shell(cx, WINDOW_SIZE);

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("more", cx);
        })
        .unwrap();
        cx.run_until_parked();

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let mut menu = window.within(r#"popover:dropdown-menu:Name("more")"#);
            // 第一项是「工具 · n / m 启用 →」。
            menu.click(gpui_kit::ElementId::Integer(0), cx);
        })
        .unwrap();
        settle_dialog(cx, handle).await;

        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            let dialog = window.within("dialog");
            assert!(dialog.find("settings-panel").visible());
            assert!(dialog.try_find("tool-toggle:shell").is_some());
        })
        .unwrap();
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
                assert!(dialog.find("settings-reviewer").visible());
                dialog.click("settings-tab:tools", cx);
            }
            window.render_frame(cx);
            {
                let mut dialog = window.within("dialog");
                // 分页内容可能超出首屏，存在即可。
                assert!(dialog.try_find("tool-toggle:shell").is_some());
                assert!(dialog.try_find("settings-search").is_some());
                dialog.click("settings-tab:mcp", cx);
            }
            window.render_frame(cx);
            window.within("dialog").click("settings-tab:skill", cx);
            window.render_frame(cx);
            let dialog = window.within("dialog");
            assert!(dialog.try_find("skill-dir-add").is_some());
            assert!(dialog.try_find("skill-create").is_some());
        })
        .unwrap();
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
