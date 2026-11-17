//! 生成工具的高级设置：接口地址、请求头、请求体模板与轮询字段。

use std::collections::BTreeMap;

use gpui_kit::base::{TestSupportExt as _, h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Editor, EditorState, InputState};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::config::{MediaTool, MediaToolConfig};
use crate::settings::SettingsForm;
use crate::ui::{cancel_button, field, input_frame};

/// 生成工具高级设置面板。
pub struct MediaForm {
    settings: WeakEntity<SettingsForm>,
    kind: MediaTool,
    endpoint: Entity<InputState>,
    poll_url: Entity<InputState>,
    id_path: Entity<InputState>,
    status_path: Entity<InputState>,
    done_value: Entity<InputState>,
    result_path: Entity<InputState>,
    interval: Entity<InputState>,
    timeout: Entity<InputState>,
    headers: Entity<EditorState>,
    body: Entity<EditorState>,
    error: Option<SharedString>,
}

impl MediaForm {
    /// 构造面板。
    pub fn new(
        window: &mut Window,
        settings: WeakEntity<SettingsForm>,
        kind: MediaTool,
        current: MediaToolConfig,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut input = |value: &str, placeholder: &'static str| {
            let state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
            state.update(cx, |state, cx| state.set_value(value.to_owned(), window, cx));
            state
        };
        let endpoint = input(&current.endpoint, "提交地址，例如 https://example.com/tasks");
        let poll_url = input(&current.poll_url, "轮询地址，支持 {{base}} 与 {{id}}");
        let id_path = input(&current.id_path, "任务 ID 字段路径，例如 output.task_id");
        let status_path = input(&current.status_path, "状态字段路径，例如 output.task_status");
        let done_value = input(&current.done_value, "完成状态值，例如 SUCCEEDED");
        let result_path = input(&current.result_path, "结果地址路径，例如 output.results.0.url");
        let interval = input(&current.interval_ms.to_string(), "轮询间隔（毫秒）");
        let timeout = input(&current.timeout_s.to_string(), "超时（秒）");
        let headers_text = current
            .headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}"))
            .collect::<Vec<_>>()
            .join("\n");
        let body_text = current
            .body_template
            .as_ref()
            .map(|body| serde_json::to_string_pretty(body).unwrap_or_default())
            .unwrap_or_default();
        let headers =
            cx.new(|cx| EditorState::new(window, cx).placeholder("Name: value，每行一个"));
        headers.update(cx, |state, cx| state.set_value(headers_text, window, cx));
        let body = cx.new(|cx| {
            EditorState::new(window, cx)
                .placeholder("请求体 JSON，字符串里的 {{prompt}} 会替换为用户提示")
        });
        body.update(cx, |state, cx| state.set_value(body_text, window, cx));

        Self {
            settings,
            kind,
            endpoint,
            poll_url,
            id_path,
            status_path,
            done_value,
            result_path,
            interval,
            timeout,
            headers,
            body,
            error: None,
        }
    }

    /// 保存到设置草稿。
    pub fn save(&mut self, cx: &mut Context<Self>) -> bool {
        let timeout: u64 = self.timeout.read(cx).value().trim().parse().unwrap_or(300);
        let interval: u64 = self.interval.read(cx).value().trim().parse().unwrap_or(2000);
        let body = self.body.read(cx).value().trim().to_owned();
        let body_template = if body.is_empty() {
            None
        } else {
            match serde_json::from_str::<serde_json::Value>(&body) {
                Ok(value @ serde_json::Value::Object(_)) => Some(value),
                Ok(_) => {
                    self.error = Some("请求体模板必须是 JSON 对象".into());
                    cx.notify();
                    return false;
                }
                Err(error) => {
                    self.error = Some(format!("请求体模板不是合法 JSON：{error}").into());
                    cx.notify();
                    return false;
                }
            }
        };
        let config = MediaToolConfig {
            provider: String::new(),
            model: String::new(),
            endpoint: self.endpoint.read(cx).value().trim().to_owned(),
            headers: parse_headers(self.headers.read(cx).value().as_ref()),
            body_template,
            poll_url: self.poll_url.read(cx).value().trim().to_owned(),
            id_path: self.id_path.read(cx).value().trim().to_owned(),
            status_path: self.status_path.read(cx).value().trim().to_owned(),
            done_value: self.done_value.read(cx).value().trim().to_owned(),
            result_path: self.result_path.read(cx).value().trim().to_owned(),
            interval_ms: interval.max(200),
            timeout_s: timeout.max(5),
        };
        let kind = self.kind;
        if self
            .settings
            .update(cx, |settings, cx| settings.set_media_advanced(kind, config, cx))
            .is_err()
        {
            self.error = Some("设置面板已关闭".into());
            cx.notify();
            return false;
        }
        true
    }

    /// 底部按钮。
    pub fn render_footer(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let form = cx.entity();
        DialogFooter::new()
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .id("media-form-error")
                        .role(Role::Status)
                        .test_support()
                        .flex_1()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .child(
                cancel_button("media-form-cancel", "取消", cx)
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
            .child(Button::new("media-form-save").primary().label("保存").on_click({
                let form = form.clone();
                move |_, window, cx| {
                    if form.update(cx, |form, cx| form.save(cx)) {
                        window.close_dialog(cx);
                    }
                }
            }))
    }

    fn editor(state: &Entity<EditorState>, height: Rems, cx: &App) -> AnyElement {
        div()
            .w_full()
            .border_2()
            .border_color(cx.theme().input)
            .rounded(cx.theme().radius)
            .child(Editor::new(state).h(height).appearance(false).bordered(false))
            .into_any_element()
    }
}

impl Render for MediaForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        v_flex()
            .id("media-form-panel")
            .test_support()
            .gap_3()
            .max_h(rems(30.))
            .overflow_y_scroll()
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(self.kind.label()))
            .child(field("提交地址", input_frame(&self.endpoint, "media-endpoint", true, cx), cx))
            .child(field("请求头", Self::editor(&self.headers, rems(3.), cx), cx))
            .child(field("请求体模板", Self::editor(&self.body, rems(6.), cx), cx))
            .child(field("轮询地址", input_frame(&self.poll_url, "media-poll-url", true, cx), cx))
            .child(field("任务 ID 路径", input_frame(&self.id_path, "media-id-path", true, cx), cx))
            .child(field(
                "状态路径",
                input_frame(&self.status_path, "media-status-path", true, cx),
                cx,
            ))
            .child(field(
                "完成状态值",
                input_frame(&self.done_value, "media-done-value", true, cx),
                cx,
            ))
            .child(field(
                "结果路径",
                input_frame(&self.result_path, "media-result-path", true, cx),
                cx,
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(input_frame(&self.interval, "media-interval", true, cx).flex_1())
                    .child(input_frame(&self.timeout, "media-timeout", true, cx).flex_1()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("百炼地址会自动使用原生异步任务；其他服务按上面的字段轮询。"),
            )
    }
}

/// 解析 `Name: value` 行。
fn parse_headers(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .filter(|(name, _)| !name.is_empty())
        .collect()
}
