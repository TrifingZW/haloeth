//! 新建 **Skill**：独立对话框。
//!
//! 名称、描述、触发条件与目标根目录各占一行；保存后在所选根目录下创建
//! `SKILL.md` 骨架，并写入设置面板的草稿配置。

use std::path::PathBuf;

use agent::skill;
use gpui_kit::base::{TestSupportExt as _, h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::InputState;
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _, WindowExt as _};
use gpui_kit::prelude::{FluentBuilder as _, InteractiveElement as _};
use gpui_kit::*;

use crate::settings::SettingsForm;
use crate::ui::{Tone, cancel_button, field, input_frame, tinted};

/// 新建 Skill 面板。
pub struct SkillForm {
    settings: WeakEntity<SettingsForm>,
    roots: Vec<PathBuf>,
    root: usize,
    name: Entity<InputState>,
    description: Entity<InputState>,
    when_to_use: Entity<InputState>,
    error: Option<SharedString>,
}

impl SkillForm {
    /// 构造面板；根目录来自设置草稿。
    pub fn new(
        window: &mut Window,
        settings: WeakEntity<SettingsForm>,
        roots: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Skill 名称，例如 pdf"));
        let description = cx.new(|cx| InputState::new(window, cx).placeholder("一句话描述"));
        let when_to_use =
            cx.new(|cx| InputState::new(window, cx).placeholder("何时使用，例如用户提到 PDF"));
        Self {
            settings,
            roots,
            root: 0,
            name,
            description,
            when_to_use,
            error: None,
        }
    }

    /// 选择目标根目录。
    fn set_root(&mut self, index: usize, cx: &mut Context<Self>) {
        self.root = index;
        self.error = None;
        cx.notify();
    }

    /// 创建 Skill；成功时由调用方关闭对话框。
    pub fn save(&mut self, cx: &mut Context<Self>) -> bool {
        let name = self.name.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.error = Some("请填写 Skill 名称".into());
            cx.notify();
            return false;
        }
        let Some(root) = self.roots.get(self.root).cloned() else {
            self.error = Some("请选择 Skill 根目录".into());
            cx.notify();
            return false;
        };
        let description = self.description.read(cx).value().trim().to_owned();
        let when_to_use = self.when_to_use.read(cx).value().trim().to_owned();

        match skill::create_skill(&root, &name, &description, &when_to_use) {
            Ok(created) => {
                tracing::info!("已创建 Skill：{}", created.display());
                let _ =
                    self.settings.update(cx, |settings, cx| settings.add_created_skill(&name, cx));
                true
            }
            Err(error) => {
                self.error = Some(format!("创建 Skill 失败：{error}").into());
                cx.notify();
                false
            }
        }
    }

    /// 面板底部：错误提示、取消与创建。
    pub fn render_footer(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let form = cx.entity();

        DialogFooter::new()
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .id("skill-form-error")
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
                cancel_button("skill-form-cancel", "取消", cx)
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
            .child(Button::new("skill-form-save").primary().label("创建").on_click({
                let form = form.clone();
                move |_, window, cx| {
                    if form.update(cx, |form, cx| form.save(cx)) {
                        window.close_dialog(cx);
                    }
                }
            }))
    }

    /// 根目录选择：每个候选一个按钮，悬停显示完整路径。
    fn render_roots(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = h_flex().gap_2().flex_wrap();
        for (index, root) in self.roots.iter().enumerate() {
            let selected = index == self.root;
            let label = root
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| root.display().to_string());
            let button = Button::new(ElementId::Name(format!("skill-form-root:{index}").into()))
                .outline()
                .border_2()
                .xsmall()
                .label(label)
                .tooltip(root.display().to_string())
                .selected(selected)
                .toggled(selected);
            row = row.child(if selected {
                tinted(button, Tone::Accent, cx)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_root(index, cx)))
                    .into_any_element()
            } else {
                button
                    .on_click(cx.listener(move |this, _, _, cx| this.set_root(index, cx)))
                    .into_any_element()
            });
        }
        row
    }
}

impl Render for SkillForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let roots_row = self.render_roots(cx).into_any_element();

        v_flex()
            .id("skill-form-panel")
            .test_support()
            .gap_3()
            .max_h(rems(26.))
            .overflow_y_scroll()
            .child(field("根目录", roots_row, cx))
            .child(field("名称", input_frame(&self.name, "skill-form-name", true, cx), cx))
            .child(field(
                "描述",
                input_frame(&self.description, "skill-form-description", true, cx),
                cx,
            ))
            .child(field(
                "触发条件",
                input_frame(&self.when_to_use, "skill-form-when", true, cx),
                cx,
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(format!(
                        "将在所选目录下创建包含 {} 的文件夹；正文只是指令，附带脚本仍通过 Shell 工具执行并走审核。",
                        skill::SKILL_FILE
                    )),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .id("skill-form-error-inline")
                        .role(Role::Status)
                        .test_support()
                        .aria_label(error.clone())
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
    }
}
