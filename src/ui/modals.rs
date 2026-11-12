//! Modal dialogs — every confirm routes through `modal_confirm`
//! so Enter and click can never diverge.
use super::controls::{Tone, button, human_size, icon_button, icon_sized, tool_button_id};
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
            Modal::UploadOpts => {
                let Some(plan) = &self.upload_plan else {
                    return div().into_any_element();
                };
                let mut list = div()
                    .id("upload-pick-list")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .max_h(px(220.))
                    .overflow_y_scroll();
                for (i, item) in plan.items.iter().enumerate() {
                    let meta = if item.dir {
                        format!(
                            "{} files · {}",
                            item.files,
                            human_size(item.bytes as i64)
                        )
                    } else {
                        human_size(item.bytes as i64)
                    };
                    list = list.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(theme().panel2)
                            .child(icon_sized(
                                if item.dir { "folder" } else { "file" },
                                14.,
                                theme().file_icon,
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .overflow_hidden()
                                    .text_sm()
                                    .child(item.name.clone()),
                            )
                            .child(div().text_xs().text_color(theme().dim).child(meta))
                            .child({
                                let rm = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                    if let Some(p) = &mut this.upload_plan {
                                        p.items.remove(i);
                                        if p.items.is_empty() {
                                            this.modal = None;
                                        }
                                    }
                                    cx.notify();
                                });
                                icon_button(
                                    format!("pick-rm-{i}"),
                                    "x",
                                    theme().dim,
                                    theme().panel,
                                    rm,
                                )
                            }),
                    );
                }

                let mode = plan.mode;
                let pill = |label: &'static str, m: PlanMode, cx: &mut Context<Self>| {
                    let on = mode == m;
                    div()
                        .id(label)
                        .cursor_pointer()
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(if on { theme().accent } else { theme().border })
                        .bg(if on { theme().accent_soft } else { theme().panel2 })
                        .text_sm()
                        .text_color(if on { theme().accent } else { theme().ghost })
                        .child(label)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                if let Some(p) = &mut this.upload_plan {
                                    p.mode = m;
                                }
                                cx.notify();
                            }),
                        )
                };

                let mut body = div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(list)
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(pill("As files", PlanMode::Files, cx))
                            .child(pill("As folder", PlanMode::Folder, cx))
                            .child(pill("7z archive", PlanMode::Archive, cx)),
                    );
                if mode != PlanMode::Files {
                    body = body.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme().dim)
                                    .child(if mode == PlanMode::Folder {
                                        "Folder name — everything lands inside it"
                                    } else {
                                        "Archive name (.7z appended)"
                                    }),
                            )
                            .child(self.f_name.clone()),
                    );
                } else {
                    body = body.child(
                        div()
                            .text_xs()
                            .text_color(theme().dim)
                            .child("Files land here — picked folders keep their subfolders."),
                    );
                }
                (
                    "Upload".into(),
                    body.child({
                        let ok = cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            this.modal_confirm(cx)
                        });
                        button("Upload", theme().accent, ok)
                    })
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
