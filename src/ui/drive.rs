//! Drive browser — the file listing view, selection detail,
//! rows/cells, and the footer activity state.
use super::controls::{Tone, tool_button};
use super::*;

impl Root {
    pub(super) fn render_drive(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let channel_name = self
            .channel
            .as_ref()
            .map(|c| c.title.clone())
            .unwrap_or_default();

        // Toolbar
        let upload = cx.listener(|this, _: &MouseDownEvent, _, cx| this.pick_upload(cx));
        let backup = cx.listener(|this, _: &MouseDownEvent, _, cx| this.pick_backup(cx));
        let watch = cx.listener(|this, _: &MouseDownEvent, _, cx| {
            this.open_modal(Modal::WatchBackup, "", "/path/to/photos", cx);
            cx.notify();
        });
        let mount = cx.listener(|this, _: &MouseDownEvent, _, cx| {
            if this.mount_dir.is_some() {
                this.send(Command::Unmount);
            } else {
                this.open_modal(Modal::Mount, "", "/mnt/tdrive", cx);
            }
            cx.notify();
        });
        let uplim = cx.listener(|this, _: &MouseDownEvent, _, cx| {
            this.open_modal(Modal::UploadLimit, "", "3", cx);
            cx.notify();
        });
        let refresh = cx.listener(|this, _: &MouseDownEvent, _, cx| {
            this.send(Command::Sync);
            this.status = "syncing…".into();
            cx.notify();
        });
        let new_folder = cx.listener(|this, _: &MouseDownEvent, _, cx| {
            this.open_modal(Modal::NewFolder, "", "folder name", cx);
            cx.notify();
        });
        let back = cx.listener(|this, _: &MouseDownEvent, _, cx| {
            this.channel = None;
            this.screen = Screen::Picker;
            this.send(Command::ListChannels);
            cx.notify();
        });
        let sign_out = cx.listener(|this, _: &MouseDownEvent, _, cx| {
            this.send(Command::SignOut);
            cx.notify();
        });
        let trash_toggle = cx.listener(|this, _: &MouseDownEvent, _, cx| {
            this.trash = !this.trash;
            this.selected = None;
            cx.notify();
        });
        let v_configured = self.vault_configured;
        let v_unlocked = self.vault_unlocked;
        let v_hint = self.vault_hint.clone();
        let lock_btn = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
            if v_configured && v_unlocked {
                this.send(Command::Lock);
            } else if v_configured {
                this.modal = Some(Modal::Unlock {
                    hint: v_hint.clone(),
                });
            } else {
                this.modal = Some(Modal::Encrypt);
            }
            cx.notify();
        });

        let mut toolbar = div()
            .flex()
            .items_center()
            .gap_2()
            .child(tool_button("Upload", Tone::Primary, upload))
            .child(tool_button("New folder", Tone::Ghost, new_folder))
            .child(tool_button("Backup", Tone::Ghost, backup))
            .child(tool_button("Watch", Tone::Ghost, watch))
            .child(tool_button(
                if self.mount_dir.is_some() {
                    "Unmount"
                } else {
                    "Mount"
                },
                Tone::Ghost,
                mount,
            ))
            .child(tool_button("Sync", Tone::Ghost, refresh))
            .child(tool_button("Limit", Tone::Ghost, uplim))
            .child(tool_button(
                if self.trash { "Files" } else { "Trash" },
                Tone::Ghost,
                trash_toggle,
            ));
        if self.trash {
            let et = cx.listener(|this, _: &MouseDownEvent, _, cx| {
                this.send(Command::EmptyTrash);
                cx.notify();
            });
            toolbar = toolbar.child(tool_button("Empty trash", Tone::Danger, et));
        }
        if !self.trash {
            let opt = cx.listener(|this, _: &MouseDownEvent, _, cx| {
                this.send(Command::OptimizeDrive);
                cx.notify();
            });
            toolbar = toolbar.child(tool_button("Optimize", Tone::Ghost, opt));
        }
        if self.channel_kind.is_personal() {
            let lock_label = if self.vault_configured {
                if self.vault_unlocked {
                    "Unlocked"
                } else {
                    "Unlock"
                }
            } else {
                "Encrypt"
            };
            toolbar = toolbar.child(tool_button(lock_label, Tone::Ghost, lock_btn));
        }
        toolbar = toolbar.child(div().w(px(150.)).child(self.f_search.clone()));
        if !self.namespace.unadopted.is_empty() && !self.trash {
            let n = self.namespace.unadopted.len();
            let ad = cx.listener(|this, _: &MouseDownEvent, _, cx| {
                this.modal = Some(Modal::Adopt);
                cx.notify();
            });
            toolbar = toolbar.child(tool_button(&format!("Unadopted ({n})"), Tone::Ghost, ad));
        }
        if !self.trash {
            let label = if self.grid { "List" } else { "Grid" };
            let gt = cx.listener(|this, _: &MouseDownEvent, _, cx| {
                this.grid = !this.grid;
                cx.notify();
            });
            toolbar = toolbar.child(tool_button(label, Tone::Ghost, gt));
        }
        toolbar = toolbar
            .child(div().flex_1())
            .child(tool_button("Drives", Tone::Ghost, back))
            .child(tool_button("Sign out", Tone::Ghost, sign_out));

        // Breadcrumb
        let mut crumbs = div().flex().items_center().gap_1().text_sm().child(
            div()
                .id("crumb-root")
                .cursor_pointer()
                .text_color(rgb(DIM))
                .hover(|s| s.text_color(rgb(ACCENT)))
                .child("root")
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.set_cwd(String::new());
                        cx.notify();
                    }),
                ),
        );
        for (id, name) in self.namespace.folder_path(&self.cwd) {
            crumbs = crumbs
                .child(div().text_color(rgb(0x4d5666)).child(" / "))
                .child(
                    div()
                        .id(SharedString::from(format!("crumb-{id}")))
                        .cursor_pointer()
                        .text_color(rgb(DIM))
                        .hover(|s| s.text_color(rgb(ACCENT)))
                        .child(name)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.set_cwd(id.clone());
                                cx.notify();
                            }),
                        ),
                );
        }

        // File rows — trash view, search results, or folder listing.
        let view_key = format!(
            "{}|{}|{}|{}|{}",
            self.cwd,
            self.trash,
            self.grid,
            self.f_search.read(cx).text(),
            self.channel.as_ref().map(|c| c.id).unwrap_or(0)
        );
        if view_key != self.last_view_key {
            self.scroll_handle = ScrollHandle::new();
            self.last_view_key = view_key;
        }
        self.entries_cache = self.current_entries(cx);
        if self.entries_cache.len() != self.last_items_len {
            // Content changed — a new folder/file may sit above the old
            // scroll offset. Snap to the top so it's visible.
            self.last_items_len = self.entries_cache.len();
            self.scroll_handle.scroll_to_top_of_item(0);
        }
        // ONE scrollable container — nesting a second overflow_y_scroll
        // inside it gave the inner pane a full-height scroll range, so it
        // swallowed wheel events and entries below the fold (files, which
        // sort after folders) were unreachable.
        let mut body = div()
            .id("file-scroll")
            .flex()
            .flex_col()
            .w_full()
            .flex_1()
            .overflow_y_scroll()
            .track_scroll(&self.scroll_handle);
        if self.entries_cache.is_empty() {
            body = body.child(
                div()
                    .py_16()
                    .flex()
                    .justify_center()
                    .text_sm()
                    .text_color(rgb(DIM))
                    .child(if self.trash {
                        "trash is empty"
                    } else if !self.f_search.read(cx).text().trim().is_empty() {
                        "no matches"
                    } else {
                        "empty folder"
                    }),
            );
        } else if self.grid && !self.trash {
            self.grid_cols = ((f32::from(window.viewport_size().width) / 110.) as usize).max(1);
            for chunk in self.entries_cache.clone().chunks(self.grid_cols) {
                let mut r = div().flex().gap_2().p_1();
                for e in chunk {
                    r = r.child(self.render_cell(e, cx));
                }
                body = body.child(r);
            }
        } else {
            for e in self.entries_cache.clone() {
                body = body.child(self.render_row(&e, cx));
            }
        }

        // Selected actions
        let mut actions = div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(36.))
            .px_3()
            .border_b_1()
            .border_color(rgb(BORDER))
            .bg(rgb(PANEL));
        if let Some(sel) = &self.selected {
            let name = self
                .namespace
                .files
                .get(sel)
                .map(|f| f.name.clone())
                .or_else(|| self.namespace.folders.get(sel).map(|f| f.name.clone()))
                .unwrap_or_default();
            let sel_c = sel.clone();
            let is_file = crate::wire::is_file_id(&sel);
            if let Some(path) = self.thumbnails.get(&sel_c) {
                actions = actions.child(img(path.clone()).w(px(28.)).h(px(28.)));
            }
            actions = actions
                .child(div().text_sm().text_color(rgb(TEXT)).child(name.clone()))
                .child(div().flex_1());
            if self.trash {
                let obj = sel_c.clone();
                let rs = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.send(Command::Restore { obj: obj.clone() });
                    this.selected = None;
                    cx.notify();
                });
                let obj2 = sel_c.clone();
                let pg = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.send(Command::Purge { obj: obj2.clone() });
                    this.selected = None;
                    cx.notify();
                });
                actions = actions
                    .child(tool_button("Restore", Tone::Ghost, rs))
                    .child(tool_button("Delete forever", Tone::Danger, pg));
            } else if is_file {
                let file = self.namespace.files.get(sel).cloned();
                if let Some(file) = file {
                    let dl = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        this.pick_download_dest(file.id.clone(), file.name.clone(), cx)
                    });
                    actions = actions.child(tool_button("Download", Tone::Ghost, dl));
                    let id = sel_c.clone();
                    let st = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        this.send(Command::Stream { obj: id.clone() });
                        this.status = "opening stream…".into();
                        cx.notify();
                    });
                    actions = actions.child(tool_button("Open", Tone::Ghost, st));
                    let id2 = sel_c.clone();
                    let vw = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        this.open_viewer(&id2, cx);
                    });
                    actions = actions.child(tool_button("View", Tone::Ghost, vw));
                }
            }
            if !self.trash {
                let obj = sel_c.clone();
                let nm2 = name.clone();
                let rn = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.open_modal(Modal::Rename { obj: obj.clone() }, &nm2, "new name", cx);
                });
                let obj2 = sel_c.clone();
                let mv = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.modal = Some(Modal::Move { obj: obj2.clone() });
                    cx.notify();
                });
                let obj3 = sel_c.clone();
                let nm = name.clone();
                let del = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.modal = Some(Modal::Delete {
                        obj: obj3.clone(),
                        name: nm.clone(),
                    });
                    cx.notify();
                });
                actions = actions
                    .child(tool_button("Rename", Tone::Ghost, rn))
                    .child(tool_button("Move", Tone::Ghost, mv))
                    .child(tool_button("Delete", Tone::Danger, del));
            }
        }

        // Transfers — collapsed pins the in-flight rows on top; expanded
        // swaps in a draggable pane with the full list.
        let active_count = self
            .transfers
            .values()
            .filter(|t| t.state == TransferState::Active || t.state == TransferState::Paused)
            .count();
        let total = self.transfers.len();
        let limit = self
            .config
            .as_ref()
            .map(|c| c.upload_limit)
            .unwrap_or_else(crate::config::default_upload_limit)
            .max(1);
        let mut ordered: Vec<TransferUpdate> = self.transfers.values().cloned().collect();
        ordered.sort_by_key(|t| match t.state {
            TransferState::Active => 0,
            TransferState::Paused => 1,
            TransferState::Failed => 2,
            TransferState::Done => 3,
        });

        let mut transfers = div().flex().flex_col().w_full();
        if self.transfers_expanded {
            // Top edge of the pane — the resize handle lives here, not
            // between the header and the list.
            transfers = transfers.child(
                div()
                    .id("transfers-drag")
                    .h(px(8.))
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor(gpui::CursorStyle::ResizeUpDown)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                            this.transfers_drag =
                                Some((f32::from(ev.position.y), this.transfers_height));
                            cx.notify();
                        }),
                    )
                    .child(div().w(px(36.)).h(px(3.)).rounded_full().bg(rgb(BORDER))),
            );
        }
        transfers = transfers.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .pt_1()
                .pb_1()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(DIM))
                        .child(format!("Transfers · {active_count} active · {total} total")),
                )
                .child(if total > limit {
                    div()
                        .id("transfers-toggle")
                        .px_2()
                        .rounded_md()
                        .text_xs()
                        .text_color(rgb(DIM))
                        .hover(|s| s.bg(rgb(PANEL2)).text_color(rgb(TEXT)))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                this.transfers_expanded = !this.transfers_expanded;
                                cx.notify();
                            }),
                        )
                        .child(if self.transfers_expanded {
                            "▾ collapse"
                        } else {
                            "▸ show all"
                        })
                        .into_any_element()
                } else {
                    div().into_any_element()
                }),
        );
        if self.transfers_expanded {
            transfers = transfers.child({
                let mut list = div()
                    .id("transfers-scroll")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_3()
                    .pb_2()
                    .h(px(self.transfers_height))
                    .overflow_y_scroll();
                for t in &ordered {
                    list = list.child(self.render_transfer(t, cx));
                }
                list
            });
        } else {
            // Pinned view: the in-flight transfers only.
            let mut pinned = div().flex().flex_col().gap_1().px_3().pb_2();
            for t in ordered.iter().take(limit) {
                pinned = pinned.child(self.render_transfer(t, cx));
            }
            transfers = transfers.child(pinned);
        }

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(BG))
            .track_focus(&self.drive_focus)
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.navigate_back();
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Forward),
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.navigate_fwd();
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                if let Some((sy, sh)) = this.transfers_drag {
                    if ev.pressed_button.is_some() {
                        this.transfers_height =
                            (sh + (sy - f32::from(ev.position.y))).clamp(64., 480.);
                    } else {
                        this.transfers_drag = None;
                    }
                    cx.notify();
                }
            }))
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if this.modal.is_some() || this.f_search.read(cx).is_focused(window) {
                    return;
                }
                if this.viewer.is_some() {
                    if matches!(e.keystroke.key.as_str(), "escape" | "backspace") {
                        this.viewer = None;
                        cx.notify();
                    }
                    return;
                }
                let ids = this.visible_ids(cx);
                match e.keystroke.key.as_str() {
                    "up" | "down" => {
                        if ids.is_empty() {
                            return;
                        }
                        let idx = ids
                            .iter()
                            .position(|i| Some(i) == this.selected.as_ref())
                            .map(|i| {
                                if e.keystroke.key == "up" {
                                    i.saturating_sub(1)
                                } else {
                                    (i + 1).min(ids.len() - 1)
                                }
                            })
                            .unwrap_or(0);
                        this.selected = Some(ids[idx].clone());
                        let row = if this.grid && !this.trash {
                            idx / this.grid_cols.max(1)
                        } else {
                            idx
                        };
                        this.scroll_handle.scroll_to_top_of_item(row);
                    }
                    "enter" => {
                        let Some(sel) = this.selected.clone() else {
                            return;
                        };
                        if crate::wire::is_folder_id(&sel) {
                            this.set_cwd(sel);
                        } else if let Some(f) = this.namespace.files.get(&sel).cloned() {
                            if !this.trash {
                                this.pick_download_dest(f.id, f.name, cx);
                            }
                        }
                    }
                    "backspace" => {
                        let parent = this
                            .namespace
                            .folders
                            .get(&this.cwd)
                            .map(|d| d.parent.clone())
                            .unwrap_or_default();
                        this.cwd = parent;
                        this.selected = None;
                    }
                    "delete" => {
                        let Some(sel) = this.selected.clone() else {
                            return;
                        };
                        let name = this
                            .namespace
                            .files
                            .get(&sel)
                            .map(|f| f.name.clone())
                            .or_else(|| this.namespace.folders.get(&sel).map(|d| d.name.clone()))
                            .unwrap_or_default();
                        this.modal = Some(Modal::Delete { obj: sel, name });
                    }
                    _ => return,
                }
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .px_4()
                    .py_2()
                    .bg(rgb(PANEL))
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(TEXT))
                            .child("TDrive"),
                    )
                    .child(div().text_color(rgb(DIM)).child("·"))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(TEXT))
                            .child(channel_name),
                    )
                    .child(div().flex_1())
                    .child(toolbar),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .px_4()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(crumbs),
            )
            .child(actions)
            .child(body)
            .child(
                div()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(PANEL))
                    .child(transfers)
                    .child({
                        const FRAMES: [&str; 10] =
                            ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                        let pending = self
                            .thumb_requested
                            .len()
                            .saturating_sub(self.thumbnails.len());
                        let active_transfers = self
                            .transfers
                            .values()
                            .filter(|t| t.state == TransferState::Active)
                            .count();
                        let label = if self.syncing && pending > 0 {
                            format!("Fetching files… · {pending} thumbnails in flight")
                        } else if self.syncing {
                            "Fetching files…".to_string()
                        } else if pending > 0 {
                            format!("Fetching thumbnails · {pending} in flight")
                        } else if active_transfers > 0 {
                            format!(
                                "{active_transfers} transfer{} running",
                                if active_transfers == 1 { "" } else { "s" }
                            )
                        } else {
                            String::new()
                        };
                        if label.is_empty() {
                            div().into_any_element()
                        } else {
                            div()
                                .h(px(28.))
                                .w_full()
                                .px_3()
                                .flex()
                                .items_center()
                                .gap_3()
                                .border_t_1()
                                .border_color(rgb(BORDER))
                                .child(
                                    div()
                                        .w(px(16.))
                                        .text_sm()
                                        .text_color(rgb(ACCENT))
                                        .child(FRAMES[(self.activity_frame % 10) as usize]),
                                )
                                .child(div().w(px(90.)).h(px(3.)).rounded_sm().bg(rgb(ACCENT)))
                                .child(div().text_xs().text_color(rgb(DIM)).child(label))
                                .into_any_element()
                        }
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(DIM))
                            .px_3()
                            .py_1()
                            .child(format!("{} items", self.entries_cache.len())),
                    )
                    .child(
                        div()
                            .px_4()
                            .py_1()
                            .text_sm()
                            .text_color(rgb(DIM))
                            .child(self.status.clone()),
                    ),
            )
            .child(self.render_modal(window, cx))
            .child(self.render_viewer(cx))
    }

    /// Whether background work is in flight — drives the bottom-left
    /// activity spinner.
    pub(super) fn activity_active(&self) -> bool {
        self.syncing
            || self
                .transfers
                .values()
                .any(|t| t.state == TransferState::Active)
            || self.thumb_requested.len() > self.thumbnails.len()
    }

    /// Ids in display order — derived from `current_entries` so
    /// keyboard selection can never index a different order.
    pub(super) fn visible_ids(&self, cx: &App) -> Vec<String> {
        self.current_entries(cx)
            .iter()
            .map(|e| e.id().to_string())
            .collect()
    }

    /// Entries for the current view (trash / search / folder listing) —
    /// the projection owns each derivation.
    pub(super) fn current_entries(&self, cx: &App) -> Vec<Entry> {
        if self.trash {
            return self.namespace.trash_entries();
        }
        let search = self.f_search.read(cx).text().trim().to_lowercase();
        if !search.is_empty() {
            return self.namespace.search(&search);
        }
        self.namespace.list(&self.cwd)
    }

    /// Gallery-style cell: thumbnail (fetched on demand) or type icon.
    pub(super) fn render_cell(
        &mut self,
        entry: &Entry,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (id, name, is_folder) = (
            entry.id().to_string(),
            entry.name().to_string(),
            entry.is_folder(),
        );
        // Ask the worker for this file's rendition once.
        if !is_folder && self.namespace.has_rendition(&id) && !self.thumb_requested.contains(&id) {
            self.thumb_requested.insert(id.clone());
            self.send(Command::FetchThumbnail {
                file_id: id.clone(),
            });
        }
        let selected = self.selected.as_deref() == Some(id.as_str());
        let click_id = id.clone();
        let thumb = self.thumbnails.get(&id).cloned();
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .w(px(100.))
            .p_2()
            .rounded_lg()
            .bg(if selected {
                rgba(0x61afef21)
            } else {
                rgba(0x00000000)
            })
            .hover(|s| s.bg(rgb(PANEL)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    this.drive_focus.focus(window);
                    if ev.click_count == 2 && is_folder {
                        this.set_cwd(click_id.clone());
                    } else {
                        this.selected = Some(click_id.clone());
                    }
                    cx.notify();
                }),
            )
            .child(
                div()
                    .w(px(80.))
                    .h(px(64.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if let Some(p) = thumb {
                        img(p).w(px(80.)).h(px(64.)).rounded_md().into_any_element()
                    } else {
                        div()
                            .w(px(80.))
                            .h(px(64.))
                            .rounded_md()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(if is_folder {
                                rgba(0x61afef1a)
                            } else {
                                rgb(PANEL2)
                            })
                            .child(div().w(px(14.)).h(px(14.)).rounded_sm().bg(if is_folder {
                                rgb(ACCENT)
                            } else {
                                rgb(0x4d5666)
                            }))
                            .into_any_element()
                    }),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(TEXT))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(name),
            )
    }

    pub(super) fn render_row(&mut self, entry: &Entry, cx: &mut Context<Self>) -> impl IntoElement {
        let (id, name, meta, is_folder) = match entry {
            Entry::Folder(f) => (f.id.clone(), f.name.clone(), "folder".to_string(), true),
            Entry::File(f) => (
                f.id.clone(),
                f.name.clone(),
                if f.encrypted {
                    format!("{} · encrypted", human_size(f.logical_size()))
                } else {
                    human_size(f.logical_size())
                },
                false,
            ),
        };
        let selected = self.selected.as_deref() == Some(id.as_str());
        let row_id = format!("row-{id}");
        let id_click = id.clone();

        div()
            .id(SharedString::from(row_id))
            .flex()
            .items_center()
            .gap_3()
            .w_full()
            .px_4()
            .py_1()
            .bg(if selected {
                rgb(0x3e4452)
            } else {
                rgba(0x00000000)
            })
            .hover(|s| s.bg(rgb(PANEL2)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    this.drive_focus.focus(window);
                    if ev.click_count == 2 && is_folder {
                        this.set_cwd(id_click.clone());
                    } else if ev.click_count == 2 {
                        this.open_viewer(&id_click, cx);
                    } else {
                        this.selected = Some(id_click.clone());
                        this.send(Command::FetchThumbnail {
                            file_id: id_click.clone(),
                        });
                    }
                    cx.notify();
                }),
            )
            .child(div().w(px(22.)).flex().items_center().child(
                div().w(px(9.)).h(px(9.)).rounded_sm().bg(if is_folder {
                    rgb(ACCENT)
                } else {
                    rgb(0x4d5666)
                }),
            ))
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(rgb(TEXT))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(name),
            )
            .child(
                div()
                    .w(px(140.))
                    .text_xs()
                    .font_family("monospace")
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_color(rgb(DIM))
                    .flex()
                    .justify_end()
                    .child(meta),
            )
    }
}
