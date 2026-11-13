//! Drive browser — the file listing view, selection detail,
//! rows/cells, and the footer activity state.
use super::controls::{
    Tone, entry_icon, folder_bg, folder_icon, icon_button, icon_sized, tool_button,
};
use super::*;

// Fixed geometry so the rubber band can hit-test items by index math
// instead of measuring rendered bounds.
pub(super) const ROW_H: f32 = 38.;
pub(super) const CELL_W: f32 = 100.;
// Two name lines fit under the thumb — long names wrap then clamp.
// The label's explicit line height leaves room for descenders.
pub(super) const CELL_H: f32 = 124.;
pub(super) const GRID_PAD: f32 = 4.;
pub(super) const GRID_GAP: f32 = 8.;
pub(super) const GRID_ROW_H: f32 = CELL_H + 2. * GRID_PAD;
/// Band smaller than this on release counts as a click, not a drag.
const RUBBER_CLICK: f32 = 4.;
/// Drag-move threshold for an armed press on a selected row.
const DRAG_ARM: f32 = 6.;
/// Titlebar height.
const BAR_H: f32 = 36.;
/// Working-bar height — a little taller than the titlebar so the path
/// and buttons read as the primary surface, not an accessory strip.
const CTX_H: f32 = 42.;

/// Actions that apply to the current selection — shared between the
/// selection bar and the right-click menu.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum SelAct {
    Download,
    Open,
    View,
    Rename,
    Copy,
    Cut,
    Paste,
    Move,
    MoveToNew,
    Style,
    Delete,
    Restore,
    Purge,
}

impl SelAct {
    fn meta(self) -> (&'static str, &'static str, Tone) {
        match self {
            SelAct::Download => ("download", "Download", Tone::Ghost),
            SelAct::Open => ("external", "Open", Tone::Ghost),
            SelAct::View => ("eye", "View", Tone::Ghost),
            SelAct::Rename => ("pencil", "Rename", Tone::Ghost),
            SelAct::Copy => ("copy", "Copy", Tone::Ghost),
            SelAct::Cut => ("cut", "Cut", Tone::Ghost),
            SelAct::Paste => ("paste", "Paste", Tone::Ghost),
            SelAct::Move => ("move", "Move", Tone::Ghost),
            SelAct::MoveToNew => ("folder_up", "Move to new folder", Tone::Ghost),
            SelAct::Style => ("sliders", "Style", Tone::Ghost),
            SelAct::Delete => ("trash", "Delete", Tone::Danger),
            SelAct::Restore => ("restore", "Restore", Tone::Ghost),
            SelAct::Purge => ("trash", "Delete forever", Tone::Danger),
        }
    }
}

/// Drive-level operations behind the Tools dropdown — infrequent, so
/// they don't earn permanent toolbar space.
#[derive(Clone, Copy)]
enum ToolAct {
    Backup,
    Watch,
    Mount,
    Limit,
    Optimize,
    Lock,
}

/// Item kind for the shared popup menu — file right-click and the
/// Tools dropdown render identically, only the dispatcher differs.
/// Actions offered by the empty-space right-click menu.
#[derive(Clone, Copy)]
enum AreaAct {
    Upload,
    NewFolder,
    Paste,
}

#[derive(Clone, Copy)]
enum MenuAct {
    Sel(SelAct),
    Tool(ToolAct),
    Area(AreaAct),
}

impl Root {
    /// Which actions apply right now — Restore/Purge in trash; file-only
    /// and single-only actions hidden when the selection doesn't fit.
    fn sel_acts(&self) -> Vec<SelAct> {
        if self.selected.is_empty() {
            return Vec::new();
        }
        if self.trash {
            return vec![SelAct::Restore, SelAct::Purge];
        }
        let single = self.selected.len() == 1;
        let mut acts = Vec::new();
        if single && crate::wire::is_file_id(&self.selected[0]) {
            acts.extend([SelAct::Download, SelAct::Open, SelAct::View]);
        }
        if single {
            acts.push(SelAct::Rename);
            if !crate::wire::is_file_id(&self.selected[0]) {
                acts.push(SelAct::Style);
            }
        }
        acts.extend([SelAct::Copy, SelAct::Cut, SelAct::Move, SelAct::MoveToNew]);
        if self.clip.is_some() {
            acts.push(SelAct::Paste);
        }
        acts.push(SelAct::Delete);
        acts
    }

    /// Selection actions shown inline in the bar — everything except
    /// "Move to new folder", which stays in the right-click menu so a
    /// wide selection never overflows the window edge.
    fn bar_acts(&self) -> Vec<SelAct> {
        self.sel_acts()
            .into_iter()
            .filter(|a| *a != SelAct::MoveToNew)
            .collect()
    }

    /// Label for the delete confirmation — one name or a count.
    fn sel_label(&self) -> String {
        if self.selected.len() == 1 {
            let sel = &self.selected[0];
            self.namespace
                .files
                .get(sel)
                .map(|f| f.name.clone())
                .or_else(|| self.namespace.folders.get(sel).map(|f| f.name.clone()))
                .unwrap_or_default()
        } else {
            format!("{} items", self.selected.len())
        }
    }

