//! Modal dialogs — every confirm routes through `modal_confirm`
//! so Enter and click can never diverge.
use super::controls::{Tone, button, tool_button_id};
use super::*;

impl Root {
    pub(super) fn render_modal(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(modal) = &self.modal else {
            return div().into_any_element();
        };
        let cancel = cx.listener(|this, _: &MouseDownEvent, _, cx| {
            this.modal = None;
            cx.notify();
        });

        let (title, body): (SharedString, AnyElement) = match modal {
            Modal::NewFolder => (
                "New folder".into(),
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(self.f_name.clone())
                    .child({
                        let ok = cx.listener(|this, _: &MouseDownEvent, _, cx| this.modal_confirm(cx));
                        button("Create", theme().accent, ok)
                    })
                    .into_any_element(),
            ),
            Modal::MoveToNew { .. } => (
                "Move to new folder".into(),
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(self.f_name.clone())
                    .child({
                        let ok = cx.listener(|this, _: &MouseDownEvent, _, cx| this.modal_confirm(cx));
                        button("Move", theme().accent, ok)
                    })
                    .into_any_element(),
            ),
            Modal::Rename { .. } => {
                (
                    "Rename".into(),
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(self.f_name.clone())
                        .child({
                            let ok = cx.listener(|this, _: &MouseDownEvent, _, cx| this.modal_confirm(cx));
                            button("Rename", theme().accent, ok)
                        })
                        .into_any_element(),
                )
            }
            Modal::Move { objs } => {
                let objs = objs.clone();
                let mut list = div()
                    .id("move-list")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .max_h(px(200.))
                    .overflow_y_scroll();
                let root_dest = "".to_string();
                let o = objs.clone();
                list = list.child(move_dest_row("root", "", {
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        this.confirm_move(&o, &root_dest, cx);
                        cx.notify();
                    })
                }));
                for d in self.namespace.folders.values() {
                    // A folder can't move into itself or its own subtree —
                    // the projection would reject it after the send.
                    if d.tombstoned
                        || objs
                            .iter()
                            .any(|o| self.namespace.subtree_contains(o, &d.id))
                    {
                        continue;
                    }
                    let dest = d.id.clone();
                    let o = objs.clone();
                    list = list.child(move_dest_row(&d.name, &d.id, {
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.confirm_move(&o, &dest, cx);
                            cx.notify();
                        })
                    }));
                }
                (
                    if objs.len() == 1 {
                        "Move to…".into()
                    } else {
                        format!("Move {} items to…", objs.len()).into()
                    },
                    list.into_any_element(),
                )
            }
            Modal::Unlock { hint } => (
                format!("Unlock drive{}", if hint.is_empty() { "".into() } else { format!(" (hint: {hint})") }).into(),
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(self.f_password.clone())
                    .child({
                        let ok = cx.listener(|this, _: &MouseDownEvent, _, cx| this.modal_confirm(cx));
                        button("Unlock", theme().accent, ok)
                    })
                    .into_any_element(),
            ),
            Modal::Encrypt => (
                "Enable encryption".into(),
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme().dim)
                            .child("Creates a password vault on this drive. There is no recovery without the password."),
                    )
                    .child(self.f_password.clone())
                    .child(self.f_channel.clone())
                    .child({
                        let ok = cx.listener(|this, _: &MouseDownEvent, _, cx| this.modal_confirm(cx));
                        button("Create vault", theme().accent, ok)
                    })
                    .into_any_element(),
            ),
            Modal::Mount => {
                (
                    "Mount drive".into(),
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(div().text_sm().text_color(theme().dim).child(
                            "Mount point path — the drive appears as a filesystem.",
                        ))
                        .child(self.f_name.clone())
                        .child({
                            let ok = cx.listener(|this, _: &MouseDownEvent, _, cx| this.modal_confirm(cx));
                            button("Mount", theme().accent, ok)
                        })
                        .into_any_element(),
                )
            }
            Modal::WatchBackup => {
                (
                    "Watch backup folder".into(),
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(div().text_sm().text_color(theme().dim).child(
                            "New images in this folder are uploaded to Photos periodically.",
                        ))
                        .child(self.f_name.clone())
                        .child({
                            let ok = cx.listener(|this, _: &MouseDownEvent, _, cx| this.modal_confirm(cx));
                            button("Watch", theme().accent, ok)
                        })
                        .into_any_element(),
                )
            }
            Modal::Adopt => {
                let mut list = div().flex().flex_col().gap_1();
                for e in self.namespace.unadopted.iter() {
                    let msg_id = e.msg_id;
                    let parent = self.cwd.clone();
                    let ab = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        this.send(Command::Adopt { msg_id, parent: parent.clone() });
                        this.modal = None;
                        cx.notify();
                    });
                    list = list.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().text_sm().child(format!("{} ({})", e.name, human_size(e.size))))
                            .child(tool_button_id(format!("adopt-{}", e.msg_id), "download", "Adopt", Tone::Ghost, ab)),
                    );
                }
                ("Unadopted attachments".into(), list.into_any_element())
            }
            Modal::UploadDirs { dirs } => {
                let label = if dirs.len() == 1 {
                    dirs[0]
                        .file_name()
                        .map(|s| format!("Upload “{}” as:", s.to_string_lossy()))
                        .unwrap_or_else(|| "Upload folder as:".into())
                } else {
                    format!("Upload {} folders as:", dirs.len())
                };
                let dirs3 = dirs.clone();
                (
                    "Upload folder".into(),
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(div().text_sm().text_color(theme().dim).child(label))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child({
                                    let ok = cx.listener(|this, _: &MouseDownEvent, _, cx| this.modal_confirm(cx));
                                    button("As folder", theme().accent, ok)
                                })
                                .child({
                                    let ok = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                        for dir in dirs3.clone() {
                                            this.send(Command::UploadArchive {
                                                dir,
                                                parent: this.cwd.clone(),
                                            });
                                        }
                                        this.modal = None;
                                        cx.notify();
                                    });
                                    button(".7z archive", theme().accent, ok)
                                }),
                        )
                        .into_any_element(),
                )
            }
            Modal::UploadLimit => (
                "Upload limit".into(),
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme().dim)
                            .child("Max parallel uploads (1–16, default 3)"),
                    )
                    .child(self.f_name.clone())
                    .child({
                        let ok = cx.listener(|this, _: &MouseDownEvent, _, cx| this.modal_confirm(cx));
                        button("Set", theme().accent, ok)
                    })
                    .into_any_element(),
            ),
            Modal::Optimize { dead } => (
                "Optimize drive".into(),
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div().text_sm().text_color(theme().dim).child(format!(
                            "{dead} obsolete messages — ops and bodies of files and                              folders that were created and later deleted — can be                              removed from the channel. Live files and unadopted                              uploads are untouched.",
                        )),
                    )
                    .child({
                        let ok = cx.listener(|this, _: &MouseDownEvent, _, cx| this.modal_confirm(cx));
                        button("Delete messages", theme().accent, ok)
                    })
                    .into_any_element(),
            ),
            Modal::Delete { objs, name } => {
                let objs = objs.clone();
                (
                    "Delete".into(),
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme().dim)
                                .child(format!("Delete “{name}”?")),
                        )
                        .child({
                            let ok = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                this.confirm_delete(&objs, cx);
                                cx.notify();
                            });
                            button("Delete", theme().danger, ok)
                        })
                        .into_any_element(),
                )
            }
        };

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x00000080))
            .on_mouse_down(MouseButton::Left, cancel)
            .child(
                div()
                    .w(px(400.))
                    .flex()
                    .flex_col()
                    .gap_4()
                    .p_5()
                    .rounded_lg()
                    .bg(theme().panel)
                    .border_1()
                    .border_color(theme().border)
                    .shadow_lg()
                    // stop clicks inside the card from closing the modal
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                        match e.keystroke.key.as_str() {
                            "escape" => {
                                this.modal = None;
                                cx.notify();
                            }
                            "enter" => this.modal_confirm(cx),
                            _ => {}
                        }
                    }))
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme().text)
                            .child(title),
                    )
                    .child(body),
            )
            .into_any_element()
    }
}

fn move_dest_row(
    name: &str,
    id: &str,
    on_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(format!(
            "dest-{}",
            if id.is_empty() { "root" } else { id }
        )))
        .px_3()
        .py_2()
        .rounded_md()
        .bg(theme().panel2)
        .hover(|s| s.bg(theme().panel2))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, on_click)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(super::controls::icon_sized("folder", 13., theme().accent))
                .child(
                    div()
                        .text_sm()
                        .text_color(theme().text)
                        .child(name.to_string()),
                ),
        )
}
