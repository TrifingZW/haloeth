//! 偏好设置窗口：独立、可缩放的全局设置。
//!
//! 采用左侧导航 + 右侧页面的 IDE 布局；所有修改即时生效（文本输入随输入保存），
//! 并提供配置 JSON 的导出与导入。会话级的审核模式、目录授权与功能开关仍在主界面。

use gpui_kit::assets::IconName;
use gpui_kit::base::{TestSupportExt as _, h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _, TitleBar};
use gpui_kit::prelude::{FluentBuilder as _, InteractiveElement as _};
use gpui_kit::*;

use crate::Haloeth;
use crate::config::Config;
use crate::models::ModelsForm;
use crate::session::Session;
use crate::settings::{SettingsForm, SettingsTab};
use crate::ui::{Tone, tinted};

/// 偏好窗口的分区。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefSection {
    /// 外观、按键、审核、存储。
    General,
    /// 提供商与模型。
    Models,
    /// 内置工具与生成工具。
    Tools,
    /// MCP 服务器。
    Mcp,
    /// Skill。
    Skill,
    /// 无障碍：语音识别与播报。
    Audio,
}

impl PrefSection {
    /// 全部分区，按导航顺序。
    pub const ALL: [Self; 6] =
        [Self::General, Self::Models, Self::Tools, Self::Mcp, Self::Skill, Self::Audio];

    /// 导航图标。
    pub fn icon(self) -> gpui_kit::assets::IconName {
        use gpui_kit::assets::IconName;
        match self {
            Self::General => IconName::Settings,
            Self::Models => IconName::Cpu,
            Self::Tools => IconName::SquareTerminal,
            Self::Mcp => IconName::Network,
            Self::Skill => IconName::Folder,
            Self::Audio => IconName::Mic,
        }
    }

    /// 导航标题。
    pub fn label(self) -> &'static str {
        match self {
            Self::General => "通用",
            Self::Models => "模型",
            Self::Tools => "工具",
            Self::Mcp => "MCP",
            Self::Skill => "Skill",
            Self::Audio => "无障碍",
        }
    }

    /// 页面说明。
    pub fn description(self) -> &'static str {
        match self {
            Self::General => "外观、按键、审核与会话存储",
            Self::Models => "各个提供商的协议、请求头、模型与输入 / 输出模态",
            Self::Tools => "AI 可以使用的内置工具与生成工具",
            Self::Mcp => "接入外部 MCP 服务器，为 AI 提供额外工具",
            Self::Skill => "Skill 根目录与技能包；AI 按需加载",
            Self::Audio => "语音识别（ASR）与语音播报（TTS）",
        }
    }

    /// 稳定标识。
    pub fn id(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Models => "models",
            Self::Tools => "tools",
            Self::Mcp => "mcp",
            Self::Skill => "skill",
            Self::Audio => "audio",
        }
    }

    /// 对应的设置分页；模型页单独渲染。
    fn tab(self) -> Option<SettingsTab> {
        match self {
            Self::General => Some(SettingsTab::General),
            Self::Models => None,
            Self::Tools => Some(SettingsTab::Tools),
            Self::Mcp => Some(SettingsTab::Mcp),
            Self::Skill => Some(SettingsTab::Skill),
            Self::Audio => Some(SettingsTab::Audio),
        }
    }
}

/// 偏好窗口视图。
pub struct PreferencesWindow {
    session: WeakEntity<Session>,
    settings: Entity<SettingsForm>,
    models: Entity<ModelsForm>,
    section: PrefSection,
    /// 左侧导航是否收起为图标。
    nav_collapsed: bool,
    error: Option<SharedString>,
    _settings_observation: Subscription,
    _models_observation: Subscription,
}