    fn fire_sel_act(&mut self, act: SelAct, cx: &mut Context<Self>) {
        self.context_menu = None;
        let sel = self.selected.clone();
        match act {
            SelAct::Download => {
                if let Some(f) = self.namespace.files.get(&sel[0]).cloned() {
                    self.pick_download_dest(f.id, f.name, cx);
                }
            }
            SelAct::Open => {
                self.send(Command::Stream {
                    obj: sel[0].clone(),
                });
                self.status = "opening stream…".into();
            }
            SelAct::View => self.open_viewer(&sel[0], cx),
            SelAct::Rename => {
                let name = self.sel_label();
                self.open_modal(
                    Modal::Rename {
                        obj: sel[0].clone(),
                    },
                    &name,
                    "new name",
                    cx,
                );
            }
            SelAct::Copy | SelAct::Cut => {
                let n = sel.len();
                self.clip = Some(Clip {
                    objs: sel,
                    cut: act == SelAct::Cut,
                });
                self.status = format!(
                    "{n} item{} {}",
                    if n == 1 { "" } else { "s" },
                    if act == SelAct::Cut { "cut" } else { "copied" },
                );
            }
            SelAct::Paste => {
                // A lone selected folder is the drop target; anything
                // else pastes into the current folder.
                let dest = if self.selected.len() == 1
                    && crate::wire::is_folder_id(&self.selected[0])
                {
                    self.selected[0].clone()
                } else {
                    self.cwd.clone()
                };
                self.paste_clip(dest, cx);
            }
            SelAct::Move => {
                self.modal = Some(Modal::Move { objs: sel });
            }
            SelAct::MoveToNew => {
                self.open_modal(
                    Modal::MoveToNew { objs: sel },
                    "",
                    "new folder name",
                    cx,
                );
            }
            SelAct::Style => {
                self.modal = Some(Modal::Style {
                    obj: sel[0].clone(),
                });
            }
            SelAct::Delete => {
                self.modal = Some(Modal::Delete {
                    objs: sel,
                    name: self.sel_label(),
                });
            }
            SelAct::Restore | SelAct::Purge => {
                for obj in &sel {
                    self.send(if act == SelAct::Restore {
                        Command::Restore {
                            obj: obj.clone(),
                        }
                    } else {
                        Command::Purge {
                            obj: obj.clone(),
                        }
                    });
                }
                self.mark_pending(
                    &sel,
                    if act == SelAct::Purge {
                        PendKind::Delete
                    } else {
                        PendKind::Timed
                    },
                    "",
                    cx,
                );
                self.selected.clear();
            }
        }
        cx.notify();
    }

    /// Tools-dropdown actions — identical to what the old toolbar row
    /// dispatched, just reached via one menu instead of six buttons.
    fn fire_tool(&mut self, act: ToolAct, cx: &mut Context<Self>) {
        self.tools_menu = None;
        match act {
            ToolAct::Backup => self.pick_backup(cx),
            ToolAct::Watch => self.open_modal(Modal::WatchBackup, "", "/path/to/photos", cx),
            ToolAct::Mount => {
                if self.mount_dir.is_some() {
                    self.send(Command::Unmount);
                } else {
                    self.open_modal(Modal::Mount, "", "/mnt/tdrive", cx);
                }
            }
            ToolAct::Limit => self.open_modal(Modal::UploadLimit, "", "3", cx),
            ToolAct::Optimize => self.send(Command::OptimizeDrive),
            ToolAct::Lock => {
                if self.vault_configured && self.vault_unlocked {
                    self.send(Command::Lock);
                } else if self.vault_configured {
                    self.modal = Some(Modal::Unlock {
                        hint: self.vault_hint.clone(),
                    });
                } else {
                    self.modal = Some(Modal::Encrypt);
                }
            }
        }
        cx.notify();
    }

    fn fire_menu_act(&mut self, act: MenuAct, cx: &mut Context<Self>) {
        // Item clicks stop propagation, so closing popups is our job.
        self.context_menu = None;
        self.tools_menu = None;
        self.empty_menu = None;
        match act {
            MenuAct::Sel(a) => self.fire_sel_act(a, cx),
            MenuAct::Tool(t) => self.fire_tool(t, cx),
            MenuAct::Area(a) => match a {
                AreaAct::Upload => self.pick_upload(cx),
                AreaAct::NewFolder => {
                    self.open_modal(Modal::NewFolder, "", "folder name", cx)
                }
                AreaAct::Paste => {
                    let dest = self.cwd.clone();
                    self.paste_clip(dest, cx);
                }
            },
        }
        cx.notify();
    }

