//! 新建 **MCP** 服务器：独立对话框。
//!
//! 字段纵向排列，每个字段独占一行，留出参数、环境变量与请求头的空间。
//! 保存时只写入设置面板的草稿配置，仍需在设置面板点「保存」才会持久化。

use std::collections::BTreeMap;

use agent::mcp::{McpServerConfig, McpTransport};
use gpui_kit::base::{TestSupportExt as _, h_flex, v_flex};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Editor, EditorState, InputState};
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _, WindowExt as _};
use gpui_kit::prelude::{FluentBuilder as _, InteractiveElement as _};
use gpui_kit::*;

use crate::settings::SettingsForm;
use crate::ui::{Tone, cancel_button, field, input_frame, tinted};

/// 传输方式选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum TransportKind {
    /// 本地子进程。
    #[default]
    Stdio,
    /// Streamable HTTP。
    Http,
}

/// 新建 MCP 服务器面板。
pub struct McpForm {
    settings: WeakEntity<SettingsForm>,
    transport: TransportKind,
    name: Entity<InputState>,
    command: Entity<InputState>,
    args: Entity<EditorState>,
    env: Entity<EditorState>,
    url: Entity<InputState>,
    headers: Entity<EditorState>,
    error: Option<SharedString>,
}

impl McpForm {
    /// 构造面板。
    pub fn new(
        window: &mut Window,
        settings: WeakEntity<SettingsForm>,
        cx: &mut Context<Self>,
    ) -> Self {
        let name =
            cx.new(|cx| InputState::new(window, cx).placeholder("服务器名称，例如 filesystem"));
        let command = cx.new(|cx| InputState::new(window, cx).placeholder("命令，例如 npx"));
        let args = cx.new(|cx| EditorState::new(window, cx).placeholder("每行一个参数"));
        let env = cx.new(|cx| EditorState::new(window, cx).placeholder("KEY=VALUE，每行一个"));
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("https://example.com/mcp"));
        let headers =
            cx.new(|cx| EditorState::new(window, cx).placeholder("Name: value，每行一个"));
        Self {
            settings,
            transport: TransportKind::Stdio,
            name,
            command,
            args,
            env,
            url,
            headers,
            error: None,
        }
    }

    /// 切换传输方式。
    fn set_transport(&mut self, transport: TransportKind, cx: &mut Context<Self>) {
        self.transport = transport;
        self.error = None;
        cx.notify();
    }

    /// 保存到设置草稿；成功时由调用方关闭对话框。
    pub fn save(&mut self, cx: &mut Context<Self>) -> bool {
        let name = self.name.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.error = Some("请填写服务器名称".into());
            cx.notify();
            return false;
        }

        let transport = match self.transport {
            TransportKind::Stdio => {
                let command = self.command.read(cx).value().trim().to_owned();
                if command.is_empty() {
                    self.error = Some("请填写要执行的命令".into());
                    cx.notify();
                    return false;
                }
                let args = lines(self.args.read(cx).value().as_ref());
                let env = match pairs(self.env.read(cx).value().as_ref(), '=') {
                    Ok(env) => env,
                    Err(line) => {
                        self.error = Some(format!("环境变量格式应为 KEY=VALUE：{line}").into());
                        cx.notify();
                        return false;
                    }
                };
                McpTransport::Stdio {
                    command,
                    args,
                    env,
                }
            }
            TransportKind::Http => {
                let url = self.url.read(cx).value().trim().to_owned();
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    self.error = Some("地址必须以 http:// 或 https:// 开头".into());
                    cx.notify();
                    return false;
                }
                let headers = match pairs(self.headers.read(cx).value().as_ref(), ':') {
                    Ok(headers) => headers,
                    Err(line) => {
                        self.error = Some(format!("请求头格式应为 Name: value：{line}").into());
                        cx.notify();
                        return false;
                    }
                };
                McpTransport::Http {
                    url,
                    headers,
                }
            }
        };

        let server = McpServerConfig {
            name,
            enabled: true,
            transport,
        };
        match self.settings.update(cx, |settings, cx| settings.add_mcp_server(server, cx)) {
            Ok(Ok(())) => true,
            Ok(Err(error)) => {
                self.error = Some(error.into());
                cx.notify();
                false
            }
            Err(_) => {
                self.error = Some("设置面板已关闭".into());
                cx.notify();
                false
            }
        }
    }

    /// 面板底部：错误提示、取消与保存。
    pub fn render_footer(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let form = cx.entity();

        DialogFooter::new()
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .id("mcp-form-error")
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
                cancel_button("mcp-form-cancel", "取消", cx)
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
            .child(Button::new("mcp-form-save").primary().label("添加").on_click({
                let form = form.clone();
                move |_, window, cx| {
                    if form.update(cx, |form, cx| form.save(cx)) {
                        window.close_dialog(cx);
                    }
                }
            }))
    }

    /// 传输方式选择按钮。
    fn render_transport(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = h_flex().gap_2();
        for (kind, id, label) in [
            (TransportKind::Stdio, "mcp-form-transport:stdio", "stdio（本地命令）"),
            (TransportKind::Http, "mcp-form-transport:http", "HTTP（远程地址）"),
        ] {
            let selected = self.transport == kind;
            let button = Button::new(id)
                .outline()
                .border_2()
                .xsmall()
                .label(label)
                .selected(selected)
                .toggled(selected);
            row = row.child(if selected {
                tinted(button, Tone::Accent, cx)
                    .on_click(cx.listener(move |this, _, _, cx| this.set_transport(kind, cx)))
                    .into_any_element()
            } else {
                button
                    .on_click(cx.listener(move |this, _, _, cx| this.set_transport(kind, cx)))
                    .into_any_element()
            });
        }
        row
    }

    /// 多行文本输入框：与单行输入共用 2px 边框与背景。
    fn editor(editor: &Entity<EditorState>, height: Rems, cx: &App) -> AnyElement {
        div()
            .w_full()
            .bg(cx.theme().input_background())
            .border_2()
            .border_color(cx.theme().input)
            .rounded(cx.theme().radius)
            .child(Editor::new(editor).h(height).appearance(false).bordered(false))
            .into_any_element()
    }
}