impl PreferencesWindow {
    /// 构造偏好窗口。
    pub fn new(
        window: &mut Window,
        session: WeakEntity<Session>,
        haloeth: WeakEntity<Haloeth>,
        section: PrefSection,
        cx: &mut Context<Self>,
    ) -> Self {
        let config =
            session.upgrade().map(|session| session.read(cx).config().clone()).unwrap_or_default();
        let settings = cx.new(|cx| {
            SettingsForm::new(
                window,
                &config,
                session.clone(),
                haloeth,
                section.tab().unwrap_or(SettingsTab::General),
                true,
                cx,
            )
        });
        let models = cx.new(|cx| ModelsForm::new(window, &config, session.clone(), true, cx));

        // 表单内部改动后即时保存：观察通知即可，无需每个控件都手动保存。
        let settings_observation = cx.observe(&settings, |this, _, cx| {
            let settings = this.settings.clone();
            settings.update(cx, |settings, cx| {
                settings.save_if_due(cx);
            });
            cx.notify();
        });
        let models_observation = cx.observe(&models, |this, _, cx| {
            let models = this.models.clone();
            models.update(cx, |models, cx| {
                models.save_if_due(cx);
            });
            cx.notify();
        });

        Self {
            session,
            settings,
            models,
            section,
            nav_collapsed: false,
            error: None,
            _settings_observation: settings_observation,
            _models_observation: models_observation,
        }
    }

    /// 只切换分页，不重新读取配置；用于同一窗口内的重入调用。
    pub fn set_section_quick(&mut self, section: PrefSection, cx: &mut Context<Self>) {
        self.section = section;
        if let Some(tab) = section.tab() {
            self.settings.update(cx, |settings, cx| settings.set_tab(tab, cx));
        }
        cx.notify();
    }

    /// 切换到指定分区；先从会话同步配置，避免模型页与工具页各持旧草稿。
    pub fn set_section(
        &mut self,
        section: PrefSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let config = self.current_config(cx).unwrap_or_default();
        self.settings.update(cx, |settings, cx| settings.reload(&config, window, cx));
        self.models.update(cx, |models, cx| models.reload_from(&config, window, cx));
        self.section = section;
        if let Some(tab) = section.tab() {
            self.settings.update(cx, |settings, cx| settings.set_tab(tab, cx));
        }
        cx.notify();
    }