    /// Floating menu shell shared by the right-click menu and the Tools
    /// dropdown — edge-clamped inside the window. `align_right` pins the
    /// menu's right edge to the click (dropdowns); `dy` nudges it below
    /// its button.
    fn popup_menu(
        &self,
        pos: Point<Pixels>,
        items: Vec<(&'static str, String, Tone, MenuAct)>,
        align_right: bool,
        dy: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let vw = f32::from(window.viewport_size().width);
        let vh = f32::from(window.viewport_size().height);
        let (w, row_h) = (200f32, 30.5f32);
        let h = items.len() as f32 * row_h + 8.;
        let raw_x = if align_right {
            f32::from(pos.x) - w + 16.
        } else {
            f32::from(pos.x)
        };
        let x = px(raw_x.max(8.).min(vw - w - 8.));
        let y = px((f32::from(pos.y) + dy).max(8.).min(vh - h - 8.));
        let mut menu = div()
            .absolute()
            .left(x)
            .top(y)
            .w(px(w))
            .flex()
            .flex_col()
            .py_1()
            .bg(theme().panel)
            .border_1()
            .border_color(theme().border)
            .rounded_md()
            .shadow_lg();
        for (icon_name, label, tone, act) in items {
            let color = match tone {
                Tone::Danger => theme().danger,
                _ => theme().text,
            };
            menu = menu.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .h(px(row_h))
                    .text_sm()
                    .text_color(color)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme().panel2))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.fire_menu_act(act, cx)
                        }),
                    )
                    .child(icon_sized(icon_name, 13., color))
                    .child(label),
            );
        }
        menu.into_any_element()
    }

    /// Click semantics: ctrl toggles, shift ranges from the anchor,
    /// plain selects one. Also arms drag-select on plain clicks.
    fn apply_click(&mut self, id: String, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        if ev.modifiers.control {
            if let Some(i) = self.selected.iter().position(|s| *s == id) {
                self.selected.remove(i);
            } else {
                self.selected.push(id.clone());
            }
            self.anchor = Some(id);
        } else if ev.modifiers.shift {
            if let Some(anchor) = self.anchor.clone() {
                if let Some(range) = self.range_to(&anchor, &id, cx) {
                    self.selected = range;
                }
            }
        } else {
            self.selected = vec![id.clone()];
            self.anchor = Some(id.clone());
            if crate::wire::is_file_id(&id) {
                self.send(Command::FetchThumbnail { file_id: id });
            }
        }
        self.drag_anchor = self.anchor.clone();
    }

    /// Ids between `from` and `to` in display order — none if either
    /// isn't visible.
    fn range_to(&self, from: &str, to: &str, cx: &App) -> Option<Vec<String>> {
        let ids = self.visible_ids(cx);
        let (a, b) = (
            ids.iter().position(|i| i == from)?,
            ids.iter().position(|i| i == to)?,
        );
        Some(ids[a.min(b)..=a.max(b)].to_vec())
    }

    /// Extend the selection while left-dragging across a row/cell —
    /// ranges from where the drag started.
    fn drag_select_to(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(from) = self.drag_anchor.clone() else {
            return;
        };
        if let Some(range) = self.range_to(&from, id, cx) {
            if range != self.selected {
                self.selected = range;
                cx.notify();
            }
        }
    }

    /// Legal move/drop target for `objs` — a folder (or root ""), never
    /// the object itself or inside its own subtree (cycle).
    fn can_drop_into(&self, target: &str, objs: &[String]) -> bool {
        if self.trash || (!target.is_empty() && !crate::wire::is_folder_id(target)) {
            return false;
        }
        objs.iter()
            .all(|o| o != target && !self.namespace.subtree_contains(o, target))
    }

    /// Paste the clipboard under `dest` — cut moves, copy duplicates.
    /// Moves that would make a cycle are skipped per object.
    fn paste_clip(&mut self, dest: String, cx: &mut Context<Self>) {
        let Some((objs, cut)) = self
            .clip
            .as_ref()
            .map(|c| (c.objs.clone(), c.cut))
        else {
            return;
        };
        let mut pasted = Vec::new();
        for obj in &objs {
            if cut {
                if !self.can_drop_into(&dest, std::slice::from_ref(obj)) {
                    continue;
                }
                self.send(Command::Move {
                    obj: obj.clone(),
                    parent: dest.clone(),
                });
            } else {
                self.send(Command::Copy {
                    obj: obj.clone(),
                    parent: dest.clone(),
                });
            }
            pasted.push(obj.clone());
        }
        let n = pasted.len();
        self.mark_pending(
            &pasted,
            if cut { PendKind::Move } else { PendKind::Timed },
            &dest,
            cx,
        );
        if cut {
            self.clip = None;
        }
        self.status = format!(
            "{} {n} item{}",
            if cut { "moving" } else { "copying" },
            if n == 1 { "" } else { "s" },
        );
    }

    /// Objects just sent through the worker get a pulsing ghost look
    /// until the namespace proves the op landed (or a deadline hits,
    /// for anything that stalls on the network).
    pub(super) fn mark_pending(&mut self, objs: &[String], kind: PendKind, dest: &str, cx: &mut Context<Self>) {
        let ttl = std::time::Duration::from_secs(match kind {
            PendKind::Timed => 8,
            _ => 15,
        });
        let deadline = std::time::Instant::now() + ttl;
        for o in objs {
            self.pending_ops.insert(
                o.clone(),
                PendingOp {
                    dest: dest.to_string(),
                    kind,
                    deadline,
                },
            );
        }
        // If no namespace event ever arrives, clear ghosts on schedule
        // instead of leaving dimmed rows behind.
        let t = cx.background_executor().timer(ttl);
        cx.spawn(async move |this, cx| {
            t.await;
            let _ = this.update(cx, |this, cx| {
                let now = std::time::Instant::now();
                this.pending_ops.retain(|_, p| p.deadline > now);
                cx.notify();
            });
        })
        .detach();
    }

    /// Ids the rubber band currently covers — list rows are full-width
    /// so only the band's vertical span matters there; grid cells
    /// check both axes.
    fn rubber_hits(&self) -> Vec<String> {
        let Some(r) = &self.rubber else { return vec![] };
        let (top, bot) = (
            f32::from(r.start.y.min(r.current.y)),
            f32::from(r.start.y.max(r.current.y)),
        );
        let ids = &self.entries_cache;
        if ids.is_empty() {
            return vec![];
        }
        if self.grid && !self.trash {
            let (left, right) = (
                f32::from(r.start.x.min(r.current.x)),
                f32::from(r.start.x.max(r.current.x)),
            );
            let cols = self.grid_cols.max(1);
            (0..ids.len())
                .filter(|&i| {
                    let (row, col) = (i / cols, i % cols);
                    let cy0 = row as f32 * GRID_ROW_H + GRID_PAD;
                    let cx0 = GRID_PAD + col as f32 * (CELL_W + GRID_GAP);
                    cy0 + CELL_H > top && cy0 < bot && cx0 + CELL_W > left && cx0 < right
                })
                .map(|i| ids[i].id().to_string())
                .collect()
        } else {
            let n = ids.len();
            let i0 = (top / ROW_H).floor().max(0.) as usize;
            let i1 = ((bot / ROW_H).floor().max(0.) as usize).min(n - 1);
            if i0 > i1 {
                return vec![];
            }
            ids[i0.min(n - 1)..=i1]
                .iter()
                .map(|e| e.id().to_string())
                .collect()
        }
    }

    /// Live-update the selection to whatever the band covers — union
    /// with the pre-drag selection when ctrl was held at press.
    fn rubber_apply(&mut self, cx: &mut Context<Self>) {
        let hits = self.rubber_hits();
        if let Some(r) = &self.rubber {
            if r.additive {
                let mut sel = r.base.clone();
                for h in hits {
                    if !sel.contains(&h) {
                        sel.push(h);
                    }
                }
                self.selected = sel;
            } else {
                self.selected = hits;
            }
        }
        self.anchor = self.selected.first().cloned();
        cx.notify();
    }

    /// Window-space point → file-list content space (for the band and
    /// hit-testing).
    fn content_pos(&self, pos: Point<Pixels>) -> Point<Pixels> {
        let origin = *self.file_origin.borrow();
        let off = self.scroll_handle.offset();
        point(
            pos.x - origin.x - off.x,
            pos.y - origin.y - off.y,
        )
    }

    /// Row/cell hover while the left button is held — once the armed
    /// press travels past the threshold it becomes a drag-move and the
    /// hovered folder lights up as a drop target; otherwise the hover
    /// keeps range-selecting. A rubber-band press owns the pointer.
    fn hover_during_drag(&mut self, id: &str, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        if self.rubber.is_some() {
            return;
        }
        if let Some((_, start)) = self.pending_move.clone() {
            if !self.drag_moving
                && (f32::from(ev.position.x - start.x).abs()
                    + f32::from(ev.position.y - start.y).abs())
                    > DRAG_ARM
            {
                self.drag_moving = true;
            }
        }
        if self.drag_moving {
            let target = if self.can_drop_into(id, &self.selected) {
                Some(id.to_string())
            } else {
                None
            };
            if self.drop_hover != target {
                self.drop_hover = target;
                cx.notify();
            }
        } else {
            self.drag_select_to(id, cx);
        }
    }

    /// Left release on a row/cell during a drag-move — ctrl held means
    /// copy, otherwise move. The folder under the pointer is the dest.
    fn drop_on(&mut self, id: &str, ev: &MouseUpEvent, cx: &mut Context<Self>) {
        if !self.drag_moving || !self.can_drop_into(id, &self.selected) {
            return;
        }
        let objs = self.selected.clone();
        if objs.is_empty() {
            return;
        }
        let copy = ev.modifiers.control;
        for obj in &objs {
            self.send(if copy {
                Command::Copy {
                    obj: obj.clone(),
                    parent: id.to_string(),
                }
            } else {
                Command::Move {
                    obj: obj.clone(),
                    parent: id.to_string(),
                }
            });
        }
        self.mark_pending(
            &objs,
            if copy { PendKind::Timed } else { PendKind::Move },
            id,
            cx,
        );
        self.status = format!(
            "{} {} item{}",
            if copy { "copying" } else { "moving" },
            objs.len(),
            if objs.len() == 1 { "" } else { "s" },
        );
        self.selected.clear();
        self.drag_moving = false;
        self.drop_hover = None;
        self.pending_move = None;
    }

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

        // Action listeners — the rarer drive-level ops live behind
        // the Tools menu (fire_tool).
        let upload_btn = cx.listener(|this, _: &MouseDownEvent, _, cx| {
            this.context_menu = None;
            this.tools_menu = None;
            this.empty_menu = None;
            this.pick_upload(cx);
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
            this.selected.clear();
            this.tools_menu = None;
            this.context_menu = None;
            this.empty_menu = None;
            cx.notify();
        });
        // stop_propagation or the window-level dismiss would close the
        // menu on the same click that opened it.
        let tools_btn = cx.listener(|this, ev: &MouseDownEvent, _, cx| {
            this.tools_menu = if this.tools_menu.is_some() {
                None
            } else {
                Some(ev.position)
            };
            this.context_menu = None;
            cx.stop_propagation();
            cx.notify();
        });

        let et = cx.listener(|this, _: &MouseDownEvent, _, cx| {
            this.send(Command::EmptyTrash);
            cx.notify();
        });
        let ad = cx.listener(|this, _: &MouseDownEvent, _, cx| {
            this.modal = Some(Modal::Adopt);
            cx.notify();
        });
        let paste_here = cx.listener(|this, _: &MouseDownEvent, _, cx| {
            let dest = this.cwd.clone();
            this.paste_clip(dest, cx);
        });

        // Path bar — the drive name is the first crumb, the deepest
        // folder is emphasized. Trash and search get a terminal crumb
        // so the bar always answers "where am I".
        let search_q = self.f_search.read(cx).text().trim().to_string();
        let at_root = !self.trash && search_q.is_empty() && self.cwd.is_empty();
        let mut crumbs = div()
            .flex()
            .items_center()
            .gap_1()
            .text_sm()
            .child(icon_sized("folder", 13., theme().dim))
            .child(
            div()
                .id("crumb-drive")
                .cursor_pointer()
                .font_weight(if at_root {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                })
                .text_color(if at_root { theme().text } else { theme().dim })
                .hover(|s| s.text_color(theme().accent))
                .child(channel_name.clone())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.trash = false;
                        this.set_cwd(String::new());
                        cx.notify();
                    }),
                ),
        );
        let tail: Vec<(String, String)> = if self.trash {
            vec![(String::new(), "Trash".into())]
        } else if !search_q.is_empty() {
            vec![(String::new(), format!("\"{search_q}\""))]
        } else {
            self.namespace.folder_path(&self.cwd)
        };
        for (i, (id, name)) in tail.iter().enumerate() {
            let last = i == tail.len() - 1;
            crumbs = crumbs.child(icon_sized("chevron_right", 10., theme().file_icon));
            if id.is_empty() || last {
                // Terminal crumb — current folder or mode label.
                crumbs = crumbs.child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme().text)
                        .child(name.clone()),
                );
            } else {
                crumbs = crumbs.child(
                    div()
                        .id(SharedString::from(format!("crumb-{id}")))
                        .cursor_pointer()
                        .text_color(theme().dim)
                        .hover(|s| s.text_color(theme().accent))
                        .child(name.clone())
                        .on_mouse_down(
                            MouseButton::Left,
                            {
                                let id = id.clone();
                                cx.listener(move |this, _, _, cx| {
                                    this.set_cwd(id.clone());
                                    cx.notify();
                                })
                            },
                        ),
                );
            }
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
            .track_scroll(&self.scroll_handle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                    // Rows stop propagation, so a press reaching the
                    // container is on empty space — arm a rubber band.
                    this.context_menu = None;
                    this.tools_menu = None;
                    this.empty_menu = None;
                    this.pending_move = None;
                    this.drag_anchor = None;
                    let at = this.content_pos(ev.position);
                    this.rubber = Some(Rubber {
                        start: at,
                        current: at,
                        additive: ev.modifiers.control,
                        base: this.selected.clone(),
                    });
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                if this.rubber.is_none() {
                    return;
                }
                if !matches!(ev.pressed_button, Some(MouseButton::Left)) {
                    this.rubber = None;
                    cx.notify();
                    return;
                }
                let at = this.content_pos(ev.position);
                let moved = if let Some(r) = &mut this.rubber {
                    if r.current != at {
                        r.current = at;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if moved {
                    this.rubber_apply(cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                    // Rows stop propagation on right-press, so this only
                    // sees empty space — open the area menu. Nothing to
                    // create into trash.
                    if this.trash {
                        return;
                    }
                    this.empty_menu = Some(ev.position);
                    this.context_menu = None;
                    this.tools_menu = None;
                    cx.notify();
                }),
            );
        if self.entries_cache.is_empty() {
            body = body.child(
                div()
                    .py_16()
                    .flex()
                    .justify_center()
                    .text_sm()
                    .text_color(theme().dim)
                    .child(if self.trash {
                        "trash is empty"
                    } else if !self.f_search.read(cx).text().trim().is_empty() {
                        "no matches"
                    } else {
                        "empty folder — drop files or folders to upload"
                    }),
            );
        } else if self.grid && !self.trash {
            self.grid_cols = ((f32::from(window.viewport_size().width) / 110.) as usize).max(1);
            for chunk in self.entries_cache.clone().chunks(self.grid_cols) {
                let mut r = div().flex().gap_2().p_1().h(px(GRID_ROW_H));
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
        // Rubber band — plain absolute child so it scrolls with content.
        if let Some(r) = &self.rubber {
            let (x0, y0) = (r.start.x.min(r.current.x), r.start.y.min(r.current.y));
            let (x1, y1) = (r.start.x.max(r.current.x), r.start.y.max(r.current.y));
            body = body.child(
                div()
                    .absolute()
                    .left(x0)
                    .top(y0)
                    .w(x1 - x0)
                    .h(y1 - y0)
                    .border_1()
                    .border_color(theme().accent)
                    .bg(theme().accent_soft),
            );
        }
        // The working bar — path on the left; the right side is
        // contextual: a compact selection set when something's
        // selected, otherwise the create/ops set plus Sync/Tools.
        let mut actions = div()
            .flex()
            .items_center()
            .gap_2()
            .h(px(CTX_H))
            .w_full()
            .px_3()
            .border_b_1()
            .border_color(theme().border)
            .child(crumbs)
            .child(div().flex_1());
        if !self.selected.is_empty() {
            if self.selected.len() == 1 {
                let sel = &self.selected[0];
                if let Some(path) = self.thumbnails.get(sel) {
                    actions = actions.child(img(path.clone()).w(px(24.)).h(px(24.)));
                }
            }
            actions = actions.child(
                div()
                    .max_w(px(180.))
                    .overflow_hidden()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme().text)
                    .child(self.sel_label()),
            );
            for act in self.bar_acts() {
                let (icon, label, tone) = act.meta();
                actions = actions.child(tool_button(
                    icon,
                    label,
                    tone,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        this.fire_sel_act(act, cx)
                    }),
                ));
            }
        } else {
            actions = actions.child(
                div()
                    .text_xs()
                    .text_color(theme().dim)
                    .child(format!("{} items", self.entries_cache.len())),
            );
            if self.trash {
                actions = actions.child(tool_button("trash", "Empty trash", Tone::Danger, et));
            } else {
                if self.clip.is_some() {
                    actions =
                        actions.child(tool_button("paste", "Paste", Tone::Ghost, paste_here));
                }
                if !self.namespace.unadopted.is_empty() {
                    let n = self.namespace.unadopted.len();
                    actions = actions.child(tool_button(
                        "inbox",
                        &format!("Unadopted ({n})"),
                        Tone::Ghost,
                        ad,
                    ));
                }
                actions = actions
                    .child(tool_button("upload", "Upload", Tone::Primary, upload_btn))
                    .child(tool_button("folder_plus", "New folder", Tone::Ghost, new_folder));
            }
            actions = actions
                .child(tool_button("refresh", "Sync", Tone::Ghost, refresh))
                .child(tool_button("sliders", "Tools", Tone::Ghost, tools_btn));
        }

        // Popup menus — the right-click menu tracks the cursor; the
        // Tools dropdown right-aligns to its button. Window coords work
        // because this subtree fills the whole window on the Drive
        // screen.
        let context = if let Some(pos) = self.context_menu {
            let items = self
                .sel_acts()
                .into_iter()
                .map(|a| {
                    let (icon, label, tone) = a.meta();
                    (icon, label.to_string(), tone, MenuAct::Sel(a))
                })
                .collect();
            self.popup_menu(pos, items, false, 0., window, cx)
        } else if let Some(pos) = self.empty_menu {
            let mut items: Vec<(&'static str, String, Tone, MenuAct)> = vec![
                ("upload", "Upload".into(), Tone::Ghost, MenuAct::Area(AreaAct::Upload)),
                ("folder_plus", "New folder".into(), Tone::Ghost, MenuAct::Area(AreaAct::NewFolder)),
            ];
            if self.clip.is_some() {
                items.push(("paste", "Paste".into(), Tone::Ghost, MenuAct::Area(AreaAct::Paste)));
            }
            self.popup_menu(pos, items, false, 0., window, cx)
        } else {
            div().into_any_element()
        };
        let tools = if let Some(pos) = self.tools_menu {
            let mut items: Vec<(&'static str, String, Tone, MenuAct)> = vec![
                ("camera", "Photo backup".into(), Tone::Ghost, MenuAct::Tool(ToolAct::Backup)),
                ("eye", "Watch folder".into(), Tone::Ghost, MenuAct::Tool(ToolAct::Watch)),
                (
                    "hard_drive",
                    (if self.mount_dir.is_some() { "Unmount" } else { "Mount" }).into(),
                    Tone::Ghost,
                    MenuAct::Tool(ToolAct::Mount),
                ),
                ("sliders", "Upload limit".into(), Tone::Ghost, MenuAct::Tool(ToolAct::Limit)),
                ("sparkles", "Optimize drive".into(), Tone::Ghost, MenuAct::Tool(ToolAct::Optimize)),
            ];
            if self.channel_kind.is_personal() {
                let (icon, label) = if self.vault_configured {
                    if self.vault_unlocked {
                        ("unlock", "Lock drive")
                    } else {
                        ("lock", "Unlock drive")
                    }
                } else {
                    ("lock", "Encrypt drive")
                };
                items.push((icon, label.into(), Tone::Ghost, MenuAct::Tool(ToolAct::Lock)));
            }
            self.popup_menu(pos, items, true, 6., window, cx)
        } else {
            div().into_any_element()
        };

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
                    .child(div().w(px(36.)).h(px(3.)).rounded_full().bg(theme().border)),
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
                        .text_color(theme().dim)
                        .child(format!("Transfers · {active_count} active · {total} total")),
                )
                .child(div().flex_1())
                .child(if active_count > 0 {
                    // Pause-all flips to resume-all once nothing's moving.
                    let any_active = self
                        .transfers
                        .values()
                        .any(|t| t.state == TransferState::Active);
                    let ids: Vec<String> = self
                        .transfers
                        .values()
                        .filter(|t| {
                            t.state
                                == if any_active {
                                    TransferState::Active
                                } else {
                                    TransferState::Paused
                                }
                        })
                        .map(|t| t.id.clone())
                        .collect();
                    icon_button(
                        "transfers-pp".to_string(),
                        if any_active { "pause" } else { "play" },
                        theme().dim,
                        theme().panel2,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            for id in &ids {
                                this.send(if any_active {
                                    Command::Pause { id: id.clone() }
                                } else {
                                    Command::Resume { id: id.clone() }
                                });
                            }
                            cx.notify();
                        }),
                    )
                    .into_any_element()
                } else {
                    div().into_any_element()
                })
                .child(if total > limit {
                    div()
                        .id("transfers-toggle")
                        .px_2()
                        .rounded_md()
                        .text_xs()
                        .text_color(theme().dim)
                        .hover(|s| s.bg(theme().panel2).text_color(theme().text))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                this.transfers_expanded = !this.transfers_expanded;
                                cx.notify();
                            }),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(icon_sized(
                                    if self.transfers_expanded {
                                        "chevron_down"
                                    } else {
                                        "chevron_right"
                                    },
                                    10.,
                                    theme().dim,
                                ))
                                .child(if self.transfers_expanded {
                                    "collapse"
                                } else {
                                    "show all"
                                }),
                        )
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
            .bg(theme().bg)
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
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    if this.context_menu.is_some()
                        || this.tools_menu.is_some()
                        || this.empty_menu.is_some()
                    {
                        this.context_menu = None;
                        this.tools_menu = None;
                        this.empty_menu = None;
                        cx.notify();
                    }
                }),
            )
            .can_drop(|val, _window, _cx| val.is::<ExternalPaths>())
            .drag_over(|style, _paths: &ExternalPaths, _window, _cx| {
                style.bg(theme().accent.opacity(0.08))
            })
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _window, cx| {
                this.external_drop(paths.paths().to_vec(), cx);
            }))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                this.cursor = ev.position;
                this.drag_copy = ev.modifiers.control;
                if ev.pressed_button.is_none() {
                    this.drag_anchor = None;
                    this.pending_move = None;
                    this.drag_moving = false;
                    this.drop_hover = None;
                    this.rubber = None;
                } else if let Some((_, start)) = &this.pending_move {
                    // Rows/cells only see hovers over themselves — the
                    // threshold also needs to trip over empty space or a
                    // release there would collapse the selection.
                    if !this.drag_moving
                        && (f32::from(ev.position.x - start.x).abs()
                            + f32::from(ev.position.y - start.y).abs())
                            > DRAG_ARM
                    {
                        this.drag_moving = true;
                        cx.notify();
                    }
                }
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
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| {
                    if let Some(r) = this.rubber.take() {
                        let grown = (f32::from(r.current.x - r.start.x)).abs()
                            + (f32::from(r.current.y - r.start.y)).abs()
                            > RUBBER_CLICK;
                        if !grown {
                            // Never became a band — a click on empty
                            // space, which deselects (ctrl keeps base).
                            this.selected = if r.additive { r.base } else { vec![] };
                            this.anchor = this.selected.first().cloned();
                        }
                        cx.notify();
                    } else if this.drag_moving {
                        // Released over empty space — cancel the drag.
                        this.drag_moving = false;
                        this.drop_hover = None;
                        this.pending_move = None;
                        cx.notify();
                    } else if let Some((id, _)) = this.pending_move.take() {
                        // Press on a selected item that never became a
                        // drag — plain click, collapse the selection.
                        this.selected = vec![id.clone()];
                        this.anchor = Some(id);
                        cx.notify();
                    }
                }),
            )
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
                if this.context_menu.is_some()
                    || this.tools_menu.is_some()
                    || this.empty_menu.is_some()
                {
                    if e.keystroke.key == "escape" {
                        this.context_menu = None;
                        this.tools_menu = None;
                        this.empty_menu = None;
                        cx.notify();
                    }
                    return;
                }
                let ids = this.visible_ids(cx);
                if e.keystroke.modifiers.control && !this.trash {
                    match e.keystroke.key.as_str() {
                        "a" => {
                            this.selected = ids;
                            this.anchor = this.selected.first().cloned();
                        }
                        "c" | "x" => {
                            if this.selected.is_empty() {
                                return;
                            }
                            let cut = e.keystroke.key == "x";
                            let n = this.selected.len();
                            this.clip = Some(Clip {
                                objs: this.selected.clone(),
                                cut,
                            });
                            this.status = format!(
                                "{n} item{} {}",
                                if n == 1 { "" } else { "s" },
                                if cut { "cut" } else { "copied" },
                            );
                        }
                        "v" => {
                            if this.clip.is_some() {
                                this.paste_clip(this.cwd.clone(), cx);
                            }
                        }
                        _ => return,
                    }
                    cx.notify();
                    return;
                }
                match e.keystroke.key.as_str() {
                    "up" | "down" => {
                        if ids.is_empty() {
                            return;
                        }
                        let idx = ids
                            .iter()
                            .position(|i| this.selected.len() == 1 && this.selected[0] == *i)
                            .map(|i| {
                                if e.keystroke.key == "up" {
                                    i.saturating_sub(1)
                                } else {
                                    (i + 1).min(ids.len() - 1)
                                }
                            })
                            .unwrap_or(0);
                        this.selected = vec![ids[idx].clone()];
                        this.anchor = Some(ids[idx].clone());
                        let row = if this.grid && !this.trash {
                            idx / this.grid_cols.max(1)
                        } else {
                            idx
                        };
                        this.scroll_handle.scroll_to_top_of_item(row);
                    }
                    "enter" => {
                        if this.selected.len() != 1 {
                            return;
                        }
                        let sel = this.selected[0].clone();
                        if crate::wire::is_folder_id(&sel) {
                            this.set_cwd(sel);
                        } else if let Some(f) = this.namespace.files.get(&sel).cloned() {
                            if !this.trash {
                                this.pick_download_dest(f.id, f.name, cx);
                            }
                        }
                    }
                    "escape" => {
                        this.selected.clear();
                        this.anchor = None;
                        this.rubber = None;
                        this.drag_moving = false;
                        this.pending_move = None;
                        this.drop_hover = None;
                    }
                    "backspace" => {
                        let parent = this
                            .namespace
                            .folders
                            .get(&this.cwd)
                            .map(|d| d.parent.clone())
                            .unwrap_or_default();
                        this.cwd = parent;
                        this.selected.clear();
                    }
                    "delete" => {
                        if this.selected.is_empty() {
                            return;
                        }
                        this.modal = Some(Modal::Delete {
                            objs: this.selected.clone(),
                            name: this.sel_label(),
                        });
                    }
                    _ => return,
                }
                cx.notify();
            }))
            // Titlebar — app wordmark left; search + session/view
            // controls right. Location lives in the context bar now.
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .w_full()
                    .h(px(BAR_H))
                    .px_3()
                    .bg(theme().panel)
                    .border_b_1()
                    .border_color(theme().border)
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme().text)
                            .child("TDrive"),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(icon_sized("search", 13., theme().dim))
                            .child(div().w(px(200.)).child(self.f_search.clone())),
                    )
                    .child(if !self.trash {
                        let gt = cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            this.grid = !this.grid;
                            if let Some(cfg) = &mut this.config {
                                cfg.grid_view = this.grid;
                                if let Err(e) = cfg.save() {
                                    this.status = format!("config save failed: {e}");
                                }
                            }
                            cx.notify();
                        });
                        tool_button(
                            if self.grid { "list" } else { "grid" },
                            if self.grid { "List" } else { "Grid" },
                            Tone::Ghost,
                            gt,
                        )
                        .into_any_element()
                    } else {
                        div().into_any_element()
                    })
                    .child(tool_button("drives", "Drives", Tone::Ghost, back))
                    .child(tool_button(
                        if self.trash { "folder" } else { "trash" },
                        if self.trash { "Files" } else { "Trash" },
                        Tone::Ghost,
                        trash_toggle,
                    ))
                    .child(tool_button("log_out", "Sign out", Tone::Ghost, sign_out)),
            )
            .child(actions)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .child(
                        // Stashes the file area's viewport origin each
                        // frame — mouse positions minus it land in
                        // content space for the rubber band.
                        div().absolute().inset_0().child(canvas(
                            {
                                let slot = self.file_origin.clone();
                                move |bounds, _, _| {
                                    *slot.borrow_mut() = bounds.origin;
                                }
                            },
                            |_, _, _, _| {},
                        )),
                    )
                    .child(body),
            )
            .child(
                div()
                    .border_t_1()
                    .border_color(theme().border)
                    .bg(theme().panel)
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
                                .border_color(theme().border)
                                .child(
                                    div()
                                        .w(px(16.))
                                        .text_sm()
                                        .text_color(theme().accent)
                                        .child(FRAMES[(self.activity_frame % 10) as usize]),
                                )
                                .child(div().w(px(90.)).h(px(3.)).rounded_sm().bg(theme().accent))
                                .child(div().text_xs().text_color(theme().dim).child(label))
                                .into_any_element()
                        }
                    })
                    .child(
                        div()
                            .px_3()
                            .py_1()
                            .text_xs()
                            .text_color(theme().dim)
                            .child(self.status.clone()),
                    ),
            )
            .child(self.render_modal(window, cx))
            .child(self.render_viewer(cx))
            .child(context)
            .child(tools)
            .child(if self.drag_moving && !self.selected.is_empty() {
                // Cursor-following chip — the "is it dragging?" signal.
                let n = self.selected.len();
                let label = format!(
                    "{} {n} item{}",
                    if self.drag_copy { "copy" } else { "move" },
                    if n == 1 { "" } else { "s" },
                );
                deferred(
                    div()
                        .absolute()
                        .left(self.cursor.x + px(14.))
                        .top(self.cursor.y + px(14.))
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(theme().accent)
                        .text_xs()
                        .text_color(theme().on_accent)
                        .shadow_lg()
                        .child(label),
                )
                .into_any_element()
            } else {
                div().into_any_element()
            })
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
        let selected = self.selected.contains(&id);
        let drop_target = self.drop_hover.as_deref() == Some(id.as_str());
        let busy = self.pending_ops.contains_key(&id);
        let drag_src = self.drag_moving && selected;
        let click_id = id.clone();
        let rc_id = id.clone();
        let hover_id = id.clone();
        let drop_id = id.clone();
        let thumb = self.thumbnails.get(&id).cloned();
        let cell = div()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .w(px(CELL_W))
            .h(px(CELL_H))
            .p_2()
            .rounded_lg()
            // Source cells go translucent while they're being dragged.
            .opacity(if drag_src { 0.55 } else { 1. })
            .bg(if selected || drop_target {
                theme().accent_soft
            } else {
                transparent_black()
            })
            // A drop target stays lit — the generic hover tint would
            // paint over it.
            .hover(move |s| {
                if drop_target {
                    s.bg(theme().accent_soft)
                } else {
                    s.bg(theme().panel)
                }
            })
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    this.drive_focus.focus(window);
                    this.context_menu = None;
                    this.tools_menu = None;
                    this.empty_menu = None;
                    if ev.click_count == 2 && is_folder {
                        this.set_cwd(click_id.clone());
                    } else if !this.trash
                        && !ev.modifiers.control
                        && !ev.modifiers.shift
                        && this.selected.contains(&click_id)
                    {
                        // Plain press on a selected item — arm a
                        // drag-move; releasing without a drag collapses
                        // the selection to it.
                        this.pending_move = Some((click_id.clone(), ev.position));
                        this.drag_anchor = None;
                    } else {
                        this.apply_click(click_id.clone(), ev, cx);
                    }
                    cx.notify();
                    // A cell press must never reach the scroll
                    // container's rubber-band arm.
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    if !this.selected.contains(&rc_id) {
                        this.selected = vec![rc_id.clone()];
                        this.anchor = Some(rc_id.clone());
                    }
                    this.context_menu = Some(ev.position);
                    this.tools_menu = None;
                    this.empty_menu = None;
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                if matches!(ev.pressed_button, Some(MouseButton::Left)) {
                    this.hover_during_drag(&hover_id, ev, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseUpEvent, _, cx| {
                    this.drop_on(&drop_id, ev, cx);
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
                                folder_bg(
                                    self.styles.get(&id).copied(),
                                    theme().accent_soft,
                                )
                            } else {
                                theme().panel2
                            })
                            .child(if is_folder {
                                folder_icon(self.styles.get(&id).copied(), 30.)
                                    .into_any_element()
                            } else {
                                entry_icon(false, &name, 30.).into_any_element()
                            })
                            .into_any_element()
                    }),
            )
            .child(
                div()
                    .w_full()
                    .text_xs()
                    .line_height(px(16.))
                    .text_color(theme().text)
                    .text_center()
                    .whitespace_normal()
                    .line_clamp(2)
                    .child(name),
            );
        if busy {
            cell.with_animation(
                SharedString::from(format!("busy-{id}")),
                Animation::new(std::time::Duration::from_millis(900)).repeat(),
                |el, d| el.opacity(0.35 + 0.3 * (1. - (2. * d - 1.).abs())),
            )
            .into_any_element()
        } else {
            cell.into_any_element()
        }
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
        let selected = self.selected.contains(&id);
        let drop_target = self.drop_hover.as_deref() == Some(id.as_str());
        let busy = self.pending_ops.contains_key(&id);
        let drag_src = self.drag_moving && selected;
        let row_id = format!("row-{id}");
        let id_click = id.clone();
        let rc_id = id.clone();
        let hover_id = id.clone();
        let drop_id = id.clone();

        let row = div()
            .id(SharedString::from(row_id))
            .flex()
            .items_center()
            .gap_3()
            .w_full()
            .px_4()
            .h(px(ROW_H))
            // Source rows go translucent while they're being dragged.
            .opacity(if drag_src { 0.55 } else { 1. })
            .bg(if drop_target {
                theme().accent_soft
            } else if selected {
                theme().selected
            } else {
                transparent_black()
            })
            // A drop target stays lit — the generic hover tint would
            // paint over it.
            .hover(move |s| {
                if drop_target {
                    s.bg(theme().accent_soft)
                } else {
                    s.bg(theme().panel2)
                }
            })
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    this.drive_focus.focus(window);
                    this.context_menu = None;
                    this.tools_menu = None;
                    this.empty_menu = None;
                    if ev.click_count == 2 && is_folder {
                        this.set_cwd(id_click.clone());
                    } else if ev.click_count == 2 {
                        this.open_viewer(&id_click, cx);
                    } else if !this.trash
                        && !ev.modifiers.control
                        && !ev.modifiers.shift
                        && this.selected.contains(&id_click)
                    {
                        // Plain press on a selected item — arm a
                        // drag-move; releasing without a drag collapses
                        // the selection to it.
                        this.pending_move = Some((id_click.clone(), ev.position));
                        this.drag_anchor = None;
                    } else {
                        this.apply_click(id_click.clone(), ev, cx);
                    }
                    cx.notify();
                    // A row press must never reach the scroll
                    // container's rubber-band arm.
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    if !this.selected.contains(&rc_id) {
                        this.selected = vec![rc_id.clone()];
                        this.anchor = Some(rc_id.clone());
                    }
                    this.context_menu = Some(ev.position);
                    this.tools_menu = None;
                    this.empty_menu = None;
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                if matches!(ev.pressed_button, Some(MouseButton::Left)) {
                    this.hover_during_drag(&hover_id, ev, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseUpEvent, _, cx| {
                    this.drop_on(&drop_id, ev, cx);
                    cx.notify();
                }),
            )
            .child(
                div()
                    .w(px(22.))
                    .h(px(22.))
                    .rounded_md()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(if is_folder {
                        folder_bg(self.styles.get(&id).copied(), transparent_black())
                    } else {
                        transparent_black()
                    })
                    .child(if is_folder {
                        folder_icon(self.styles.get(&id).copied(), 15.).into_any_element()
                    } else {
                        entry_icon(false, &name, 15.).into_any_element()
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .text_sm()
                    .text_color(theme().text)
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
                    .text_color(theme().dim)
                    .flex()
                    .justify_end()
                    .child(meta),
            );
        if busy {
            // In-flight op — pulse the row as a ghost until the
            // namespace confirms it landed.
            row.with_animation(
                SharedString::from(format!("busy-{id}")),
                Animation::new(std::time::Duration::from_millis(900)).repeat(),
                |el, d| el.opacity(0.35 + 0.3 * (1. - (2. * d - 1.).abs())),
            )
            .into_any_element()
        } else {
            row.into_any_element()
        }
    }
}
