//! 模型面板：提供商与模型的增删查改、模型目录拉取与当前模型选择。
//!
//! 面板作为对话框内容渲染；编辑先落在草稿配置上，保存时写入配置文件并应用到会话。
//! 同一时刻只展开一个提供商，字段与新增输入框因此可以复用同一组输入状态。

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::base::{TestSupportExt as _, h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::collapsible::Collapsible;
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, Selectable as _, Sizable as _, WindowExt as _};
use gpui_kit::prelude::{FluentBuilder as _, InteractiveElement as _};
use gpui_kit::*;

use crate::config::{Config, DEFAULT_BASE_URL, Provider};
use crate::session::Session;

/// 删除提供商的确认动作：对话框按钮与回车确认共用同一实现。
type ProviderRemoval = Rc<dyn Fn(&mut Window, &mut App)>;

/// 模型目录拉取状态，按提供商区分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelsStatus {
    /// 尚未拉取
    Idle,
    /// 正在拉取指定提供商
    Fetching(String),
    /// 拉取成功：提供商与模型数量
    Fetched {
        provider: String,
        count: usize,
    },
    /// 拉取失败：提供商与原因
    Failed {
        provider: String,
        reason: String,
    },
}

impl ModelsStatus {
    /// 指定提供商的拉取结果文本；与本次拉取无关时返回 [`None`]。
    fn line(&self, provider: &str) -> Option<String> {
        match self {
            Self::Fetched {
                provider: name,
                count,
            } if name == provider => Some(format!("已拉取 {count} 个模型")),
            Self::Failed {
                provider: name,
                reason,
            } if name == provider => Some(format!("拉取失败：{reason}")),
            _ => None,
        }
    }

    /// 指定提供商是否正在拉取。
    fn is_fetching(&self, provider: &str) -> bool {
        matches!(self, Self::Fetching(name) if name == provider)
    }

    /// 指定提供商的最近一次拉取是否失败。
    fn is_failed(&self, provider: &str) -> bool {
        matches!(self, Self::Failed { provider: name, .. } if name == provider)
    }
}

/// 模型面板：提供商与模型的编辑状态。
pub struct ModelsForm {
    /// 草稿配置
    draft: Config,
    /// 会话：保存与拉取经由它执行
    session: WeakEntity<Session>,
    /// 展开的提供商名称，同一时刻只有一个
    expanded: Option<String>,
    /// 展开项的字段输入框
    name: Entity<InputState>,
    base_url: Entity<InputState>,
    api_key: Entity<InputState>,
    /// 待添加的模型标识
    new_model: Entity<InputState>,
    /// 拉取状态
    status: ModelsStatus,
    /// 面板级提示：保存校验与写盘失败
    error: Option<SharedString>,
}