    /// 导出配置为 JSON。
    fn export_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config) = self.current_config(cx) else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let picked = rfd::AsyncFileDialog::new()
                .set_title("导出偏好")
                .set_file_name("haloeth-config.json")
                .save_file()
                .await;
            let Some(path) = picked else {
                return;
            };
            let result =
                serde_json::to_string_pretty(&config).map_err(|error| error.to_string()).and_then(
                    |text| std::fs::write(path.path(), text).map_err(|error| error.to_string()),
                );
            this.update(cx, |this, cx| {
                this.error = result.err().map(|error| format!("导出失败：{error}").into());
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 从 JSON 导入配置。
    fn import_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.session.clone();
        cx.spawn_in(window, async move |this, cx| {
            let picked = rfd::AsyncFileDialog::new()
                .set_title("导入偏好")
                .add_filter("JSON", &["json"])
                .pick_file()
                .await;
            let Some(path) = picked else {
                return;
            };
            let imported =
                std::fs::read_to_string(path.path()).map_err(|error| error.to_string()).and_then(
                    |text| serde_json::from_str::<Config>(&text).map_err(|error| error.to_string()),
                );
            match imported {
                Ok(mut config) => {
                    config.normalise();
                    let _ = config.save();
                    session.update(cx, |session, cx| session.apply_config(config, cx)).ok();
                    // 表单持有自己的草稿；不重新载入的话，下一次即时保存会把导入结果覆盖掉。
                    let section =
                        this.update(cx, |this, _| this.section).unwrap_or(PrefSection::General);
                    let _ = cx.update(|window, cx| {
                        this.update(cx, |this, cx| {
                            this.error = None;
                            this.set_section(section, window, cx);
                        })
                        .ok();
                    });
                }
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.error = Some(format!("导入失败：{error}").into());
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    /// 当前会话的配置。
    fn current_config(&self, cx: &App) -> Option<Config> {
        self.session.upgrade().map(|session| session.read(cx).config().clone())
    }

    /// 左侧导航；可收起为图标列。
    fn render_nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.section;
        let collapsed = self.nav_collapsed;
        let toggle = Button::new("pref-nav-toggle")
            .ghost()
            .xsmall()
            .w_full()
            .icon(if collapsed {
                IconName::PanelLeftOpen
            } else {
                IconName::PanelLeftClose
            })
            .tooltip(if collapsed {
                "展开导航"
            } else {
                "收起导航"
            })
            .on_click(cx.listener(|this, _, _, cx| {
                this.nav_collapsed = !this.nav_collapsed;
                cx.notify();
            }));

        v_flex()
            .id("preferences-nav")
            .test_support()
            .h_full()
            .w(if collapsed {
                rems(3.25)
            } else {
                rems(10.)
            })
            .flex_shrink_0()
            .border_r_2()
            .border_color(cx.theme().border)
            .p_2()
            .gap_1()
            .child(toggle)
            .children(PrefSection::ALL.into_iter().map(|section| {
                let selected = section == active;
                let button =
                    Button::new(ElementId::Name(format!("pref-nav:{}", section.id()).into()))
                        .ghost()
                        .w_full()
                        .icon(section.icon())
                        .label(if collapsed {
                            ""
                        } else {
                            section.label()
                        })
                        .tooltip(section.label())
                        .selected(selected)
                        .toggled(selected);
                if selected {
                    tinted(button, Tone::Accent, cx)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.set_section(section, window, cx)
                        }))
                        .into_any_element()
                } else {
                    button
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.set_section(section, window, cx)
                        }))
                        .into_any_element()
                }
            }))
    }

    /// 右侧页面内容。
    fn render_body(&mut self, _cx: &mut Context<Self>) -> AnyElement {
        match self.section {
            PrefSection::Models => self.models.clone().into_any_element(),
            _ => self.settings.clone().into_any_element(),
        }
    }
}

impl Render for PreferencesWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let section = self.section;
        let body = self.render_body(cx);
        let has_error = self.error.is_some();

        v_flex()
            .id("preferences-window")
            .test_support()
            .size_full()
            .overflow_hidden()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                TitleBar::new().border_b_2().child(
                    h_flex()
                        .items_center()
                        .gap_2()
                        .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("偏好设置")),
                ),
            )
            .child(
                h_flex().flex_1().min_h_0().items_stretch().child(self.render_nav(cx)).child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .overflow_hidden()
                        .child(
                            h_flex()
                                .id("pref-header")
                                .test_support()
                                .items_center()
                                .gap_2()
                                .p_3()
                                .border_b_2()
                                .border_color(cx.theme().border)
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_base()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(section.label()),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(muted)
                                                .child(section.description()),
                                        ),
                                )
                                .child(
                                    Button::new("pref-export")
                                        .outline()
                                        .border_2()
                                        .xsmall()
                                        .label("导出 JSON")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.export_config(window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("pref-import")
                                        .outline()
                                        .border_2()
                                        .xsmall()
                                        .label("导入 JSON")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.import_config(window, cx)
                                        })),
                                ),
                        )
                        .when(has_error, |this| {
                            this.child(
                                div()
                                    .id("pref-error")
                                    .role(Role::Status)
                                    .test_support()
                                    .px_3()
                                    .py_1()
                                    .text_xs()
                                    .text_color(cx.theme().danger)
                                    .child(self.error.clone().unwrap_or_default()),
                            )
                        })
                        .child(
                            div()
                                .id("pref-scroll")
                                .test_support()
                                .w_full()
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .p_3()
                                .child(body),
                        ),
                ),
            )
    }
}
