//! 图片查看窗口：读取本地图片，支持 90° 旋转、水平镜像与缩放。

use std::sync::Arc;

use gpui_kit::base::{TestSupportExt as _, h_flex, v_flex};
use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, TitleBar};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// 图片查看器。
pub struct ImageViewer {
    path: String,
    caption: String,
    original: Option<image::RgbaImage>,
    rendered: Option<Arc<RenderImage>>,
    rotation: u8,
    mirrored: bool,
    zoom: f32,
    /// 鼠标拖动产生的相对偏移。
    pan: Point<Pixels>,
    /// 拖动起点与起始偏移。
    drag_start: Option<(Point<Pixels>, Point<Pixels>)>,
    error: Option<SharedString>,
}

impl ImageViewer {
    /// 读取并解码本地图片。
    pub fn new(path: String, caption: String) -> Self {
        let mut viewer = Self {
            path,
            caption,
            original: None,
            rendered: None,
            rotation: 0,
            mirrored: false,
            zoom: 1.,
            pan: point(px(0.), px(0.)),
            drag_start: None,
            error: None,
        };
        match std::fs::read(&viewer.path).map_err(|error| error.to_string()).and_then(|bytes| {
            image::load_from_memory(&bytes)
                .map(|image| image.to_rgba8())
                .map_err(|error| error.to_string())
        }) {
            Ok(image) => {
                viewer.original = Some(image);
                viewer.rebuild();
            }
            Err(error) => {
                viewer.error = Some(format!("无法读取图片：{error}").into());
            }
        }
        viewer
    }

    /// 应用旋转与镜像，重新编码为可渲染图片。
    fn rebuild(&mut self) {
        let Some(original) = &self.original else {
            return;
        };
        let mut image = original.clone();
        for _ in 0..self.rotation {
            image = image::imageops::rotate90(&image);
        }
        if self.mirrored {
            image = image::imageops::flip_horizontal(&image);
        }
        self.rendered =
            Some(Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(image)])));
        self.error = None;
    }

    fn rotate(&mut self, left: bool, cx: &mut Context<Self>) {
        self.rotation = if left {
            (self.rotation + 3) % 4
        } else {
            (self.rotation + 1) % 4
        };
        self.rebuild();
        cx.notify();
    }

    fn zoom(&mut self, delta: f32, cx: &mut Context<Self>) {
        self.zoom = (self.zoom + delta).clamp(0.25, 4.);
        cx.notify();
    }
}