impl Render for McpForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let transport = self.transport;
        let transport_row = self.render_transport(cx).into_any_element();

        v_flex()
            .id("mcp-form-panel")
            .test_support()
            .gap_3()
            .max_h(rems(26.))
            .overflow_y_scroll()
            .child(field("名称", input_frame(&self.name, "mcp-form-name", true, cx), cx))
            .child(field("传输方式", transport_row, cx))
            .when(transport == TransportKind::Stdio, |this| {
                this.child(field(
                    "命令",
                    input_frame(&self.command, "mcp-form-command", true, cx),
                    cx,
                ))
                .child(field("参数", Self::editor(&self.args, rems(4.), cx), cx))
                .child(field(
                    "环境变量",
                    Self::editor(&self.env, rems(4.), cx),
                    cx,
                ))
            })
            .when(transport == TransportKind::Http, |this| {
                this.child(field("地址", input_frame(&self.url, "mcp-form-url", true, cx), cx))
                    .child(field("请求头", Self::editor(&self.headers, rems(4.), cx), cx))
            })
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("stdio 服务器是以当前用户身份运行的本地程序，添加即表示信任其命令。"),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .id("mcp-form-error-inline")
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

/// 按行拆分，去掉空行与首尾空白。
fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|line| !line.is_empty()).map(str::to_owned).collect()
}

/// 按行解析 `KEY=VALUE` 或 `Name: value`；返回第一个格式错误的行。
fn pairs(text: &str, separator: char) -> Result<BTreeMap<String, String>, String> {
    let mut map = BTreeMap::new();
    for line in lines(text) {
        let Some((key, value)) = line.split_once(separator) else {
            return Err(line);
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(line);
        }
        map.insert(key.to_owned(), value.trim().to_owned());
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::{lines, pairs};

    #[test]
    fn lines_and_pairs_ignore_blank_rows() {
        assert_eq!(lines("-y\n\n  server  \n"), vec!["-y".to_owned(), "server".to_owned()]);

        let env = pairs("A=1\n\nB = two\n", '=').expect("env should parse");
        assert_eq!(env.get("A").map(String::as_str), Some("1"));
        assert_eq!(env.get("B").map(String::as_str), Some("two"));
        assert_eq!(pairs("broken", '='), Err("broken".to_owned()));
        assert_eq!(pairs(" =missing", '='), Err("=missing".to_owned()));
    }

    #[test]
    fn headers_split_on_the_first_colon() {
        let headers = pairs("Authorization: Bearer a:b\n", ':').expect("headers should parse");
        assert_eq!(headers.get("Authorization").map(String::as_str), Some("Bearer a:b"));
    }
}
