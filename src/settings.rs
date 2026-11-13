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
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _, WindowExt as _};
use gpui_kit::prelude::{FluentBuilder as _, InteractiveElement as _};
use gpui_kit::*;

use crate::Haloeth;
use crate::config::{Config, ThemePreference};
use crate::session::Session;
use crate::ui::{Tone, cancel_button, tinted};

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
}

impl SettingsTab {
    /// 分页标题。
    pub fn label(self) -> &'static str {
        match self {
            Self::General => "通用",
            Self::Tools => "工具",
            Self::Mcp => "MCP",
            Self::Skill => "Skill",
        }
    }

    /// 分页标识。
    pub fn id(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Tools => "tools",
            Self::Mcp => "mcp",
            Self::Skill => "skill",
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
    /// 审核模型的提供商与模型
    reviewer_provider: Entity<InputState>,
    reviewer_model: Entity<InputState>,
    /// 新增 MCP 服务器
    mcp_name: Entity<InputState>,
    mcp_target: Entity<InputState>,
    /// 新增 Skill 根目录
    skill_dir: Entity<InputState>,
    /// 新建 Skill
    skill_name: Entity<InputState>,
    skill_description: Entity<InputState>,
    /// 已发现的 Skill
    discovered: Vec<skill::Skill>,
    error: Option<SharedString>,
}

impl SettingsForm {
    /// 构造设置面板。
    pub fn new(
        window: &mut Window,
        config: &Config,
        session: WeakEntity<Session>,
        haloeth: WeakEntity<Haloeth>,
        tab: SettingsTab,
        cx: &mut Context<Self>,
    ) -> Self {
        let storage =
            cx.new(|cx| InputState::new(window, cx).placeholder("会话目录（留空使用系统默认）"));
        storage.update(cx, |state, cx| {
            state.set_value(config.storage.sessions_dir.clone(), window, cx)
        });
        let reviewer_provider = cx.new(|cx| {
            InputState::new(window, cx).placeholder("提供商名称（留空表示复用当前模型）")
        });
        reviewer_provider.update(cx, |state, cx| {
            state.set_value(config.approval.reviewer.provider.clone(), window, cx)
        });
        let reviewer_model = cx.new(|cx| InputState::new(window, cx).placeholder("审核模型标识"));
        reviewer_model.update(cx, |state, cx| {
            state.set_value(config.approval.reviewer.model.clone(), window, cx)
        });
        let skill_dir = cx.new(|cx| InputState::new(window, cx).placeholder("额外的 Skill 根目录"));
        let mcp_name = cx.new(|cx| InputState::new(window, cx).placeholder("服务器名称"));
        let mcp_target =
            cx.new(|cx| InputState::new(window, cx).placeholder("命令与参数，或 http(s) 地址"));
        let skill_name = cx.new(|cx| InputState::new(window, cx).placeholder("Skill 名称"));
        let skill_description = cx.new(|cx| InputState::new(window, cx).placeholder("一句话描述"));

        let draft = config.clone();
        let discovered = skill::discover(&draft.skill_dirs());
        Self {
            draft,
            session,
            haloeth,
            tab,
            storage,
            reviewer_provider,
            reviewer_model,
            mcp_name,
            mcp_target,
            skill_dir,
            skill_name,
            skill_description,
            discovered,
            error: None,
        }
    }

    /// 切换分页。
    fn set_tab(&mut self, tab: SettingsTab, cx: &mut Context<Self>) {
        self.tab = tab;
        cx.notify();
    }

    /// 保存：校验、写盘、应用到会话。
    pub fn save(&mut self, cx: &mut Context<Self>) -> bool {
        let storage = self.storage.read(cx).value().trim().to_owned();
        if !storage.is_empty() && !std::path::Path::new(&storage).is_dir() {
            self.error = Some("会话目录必须是一个已存在的目录".into());
            self.tab = SettingsTab::General;
            cx.notify();
            return false;
        }
        for dir in &self.draft.skills.dirs {
            if !std::path::Path::new(dir.trim()).is_dir() {
                self.error = Some(format!("Skill 目录不存在：{dir}").into());
                self.tab = SettingsTab::Skill;
                cx.notify();
                return false;
            }
        }
        self.draft.storage.sessions_dir = storage;
        self.draft.approval.reviewer.provider =
            self.reviewer_provider.read(cx).value().trim().to_owned();
        self.draft.approval.reviewer.model = self.reviewer_model.read(cx).value().trim().to_owned();
        self.draft.approval.reviewer.use_current = self.draft.approval.reviewer.provider.is_empty();

        if let Err(error) = self.draft.save() {
            self.error = Some(format!("保存失败：{error}").into());
            cx.notify();
            return false;
        }
        let config = self.draft.clone();
        self.session.update(cx, |session, cx| session.apply_config(config, cx)).ok();
        self.haloeth.update(cx, |haloeth, cx| haloeth.save_active(cx)).ok();
        true
    }