impl ModelsForm {
    /// 以会话配置构造面板，草稿为配置的副本，并展开当前模型所属的提供商。
    pub fn new(
        window: &mut Window,
        config: &Config,
        session: WeakEntity<Session>,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("提供商名称"));
        let base_url = cx.new(|cx| InputState::new(window, cx).placeholder(DEFAULT_BASE_URL));
        let api_key = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("sk-…"));
        let new_model = cx.new(|cx| InputState::new(window, cx).placeholder("模型标识"));

        let expanded = config
            .selection()
            .map(|(provider, _)| provider.to_owned())
            .or_else(|| config.providers.first().map(|provider| provider.name.clone()));
        let mut form = Self {
            draft: config.clone(),
            session,
            expanded,
            name,
            base_url,
            api_key,
            new_model,
            status: ModelsStatus::Idle,
            error: None,
        };
        form.draft.normalise();
        form.reload_fields(window, cx);
        form
    }

    /// 指定提供商是否正在展开。
    fn is_expanded(&self, provider: &str) -> bool {
        self.expanded.as_deref() == Some(provider)
    }

    /// 展开或收起提供商；切换前先把输入框内容写回草稿。
    pub fn toggle(&mut self, provider: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.commit_fields(cx) {
            cx.notify();
            return;
        }
        self.expanded = (!self.is_expanded(provider)).then(|| provider.to_owned());
        self.reload_fields(window, cx);
    }

    /// 追加一个提供商并展开它。
    pub fn add_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.commit_fields(cx) {
            cx.notify();
            return;
        }
        let name = self.unique_name();
        if let Err(reason) =
            self.draft.add_provider(Provider::new(name.clone(), String::new(), String::new()))
        {
            self.error = Some(reason.into());
            cx.notify();
            return;
        }
        self.expanded = Some(name);
        self.reload_fields(window, cx);
    }

    /// 生成尚未占用的提供商名称。
    fn unique_name(&self) -> String {
        let base = "新提供商";
        if self.draft.provider_named(base).is_none() {
            return base.to_owned();
        }
        (2..)
            .map(|index| format!("{base} {index}"))
            .find(|name| self.draft.provider_named(name).is_none())
            .unwrap_or_else(|| base.to_owned())
    }

    /// 请求删除提供商：删除会丢弃密钥与模型目录，先由用户确认。
    pub fn confirm_remove_provider(
        &mut self,
        provider: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = provider.to_owned();
        let form = cx.entity().downgrade();
        let remove: ProviderRemoval = {
            let form = form.clone();
            let name = name.clone();
            Rc::new(move |window, cx| {
                form.update(cx, |form, cx| form.remove_provider(&name, window, cx)).ok();
            })
        };

        window.open_alert_dialog(cx, move |alert, _, _| {
            let button_remove = remove.clone();
            let enter_remove = remove.clone();
            alert
                .border_2()
                .title(format!("删除提供商「{name}」？"))
                .description("它的接口地址、密钥与模型目录将一并从配置中移除。")
                // 自绘底部动作：默认按钮带 1px 边框，与全局的 2px 不一致
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("cancel")
                                .outline()
                                .border_2()
                                .label("取消")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("ok").danger().label("删除").on_click(
                            move |_, window, cx| {
                                button_remove(window, cx);
                                window.close_dialog(cx);
                            },
                        )),
                )
                .on_ok(move |_, window, cx| {
                    enter_remove(window, cx);
                    true
                })
        });
    }

    /// 从草稿移除提供商，并展开剩余的第一个。
    fn remove_provider(&mut self, provider: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.draft.remove_provider(provider);
        if self.is_expanded(provider) {
            self.expanded = self.draft.providers.first().map(|entry| entry.name.clone());
        }
        self.error = None;
        self.reload_fields(window, cx);
    }

    /// 选择模型：只修改草稿，保存后生效。
    pub fn select_model(&mut self, provider: &str, model: &str, cx: &mut Context<Self>) {
        if !self.commit_fields(cx) {
            cx.notify();
            return;
        }
        if let Err(reason) = self.draft.select_model(provider, model) {
            self.error = Some(reason.into());
        }
        cx.notify();
    }

    /// 以输入框中的标识为指定提供商添加模型。
    pub fn add_model(&mut self, provider: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.commit_fields(cx) {
            cx.notify();
            return;
        }
        let model = self.new_model.read(cx).value().trim().to_owned();
        if model.is_empty() {
            self.error = Some("请填写模型标识".into());
            cx.notify();
            return;
        }
        self.draft.add_model(provider, &model);
        self.new_model.update(cx, |state, cx| state.set_value("", window, cx));
        cx.notify();
    }

    /// 从指定提供商删除模型。
    pub fn remove_model(&mut self, provider: &str, model: &str, cx: &mut Context<Self>) {
        if !self.commit_fields(cx) {
            cx.notify();
            return;
        }
        self.draft.remove_model(provider, model);
        cx.notify();
    }

    /// 拉取指定提供商的模型目录，结果替换草稿中的目录。
    pub fn fetch(&mut self, provider: &str, cx: &mut Context<Self>) {
        if self.status.is_fetching(provider) || !self.commit_fields(cx) {
            cx.notify();
            return;
        }
        let Some(target) = self.draft.provider_named(provider).cloned() else {
            return;
        };
        if !target.is_configured() {
            self.status = ModelsStatus::Failed {
                provider: provider.to_owned(),
                reason: "请先填写 API 密钥".to_owned(),
            };
            cx.notify();
            return;
        }

        let name = target.name.clone();
        let Some(session) = self.session.upgrade() else {
            self.status = ModelsStatus::Failed {
                provider: name,
                reason: "会话已关闭".to_owned(),
            };
            cx.notify();
            return;
        };
        let fetch = session.update(cx, |session, cx| session.fetch_models(target, cx));
        self.status = ModelsStatus::Fetching(name.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = fetch.await;
            this.update(cx, |form, cx| form.finish_fetch(&name, result, cx)).ok();
        })
        .detach();
    }

    /// 拉取结束：成功时替换该提供商的模型目录。
    fn finish_fetch(
        &mut self,
        provider: &str,
        result: Result<Vec<String>, String>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(models) => {
                let count = models.len();
                self.draft.replace_models(provider, models);
                self.status = ModelsStatus::Fetched {
                    provider: provider.to_owned(),
                    count,
                };
            }
            Err(reason) => {
                self.status = ModelsStatus::Failed {
                    provider: provider.to_owned(),
                    reason,
                };
            }
        }
        cx.notify();
    }

    /// 保存草稿：写入配置文件并应用到会话；返回是否成功。
    pub fn save(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(config) = self.commit(cx) else {
            return false;
        };
        if let Err(error) = config.save() {
            self.error = Some(format!("保存失败：{error}").into());
            cx.notify();
            return false;
        }
        self.session.update(cx, |session, cx| session.apply_config(config, cx)).ok();
        true
    }

    /// 提交草稿：写回字段并校验当前模型的提供商已配置密钥；失败时记录提示。
    fn commit(&mut self, cx: &mut Context<Self>) -> Option<Config> {
        if !self.commit_fields(cx) {
            cx.notify();
            return None;
        }
        if let Some(provider) = self.draft.provider()
            && !provider.is_configured()
        {
            let name = provider.name.clone();
            self.error = Some(format!("请为提供商「{name}」填写 API 密钥").into());
            cx.notify();
            return None;
        }
        Some(self.draft.clone())
    }

    /// 面板底部：保存状态与取消、保存动作。
    pub fn render_footer(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let form = cx.entity();

        DialogFooter::new()
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .id("models-error")
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
                Button::new("models-cancel")
                    .outline()
                    .border_2()
                    .label("取消")
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
            .child(Button::new("models-save").primary().label("保存").on_click({
                let form = form.clone();
                move |_, window, cx| {
                    if form.update(cx, |form, cx| form.save(cx)) {
                        window.close_dialog(cx);
                    }
                }
            }))
    }

    /// 把输入框内容写回草稿中的展开项；失败时记录提示。
    fn commit_fields(&mut self, cx: &App) -> bool {
        match self.write_fields(cx) {
            Ok(()) => {
                self.error = None;
                true
            }
            Err(reason) => {
                self.error = Some(reason.into());
                false
            }
        }
    }

    /// 把输入框内容写入草稿；重名等冲突返回原因。
    fn write_fields(&mut self, cx: &App) -> Result<(), String> {
        let Some(provider) = self.expanded.clone() else {
            return Ok(());
        };
        let name = self.name.read(cx).value().trim().to_owned();
        if !name.is_empty() && name != provider {
            self.draft.rename_provider(&provider, &name)?;
            self.expanded = Some(name);
        }
        let Some(current) = self.expanded.clone() else {
            return Ok(());
        };
        let base_url = self.base_url.read(cx).value().trim().to_owned();
        let api_key = self.api_key.read(cx).value().trim().to_owned();
        self.draft.update_provider(&current, base_url, api_key);
        Ok(())
    }

    /// 把草稿中的展开项写入输入框，并清空待添加的模型标识。
    fn reload_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (name, base_url, api_key) =
            match self.expanded.as_deref().and_then(|provider| self.draft.provider_named(provider))
            {
                Some(provider) => {
                    (provider.name.clone(), provider.base_url.clone(), provider.api_key.clone())
                }
                None => (String::new(), String::new(), String::new()),
            };
        self.name.update(cx, |state, cx| state.set_value(name, window, cx));
        self.base_url.update(cx, |state, cx| state.set_value(base_url, window, cx));
        self.api_key.update(cx, |state, cx| state.set_value(api_key, window, cx));
        self.new_model.update(cx, |state, cx| state.set_value("", window, cx));
        cx.notify();
    }

    /// 单个提供商：折叠标题、地址摘要与展开后的编辑区。
    fn render_provider(&self, provider: &str, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.draft.provider_named(provider) else {
            return div().into_any_element();
        };
        let key = provider.to_owned();
        let expanded = self.is_expanded(provider);
        let entry_selected =
            self.draft.provider().map(|entry| entry.name.as_str()) == Some(provider);
        let endpoint = entry.endpoint();
        let models = entry.models.clone();
        let muted = cx.theme().muted_foreground;
        let primary = cx.theme().primary;

        let disclosure = if expanded {
            IconName::ChevronDown
        } else {
            IconName::ChevronRight
        };
        let header = Button::new(provider_id("provider-toggle", provider))
            .ghost()
            .w_full()
            .justify_start()
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .child(Icon::new(disclosure).xsmall().text_color(muted))
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(key.clone()))
                    .when(entry_selected, |this| {
                        this.child(
                            h_flex()
                                .items_center()
                                .gap_1()
                                .child(
                                    Icon::new(IconName::CircleCheck).xsmall().text_color(primary),
                                )
                                .child(div().text_xs().text_color(muted).child("当前模型所属")),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        div().text_xs().text_color(muted).child(format!("{} 个模型", models.len())),
                    ),
            )
            .on_click(cx.listener({
                let key = key.clone();
                move |this, _, window, cx| this.toggle(&key, window, cx)
            }))
            .accessibility_label(format!("提供商：{provider}（{} 个模型）", models.len()));

        let body = v_flex()
            .gap_3()
            .pl_6()
            .child(field("名称", input_frame(&self.name, "provider-name", false, cx), cx))
            .child(field(
                "接口地址",
                input_frame(&self.base_url, "provider-endpoint", true, cx),
                cx,
            ))
            .child(field("API 密钥", input_frame(&self.api_key, "provider-key", true, cx), cx))
            .child(self.render_fetch_row(provider, cx))
            .child(self.render_models(provider, &models, entry_selected, cx))
            .child(
                h_flex().child(
                    Button::new(provider_id("provider-remove", provider))
                        .outline()
                        .border_2()
                        .danger()
                        .icon(IconName::Trash)
                        .label("删除提供商")
                        .on_click(cx.listener({
                            let key = key.clone();
                            move |this, _, window, cx| {
                                this.confirm_remove_provider(&key, window, cx);
                            }
                        })),
                ),
            );

        Collapsible::new()
            .open(expanded)
            .gap_2()
            .child(header)
            .when(!expanded, |this| {
                this.child(div().pl_6().text_xs().text_color(muted).truncate().child(endpoint))
            })
            .when(expanded, |this| this.content(body))
            .into_any_element()
    }

    /// 拉取动作与结果提示。
    fn render_fetch_row(&self, provider: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let status = self.status.line(provider);
        let failed = self.status.is_failed(provider);
        let danger = cx.theme().danger;
        let muted = cx.theme().muted_foreground;

        h_flex()
            .items_center()
            .gap_3()
            .child(
                Button::new(provider_id("models-fetch", provider))
                    .outline()
                    .border_2()
                    .icon(IconName::RefreshCw)
                    .label("拉取模型")
                    .loading(self.status.is_fetching(provider))
                    .on_click(cx.listener({
                        let key = provider.to_owned();
                        move |this, _, _, cx| this.fetch(&key, cx)
                    })),
            )
            .when_some(status, |this, line| {
                this.child(
                    div()
                        .id(provider_id("fetch-status", provider))
                        .role(Role::Status)
                        .test_support()
                        .aria_label(line.clone())
                        .text_xs()
                        .text_color(if failed {
                            danger
                        } else {
                            muted
                        })
                        .child(line),
                )
            })
    }

    /// 模型列表：行内选择当前模型，行尾删除。
    fn render_models(
        &self,
        provider: &str,
        models: &[String],
        entry_selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mono = cx.theme().mono_font_family.clone();
        let primary = cx.theme().primary;
        let muted = cx.theme().muted_foreground;
        let current = entry_selected.then(|| self.draft.model()).flatten();

        let rows: Vec<AnyElement> = models
            .iter()
            .map(|model| {
                let selected = current == Some(model.as_str());
                let select = Button::new(model_id("model-select", provider, model))
                    .ghost()
                    .flex_1()
                    .justify_start()
                    .selected(selected)
                    .toggled(selected)
                    .accessibility_label(format!("使用模型「{model}」"))
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .w_full()
                            .child(
                                div().w_4().child(
                                    Icon::new(if selected {
                                        IconName::Check
                                    } else {
                                        IconName::CircleSmall
                                    })
                                    .xsmall()
                                    .text_color(if selected {
                                        primary
                                    } else {
                                        muted
                                    }),
                                ),
                            )
                            .child(
                                div()
                                    .font_family(mono.clone())
                                    .text_sm()
                                    .truncate()
                                    .child(model.clone()),
                            ),
                    )
                    .on_click(cx.listener({
                        let key = provider.to_owned();
                        let id = model.clone();
                        move |this, _, _, cx| this.select_model(&key, &id, cx)
                    }));
                let remove = Button::new(model_id("model-remove", provider, model))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Trash)
                    .tooltip(format!("删除模型「{model}」"))
                    .accessibility_label(format!("删除模型「{model}」"))
                    .on_click(cx.listener({
                        let key = provider.to_owned();
                        let id = model.clone();
                        move |this, _, _, cx| this.remove_model(&key, &id, cx)
                    }));

                h_flex()
                    .items_center()
                    .gap_1()
                    .w_full()
                    .child(select)
                    .child(remove)
                    .into_any_element()
            })
            .collect();

        let empty = rows.is_empty();
        v_flex()
            .gap_1()
            .w_full()
            .when(empty, |this| {
                this.child(
                    div()
                        .id(provider_id("models-empty", provider))
                        .role(Role::Status)
                        .test_support()
                        .aria_label("尚无模型")
                        .text_xs()
                        .text_color(muted)
                        .child("尚无模型。使用「拉取模型」，或在下方手动添加。"),
                )
            })
            .children(rows)
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .pt_1()
                    .child(input_frame(&self.new_model, "model-input", false, cx).flex_1())
                    .child(
                        Button::new(provider_id("model-add", provider))
                            .outline()
                            .border_2()
                            .icon(IconName::Plus)
                            .label("添加")
                            .on_click(cx.listener({
                                let key = provider.to_owned();
                                move |this, _, window, cx| {
                                    this.add_model(&key, window, cx);
                                }
                            })),
                    ),
            )
    }
}