impl Render for ImageViewer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let size = self
            .original
            .as_ref()
            .map(|image| (image.width() as f32, image.height() as f32))
            .unwrap_or((0., 0.));
        let display_width = size.0 * self.zoom;
        let display_height = size.1 * self.zoom;

        v_flex()
            .id("image-viewer")
            .test_support()
            .size_full()
            .overflow_hidden()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(TitleBar::new().border_b_2().child(
                h_flex().items_center().gap_2().min_w_0().child(
                    div().text_sm().font_weight(FontWeight::SEMIBOLD).child(
                        if self.caption.is_empty() {
                            "图片".to_owned()
                        } else {
                            self.caption.clone()
                        },
                    ),
                ),
            ))
            .child(
                h_flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .border_b_2()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("image-rotate-left")
                            .outline()
                            .border_2()
                            .xsmall()
                            .label("左转")
                            .on_click(cx.listener(|this, _, _, cx| this.rotate(true, cx))),
                    )
                    .child(
                        Button::new("image-rotate-right")
                            .outline()
                            .border_2()
                            .xsmall()
                            .label("右转")
                            .on_click(cx.listener(|this, _, _, cx| this.rotate(false, cx))),
                    )
                    .child(
                        Button::new("image-mirror")
                            .outline()
                            .border_2()
                            .xsmall()
                            .label(if self.mirrored {
                                "取消镜像"
                            } else {
                                "镜像"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.mirrored = !this.mirrored;
                                this.rebuild();
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("image-zoom-out")
                            .outline()
                            .border_2()
                            .xsmall()
                            .label("缩小")
                            .on_click(cx.listener(|this, _, _, cx| this.zoom(-0.25, cx))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("{:.0}%", self.zoom * 100.)),
                    )
                    .child(
                        Button::new("image-zoom-in")
                            .outline()
                            .border_2()
                            .xsmall()
                            .label("放大")
                            .on_click(cx.listener(|this, _, _, cx| this.zoom(0.25, cx))),
                    )
                    .child(
                        Button::new("image-reset")
                            .outline()
                            .border_2()
                            .xsmall()
                            .label("重置")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.rotation = 0;
                                this.mirrored = false;
                                this.zoom = 1.;
                                this.pan = point(px(0.), px(0.));
                                this.rebuild();
                                cx.notify();
                            })),
                    ),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .id("image-error")
                        .role(Role::Status)
                        .test_support()
                        .p_3()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .when_some(self.rendered.clone(), |this, image| {
                this.child(
                    div()
                        .id("image-canvas")
                        .test_support()
                        .flex_1()
                        .min_h_0()
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_move()
                        .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| {
                            let delta = match event.delta {
                                ScrollDelta::Lines(delta) => delta.y,
                                ScrollDelta::Pixels(delta) => delta.y / px(40.),
                            };
                            this.zoom(delta * 0.08, cx);
                            cx.stop_propagation();
                        }))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                this.drag_start = Some((event.position, this.pan));
                                cx.notify();
                            }),
                        )
                        .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                            if event.pressed_button != Some(MouseButton::Left) {
                                return;
                            }
                            if let Some((start, pan)) = this.drag_start {
                                this.pan = point(
                                    pan.x + (event.position.x - start.x),
                                    pan.y + (event.position.y - start.y),
                                );
                                cx.notify();
                            }
                        }))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.drag_start = None;
                                cx.notify();
                            }),
                        )
                        .child(
                            div()
                                .w(px(display_width))
                                .h(px(display_height))
                                .flex_shrink_0()
                                .overflow_hidden()
                                .left(self.pan.x)
                                .top(self.pan.y)
                                .child(img(image).w_full().h_full().object_fit(ObjectFit::Contain)),
                        ),
                )
            })
            .when(self.rendered.is_none() && self.error.is_none(), |this| {
                this.child(div().p_3().text_sm().text_color(muted).child("正在读取图片…"))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::ImageViewer;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AppContext as _, TestAppContext, px, size};

    #[gpui_kit::test]
    fn viewer_opens_and_rotates(cx: &mut TestAppContext) {
        let path = std::env::temp_dir().join("haloeth-viewer.png");
        let image = image::RgbaImage::from_pixel(4, 2, image::Rgba([0, 0, 255, 255]));
        image.save(&path).expect("image");
        let viewer =
            cx.update(|cx| cx.new(|_| ImageViewer::new(path.display().to_string(), "t".into())));

        cx.update(|cx| {
            viewer.update(cx, |viewer, cx| {
                assert!(viewer.rendered.is_some());
                viewer.rotate(false, cx);
                assert_eq!(viewer.rotation, 1);
                viewer.zoom(0.5, cx);
                assert!(viewer.zoom > 1.);
                viewer.mirrored = true;
                viewer.rebuild();
                assert!(viewer.error.is_none());
            });
        });
        let _ = std::fs::remove_file(path);
    }

    #[gpui_kit::test]
    fn viewer_renders_in_a_window(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let path = std::env::temp_dir().join("haloeth-viewer-render.png");
        let image = image::RgbaImage::from_pixel(16, 12, image::Rgba([255, 0, 0, 255]));
        image.save(&path).expect("image");
        let handle = cx.open_window(size(px(480.), px(360.)), |window, cx| {
            let viewer = cx.new(|_| ImageViewer::new(path.display().to_string(), "t".into()));
            Root::new(viewer, window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("image-viewer").visible());
            window.click("image-rotate-right", cx);
            window.render_frame(cx);
            window.click("image-zoom-in", cx);
            window.render_frame(cx);
        })
        .unwrap();
        let _ = std::fs::remove_file(path);
    }
}