    /// 切换审核模型的来源。
    fn toggle_reviewer(&mut self, cx: &mut Context<Self>) {
        let reviewer = &mut self.draft.approval.reviewer;
        reviewer.use_current = !reviewer.use_current;
        cx.notify();
    }

    /// 切换外观并立即预览。
    fn set_theme(&mut self, theme: ThemePreference, cx: &mut Context<Self>) {
        self.draft.ui.theme = theme;
        crate::ui::apply_theme(theme, None, cx);
        cx.notify();
    }

    /// 切换搜索。
    fn toggle_search(&mut self, cx: &mut Context<Self>) {
        self.draft.search.enabled = !self.draft.search.enabled;
        cx.notify();
    }

    /// 切换内置工具的启用状态。
    fn toggle_tool(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(index) = self.draft.tools.disabled.iter().position(|entry| entry == name) {
            self.draft.tools.disabled.remove(index);
        } else {
            self.draft.tools.disabled.push(name.to_owned());
        }
        cx.notify();
    }

    /// 切换单个 MCP 服务器的启用状态。
    fn toggle_server(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(server) = self.draft.mcp_servers.get_mut(index) {
            server.enabled = !server.enabled;
            cx.notify();
        }
    }

    /// 删除 MCP 服务器。
    fn remove_server(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.draft.mcp_servers.len() {
            self.draft.mcp_servers.remove(index);
            cx.notify();
        }
    }