impl Render for ModelsForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let providers: Vec<String> =
            self.draft.providers.iter().map(|provider| provider.name.clone()).collect();
        let empty = providers.is_empty();
        let muted = cx.theme().muted_foreground;

        let mut blocks: Vec<AnyElement> = Vec::new();
        for (index, provider) in providers.iter().enumerate() {
            if index > 0 {
                blocks.push(provider_divider(provider, cx));
            }
            blocks.push(self.render_provider(provider, cx));
        }

        v_flex()
            .id("models-panel")
            .test_support()
            .gap_5()
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("修改在保存后生效；密钥以明文写入本机配置文件。"),
            )
            .when(empty, |this| {
                this.child(
                    div()
                        .id("models-empty")
                        .role(Role::Status)
                        .test_support()
                        .aria_label("尚未添加提供商")
                        .text_sm()
                        .text_color(muted)
                        .child(
                            "尚未添加提供商。添加后填写接口地址与 API 密钥，即可拉取或手动录入模型。",
                        ),
                )
            })
            .children(blocks)
            .child(
                h_flex().child(
                    Button::new("provider-add")
                        .outline()
                        .border_2()
                        .icon(IconName::Plus)
                        .label("添加提供商")
                        .on_click(cx.listener(|this, _, window, cx| this.add_provider(window, cx))),
                ),
            )
    }
}

