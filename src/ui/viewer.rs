//! In-app image/video viewer.
use super::controls::{Tone, tool_button};
use super::*;

impl Root {
    /// Open the in-app viewer for a file: images get a fetched preview
    /// rendition, videos get a preview frame + open-in-player, everything
    /// else gets a details card with download/open-externally actions.
    pub(super) fn open_viewer(&mut self, file_id: &str, cx: &mut Context<Self>) {
        let Some(f) = self.namespace.files.get(file_id).cloned() else {
            return;
        };
        let kind = if crate::tg::is_image_name(&f.name) {
            ViewKind::Image
        } else if crate::tg::is_video_name(&f.name) {
            ViewKind::Video
        } else {
            ViewKind::Other
        };
        self.viewer = Some(Viewer {
            file_id: f.id.clone(),
            name: f.name.clone(),
            kind,
            path: None,
            failed: false,
            zoom: 1.,
        });
        if kind != ViewKind::Other {
            self.send(Command::FetchPreview {
                file_id: f.id.clone(),
            });
        }
        cx.notify();
    }

    /// Fullscreen viewer overlay — closes on backdrop click, Escape or ✕.
    pub(super) fn render_viewer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(v) = &self.viewer else {
            return div().into_any_element();
        };
        let file_id = v.file_id.clone();
        let name = v.name.clone();
        let kind = v.kind;
        let path = v.path.clone();
        let failed = v.failed;

        let mut body = div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden();
        let zoom = v.zoom;
        if let Some(p) = path {
            // Zoom scales the image past the container; the scroll
            // surface doubles as pan.
            body = if kind == ViewKind::Image && zoom != 1. {
                body.child(
                    div()
                        .id("viewer-zoom")
                        .size_full()
                        .overflow_x_scroll()
                        .overflow_y_scroll()
                        .child(
                            img(p)
                                .w(relative(zoom))
                                .object_fit(gpui::ObjectFit::Contain),
                        ),
                )
            } else {
                body.child(
                    img(p)
                        .max_w_full()
                        .max_h_full()
                        .object_fit(gpui::ObjectFit::Contain),
                )
            };
        } else if failed || kind == ViewKind::Other {
            body = body.child(div().text_sm().text_color(theme().dim).child(
                if kind == ViewKind::Other {
                    "No preview for this file type"
                } else {
                    "Preview unavailable"
                },
            ));
        } else {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(theme().dim)
                    .child("Loading preview…"),
            );
        }

        // Controls: keep clicks inside from closing the overlay.
        let stop = |_: &MouseDownEvent, _: &mut Window, cx: &mut App| {
            cx.stop_propagation();
        };
        let mut controls = div()
            .flex()
            .items_center()
            .gap_2()
            .on_mouse_down(MouseButton::Left, stop);
        if kind == ViewKind::Video {
            let obj = file_id.clone();
            controls = controls.child(tool_button(
                "play",
                "Play in player",
                Tone::Ghost,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.send(Command::Stream { obj: obj.clone() });
                    this.status = "opening stream…".into();
                    cx.notify();
                }),
            ));
        }
        if kind == ViewKind::Other {
            let obj = file_id.clone();
            let nm = name.clone();
            controls = controls.child(tool_button(
                "download",
                "Download",
                Tone::Ghost,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.pick_download_dest(obj.clone(), nm.clone(), cx);
                }),
            ));
        }
        // ‹ › walk through the current view's viewable entries.
        let siblings: Vec<String> = self
            .entries_cache
            .iter()
            .filter(|e| {
                !e.is_folder()
                    && (crate::tg::is_image_name(e.name())
                        || crate::tg::is_video_name(e.name()))
            })
            .map(|e| e.id().to_string())
            .collect();
        if let Some(pos) = siblings.iter().position(|id| *id == file_id) {
            for (delta, glyph) in [(-1i32, "‹"), (1i32, "›")] {
                let target = pos as i32 + delta;
                if target >= 0 && target < siblings.len() as i32 {
                    let next_id = siblings[target as usize].clone();
                    controls = controls.child(
                        div()
                            .id(SharedString::from(format!("nav-{glyph}")))
                            .px_2()
                            .rounded_md()
                            .text_sm()
                            .text_color(theme().dim)
                            .hover(|s| s.bg(theme().panel2).text_color(theme().text))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                    this.open_viewer(&next_id, cx);
                                }),
                            )
                            .child(glyph),
                    );
                }
            }
        }
        if kind == ViewKind::Image {
            for (label, factor) in [("−", 0.75f32), ("+", 1.25f32), ("1:1", 0.)] {
                controls = controls.child(
                    div()
                        .id(SharedString::from(format!("zoom-{label}")))
                        .px_2()
                        .rounded_md()
                        .text_sm()
                        .text_color(theme().dim)
                        .hover(|s| s.bg(theme().panel2).text_color(theme().text))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                if let Some(v) = &mut this.viewer {
                                    v.zoom = if factor == 0. {
                                        1.
                                    } else {
                                        (v.zoom * factor).clamp(0.25, 8.)
                                    };
                                }
                                cx.notify();
                            }),
                        )
                        .child(label),
                );
            }
        }
        controls = controls.child(tool_button(
            "x",
            "Close",
            Tone::Ghost,
            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                this.viewer = None;
                cx.notify();
            }),
        ));

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgba(0x000000e6))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.viewer = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_3()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme().text)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(name),
                    )
                    .child(controls),
            )
            .child(body)
            .into_any_element()
    }
}