    /// 新增 MCP 服务器。
    fn add_server(&mut self, cx: &mut Context<Self>) {
        let name = self.mcp_name.read(cx).value().trim().to_owned();
        let target = self.mcp_target.read(cx).value().trim().to_owned();
        if name.is_empty() || target.is_empty() {
            self.error = Some("请填写服务器名称与命令或地址".into());
            cx.notify();
            return;
        }
        if self.draft.mcp_servers.iter().any(|server| server.name == name) {
            self.error = Some(format!("服务器「{name}」已存在").into());
            cx.notify();
            return;
        }
        let transport = if target.starts_with("http://") || target.starts_with("https://") {
            McpTransport::Http {
                url: target,
                headers: Default::default(),
            }
        } else {
            let mut parts = target.split_whitespace();
            let command = parts.next().unwrap_or_default().to_owned();
            McpTransport::Stdio {
                command,
                args: parts.map(str::to_owned).collect(),
                env: Default::default(),
            }
        };
        self.draft.mcp_servers.push(McpServerConfig {
            name,
            enabled: true,
            transport,
        });
        self.error = None;
        cx.notify();
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

    /// 切换 Skill 的启用状态。
    fn toggle_skill(&mut self, name: &str, cx: &mut Context<Self>) {
        // 启用列表为空表示「全部启用」，第一次显式切换时先展开成完整列表。
        if self.draft.skills.enabled.is_empty() {
            self.draft.skills.enabled =
                self.discovered.iter().map(|skill| skill.name.clone()).collect();
        }
        if let Some(index) = self.draft.skills.enabled.iter().position(|entry| entry == name) {
            self.draft.skills.enabled.remove(index);
        } else {
            self.draft.skills.enabled.push(name.to_owned());
        }
        cx.notify();
    }

    /// 新建一个 Skill 骨架。
    fn create_skill(&mut self, cx: &mut Context<Self>) {
        let name = self.skill_name.read(cx).value().trim().to_owned();
        let description = self.skill_description.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.error = Some("请填写 Skill 名称".into());
            cx.notify();
            return;
        }
        let Some(dir) =
            self.draft.skill_dirs().into_iter().next().or_else(|| self.draft.default_skill_dir())
        else {
            self.error = Some("找不到 Skill 目录".into());
            cx.notify();
            return;
        };
        match skill::create_skill(&dir, &name, &description) {
            Ok(created) => {
                if self.draft.skills.enabled.is_empty() {
                    self.draft.skills.enabled =
                        self.discovered.iter().map(|skill| skill.name.clone()).collect();
                }
                if !self.draft.skills.enabled.iter().any(|entry| entry == &name) {
                    self.draft.skills.enabled.push(name.clone());
                }
                self.discovered = skill::discover(&self.draft.skill_dirs());
                self.error = None;
                tracing::info!("已创建 Skill：{}", created.display());
            }
            Err(error) => {
                self.error = Some(format!("创建 Skill 失败：{error}").into());
            }
        }
        cx.notify();
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
        let tabs = [SettingsTab::General, SettingsTab::Tools, SettingsTab::Mcp, SettingsTab::Skill];
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

    /// 通用：外观、审核模型、网络策略与存储。
    fn render_general(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
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
                    tinted(button, Tone::Accent, cx)
                        .on_click(cx.listener(move |this, _, _, cx| this.set_theme(theme, cx)))
                } else {
                    button.on_click(cx.listener(move |this, _, _, cx| this.set_theme(theme, cx)))
                }
            })))
            .child(section_title("审核", cx))
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
                        this.child(Input::new(&self.reviewer_provider).cleanable(true))
                            .child(Input::new(&self.reviewer_model).cleanable(true))
                    }),
            )
            .child(section_title("会话存储", cx))
            .child(div().text_xs().text_color(muted).child(format!("当前位置：{storage_path}")))
            .child(Input::new(&self.storage).cleanable(true))
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("修改后新会话写入新位置，旧目录中的会话不会自动迁移。"),
            )
    }

    /// 工具：内置工具与搜索。
    fn render_tools(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let tools = agent::tools::builtin_tools(&agent::tools::BuiltinOptions::default());
        let disabled = self.draft.tools.disabled.clone();
        let rows: Vec<AnyElement> = tools
            .iter()
            .map(|tool| {
                let name = tool.spec().name.clone();
                let description = tool.spec().description.clone();
                let enabled = !disabled.iter().any(|entry| entry == &name);
                let toggle_name = name.clone();
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
                                    .child(name.clone()),
                            )
                            .child(div().text_xs().text_color(muted).truncate().child(description)),
                    )
                    .child(
                        tinted(
                            Button::new(ElementId::Name(format!("tool-toggle:{name}").into()))
                                .outline()
                                .border_2()
                                .xsmall(),
                            if enabled {
                                Tone::Success
                            } else {
                                Tone::Danger
                            },
                            cx,
                        )
                        .label(if enabled {
                            "已启用"
                        } else {
                            "已停用"
                        })
                        .selected(enabled)
                        .toggled(enabled)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_tool(&toggle_name, cx);
                        })),
                    )
                    .into_any_element()
            })
            .collect();
        let search_enabled = self.draft.search.enabled;

        v_flex().gap_2().child(section_title("内置工具", cx)).children(rows).child(
            h_flex()
                .items_center()
                .gap_2()
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
                        "web_search：已启用"
                    } else {
                        "web_search：已停用"
                    })
                    .selected(search_enabled)
                    .toggled(search_enabled)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_search(cx))),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child("搜索使用提供商自带的搜索接口；未配置提供商时不可用。"),
                ),
        )
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
                    .child(
                        tinted(
                            Button::new(ElementId::Name(format!("mcp-toggle:{name}").into()))
                                .outline()
                                .border_2()
                                .xsmall(),
                            if server.enabled {
                                Tone::Success
                            } else {
                                Tone::Danger
                            },
                            cx,
                        )
                        .label(if server.enabled {
                            "已启用"
                        } else {
                            "已停用"
                        })
                        .selected(server.enabled)
                        .toggled(server.enabled)
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle_server(index, cx))),
                    )
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
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(Input::new(&self.mcp_name).cleanable(true))
                    .child(Input::new(&self.mcp_target).cleanable(true).flex_1())
                    .child(
                        Button::new("mcp-add")
                            .outline()
                            .border_2()
                            .xsmall()
                            .label("添加")
                            .on_click(cx.listener(|this, _, _, cx| this.add_server(cx))),
                    ),
            )
    }

    /// Skill：根目录、扫描结果与新建。
    fn render_skills(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let enabled = self.draft.skills.enabled.clone();
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
                let toggle_name = name.clone();
                let open_name = name.clone();
                let is_enabled = enabled.is_empty() || enabled.iter().any(|entry| entry == &name);
                let description = skill.description.clone();
                let root = skill.root.clone();
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
                    .child(
                        tinted(
                            Button::new(ElementId::Name(format!("skill-toggle:{name}").into()))
                                .outline()
                                .border_2()
                                .xsmall(),
                            if is_enabled {
                                Tone::Success
                            } else {
                                Tone::Danger
                            },
                            cx,
                        )
                        .label(if is_enabled {
                            "已启用"
                        } else {
                            "已停用"
                        })
                        .selected(is_enabled)
                        .toggled(is_enabled)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_skill(&toggle_name, cx);
                        })),
                    )
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
                    .child(Input::new(&self.skill_dir).cleanable(true).flex_1())
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
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(Input::new(&self.skill_name).cleanable(true))
                    .child(Input::new(&self.skill_description).cleanable(true).flex_1())
                    .child(
                        Button::new("skill-create")
                            .outline()
                            .border_2()
                            .xsmall()
                            .icon(IconName::Plus)
                            .label("新建")
                            .on_click(cx.listener(|this, _, _, cx| this.create_skill(cx))),
                    ),
            )
    }
}

impl Render for SettingsForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let tab = self.tab;

        v_flex()
            .id("settings-panel")
            .test_support()
            .gap_4()
            .max_h(rems(26.))
            .overflow_y_scroll()
            .child(self.render_tabs(cx))
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("这里只放跨会话的设置；审核模式、目录授权与工具开关在主界面直接切换。"),
            )
            .when(tab == SettingsTab::General, |this| this.child(self.render_general(cx)))
            .when(tab == SettingsTab::Tools, |this| this.child(self.render_tools(cx)))
            .when(tab == SettingsTab::Mcp, |this| this.child(self.render_mcp(cx)))
            .when(tab == SettingsTab::Skill, |this| this.child(self.render_skills(cx)))
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