/// 提供商卡片之间的分隔线。
///
/// 组件自带的 `Separator` 将粗细写死为 1px 且不可配置，因此自绘：
/// 零高度元素的下边框即 2px 线，与全局边框一致。
fn provider_divider(provider: &str, cx: &App) -> AnyElement {
    div()
        .id(provider_id("provider-divider", provider))
        .test_support()
        .w_full()
        .flex_shrink_0()
        .border_b_2()
        .border_color(cx.theme().border)
        .into_any_element()
}

/// 带标签的表单字段：标签在上，控件在下。
fn field(label: &str, control: impl IntoElement, cx: &App) -> impl IntoElement {
    v_flex()
        .gap_1()
        .child(div().text_xs().text_color(cx.theme().muted_foreground).child(label.to_owned()))
        .child(control)
}

/// 输入控件外框：边框统一 2px，聚焦只改变边框颜色。
///
/// 组件在聚焦时会把自身边框收细为 1px 并补一圈光晕，且不可配置，
/// 因此背景与边框改由外框绘制，控件自身不绘制表面。
/// `cleanable` 对应组件的清除按钮。
fn input_frame(
    state: &Entity<InputState>,
    id: &'static str,
    cleanable: bool,
    cx: &App,
) -> impl Styled + IntoElement {
    let focus_handle = state.read(cx).focus_handle(cx);

    div()
        .id(ElementId::Name(format!("input-frame:{id}").into()))
        .test_support()
        .bg(cx.theme().input_background())
        .border_2()
        .border_color(cx.theme().input)
        .rounded(cx.theme().radius)
        .track_focus(&focus_handle)
        .focus(|style| style.border_color(cx.theme().ring))
        // 控件默认高 2rem，外框上下各占 2px，因此控件限高 2rem − 4px，字段总高不变
        .child(Input::new(state).id(id).cleanable(cleanable).appearance(false).max_h(rems(1.75)))
}

/// 面板元素的稳定标识：以提供商名称命名空间，避免同名模型互相覆盖。
fn provider_id(part: &str, provider: &str) -> ElementId {
    ElementId::Name(format!("{part}:{provider}").into())
}

/// 模型行的稳定标识：提供商与模型共同确定。
fn model_id(part: &str, provider: &str, model: &str) -> ElementId {
    ElementId::Name(format!("{part}:{provider}/{model}").into())
}

#[cfg(test)]
mod tests {
    use super::ModelsStatus;

    #[test]
    fn fetch_status_is_reported_per_provider() {
        let status = ModelsStatus::Fetched {
            provider: "a".to_owned(),
            count: 3,
        };

        assert_eq!(status.line("a"), Some("已拉取 3 个模型".to_owned()));
        assert_eq!(status.line("b"), None);
        assert!(!status.is_fetching("a"));
        assert!(!status.is_failed("a"));
    }

    #[test]
    fn failures_and_progress_are_scoped_to_one_provider() {
        let fetching = ModelsStatus::Fetching("a".to_owned());
        assert!(fetching.is_fetching("a"));
        assert!(!fetching.is_fetching("b"));
        assert_eq!(fetching.line("a"), None);

        let failed = ModelsStatus::Failed {
            provider: "b".to_owned(),
            reason: "离线".to_owned(),
        };
        assert!(failed.is_failed("b"));
        assert!(!failed.is_failed("a"));
        assert_eq!(failed.line("b"), Some("拉取失败：离线".to_owned()));
    }
}
