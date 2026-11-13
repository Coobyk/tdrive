//! GPUI front end — screens, file browser, transfers panel.

use crate::config::AppConfig;
use crate::input::TextField;
use crate::project::{Entry, Namespace};
use crate::tg::{self, ChannelInfo, Command, Event, TransferState, TransferUpdate};
use gpui::*;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

mod controls;
mod drive;
mod modals;
mod screens;
mod theme;
mod transfers;
mod viewer;

pub use controls::human_size;
pub(crate) use theme::{apply_appearance, theme};


#[derive(Debug, Clone, PartialEq)]
enum Screen {
    Setup,
    Connecting,
    AuthPhone,
    AuthCode,
    AuthPassword,
    Picker,
    Drive,
}

enum Modal {
    NewFolder,
    Rename {
        obj: String,
    },
    Move {
        objs: Vec<String>,
    },
    /// "Move to new folder" — mkdir under cwd, then move `objs` into it.
    MoveToNew {
        objs: Vec<String>,
    },
    Delete {
        objs: Vec<String>,
        name: String,
    },
    Unlock {
        hint: String,
    },
    Encrypt,
    Adopt,
    WatchBackup,
    Mount,
    /// Review picked upload items — removable list + files/folder/7z
    /// mode. The plan lives on `upload_plan`.
    UploadOpts,
    /// Max concurrent uploads.
    UploadLimit,
    /// Confirm deleting `dead` obsolete channel messages (scanned by
    /// OptimizeDrive).
    Optimize {
        dead: usize,
    },
    /// Folder cosmetics — color + badge picker for one folder id.
    Style {
        obj: String,
    },
}

/// Per-folder cosmetics kept locally (never in remote metadata, or
/// other TDrive clients would see unknown fields). 0 = unset.
#[derive(Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct FolderStyle {
    /// Index into FOLDER_COLORS +1, 0 = theme accent.
    color: u8,
    /// Index into FOLDER_BADGES +1, 0 = no badge.
    badge: u8,
}

/// Desktop notification through libnotify — best effort, silent if
/// notify-send isn't installed.
fn notify(text: &str) {
    let _ = std::process::Command::new("notify-send")
        .arg("TDrive")
        .arg(text)
        .spawn();
}

fn load_styles() -> std::collections::HashMap<String, FolderStyle> {
    crate::config::styles_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Internal clipboard — cut objects move on paste, copies duplicate.
struct Clip {
    objs: Vec<String>,
    cut: bool,
}

/// One picked path in the upload review — dir counts/bytes are walked
/// once at pick time so the list can show totals without rescanning.
struct UploadPick {
    path: PathBuf,
    name: String,
    dir: bool,
    files: u64,
    bytes: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum PlanMode {
    /// Items land in cwd as-is — dirs become same-named folders.
    Files,
    /// Everything nested under one new named folder.
    Folder,
    /// One .7z archive containing the picked items.
    Archive,
}

struct UploadPlan {
    items: Vec<UploadPick>,
    mode: PlanMode,
}

/// Rubber-band selection state — corners live in file-list content
/// space (viewport point minus scroll offset) so the band stays glued
/// to items while the list scrolls under the pointer.
struct Rubber {
    start: Point<Pixels>,
    current: Point<Pixels>,
    /// ctrl was held at press — union hits with `base`, not replace.
    additive: bool,
    base: Vec<String>,
}

/// Which async namespace op is in flight for an object.
#[derive(Clone, Copy, PartialEq)]
enum PendKind {
    /// Verified once the object's parent becomes `dest`.
    Move,
    /// Verified once the object leaves the namespace entirely.
    Delete,
    /// Cheap to verify? No (copy, restore) — clears on the next
    /// namespace refresh or the deadline.
    Timed,
}

struct PendingOp {
    dest: String,
    kind: PendKind,
    deadline: std::time::Instant,
}

/// In-app file viewer state.
struct Viewer {
    file_id: String,
    name: String,
    kind: ViewKind,
    path: Option<PathBuf>,
    failed: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum ViewKind {
    Image,
    Video,
    Other,
}

pub struct Root {
    tg: tokio::sync::mpsc::UnboundedSender<Command>,
    screen: Screen,
    status: String,
    config: Option<AppConfig>,

    f_api_id: Entity<TextField>,
    f_api_hash: Entity<TextField>,
    f_phone: Entity<TextField>,
    f_code: Entity<TextField>,
    f_password: Entity<TextField>,
    f_channel: Entity<TextField>,
    f_name: Entity<TextField>,

    channels: Vec<ChannelInfo>,
    channel: Option<ChannelInfo>,
    namespace: Arc<Namespace>,
    cwd: String,
    selected: Vec<String>,
    /// Range anchor for shift-click selection.
    anchor: Option<String>,
    /// Row id the current left-drag started on — drive-selects a range
    /// while the pointer moves across other rows.
    drag_anchor: Option<String>,
    /// Right-click menu position (window coords) while open.
    context_menu: Option<Point<Pixels>>,
    /// Tools dropdown position (window coords) while open.
    tools_menu: Option<Point<Pixels>>,
    /// Empty-space right-click menu (upload/new-folder/paste).
    empty_menu: Option<Point<Pixels>>,
    /// Internal clipboard for ctrl+c/x + Paste — cut pastes as moves.
    clip: Option<Clip>,
    /// Picked paths awaiting the upload-review modal's verdict.
    upload_plan: Option<UploadPlan>,
    /// Folder cosmetics by object id — local-only, persists to
    /// `folder_styles.json` under the data dir.
    styles: std::collections::HashMap<String, FolderStyle>,
    /// Left press on an already-selected row, armed to become a
    /// drag-move once the pointer travels past the threshold.
    pending_move: Option<(String, Point<Pixels>)>,
    /// Armed press has become a real drag — folders under the pointer
    /// light up as drop targets.
    drag_moving: bool,
    /// Folder the drag currently hovers when it's a legal drop target.
    drop_hover: Option<String>,
    /// Left press on empty file-area space, grown into a Finder-style
    /// rubber band; corners are in content space (scroll-adjusted).
    rubber: Option<Rubber>,
    /// Last cursor position in window coords — the drag-move chip
    /// follows it.
    cursor: Point<Pixels>,
    /// Ctrl held during a drag-move — chip reads "copy" instead.
    drag_copy: bool,
    /// Objects with an in-flight move/copy/delete — rows pulse dimmed
    /// until the namespace confirms or the deadline passes.
    pending_ops: std::collections::HashMap<String, PendingOp>,
    /// Viewport origin of the file-scroll area in window coords,
    /// refreshed every frame by a canvas in prepaint.
    file_origin: std::rc::Rc<std::cell::RefCell<Point<Pixels>>>,
    transfers: BTreeMap<String, TransferUpdate>,
    modal: Option<Modal>,
    signed_in_as: String,
    f_search: Entity<TextField>,
    f_invite: Entity<TextField>,
    /// Optional 7z password — shown in the review's archive mode.
    f_archpw: Entity<TextField>,
    vault_configured: bool,
    vault_unlocked: bool,
    vault_hint: String,
    trash: bool,
    drive_focus: FocusHandle,
    thumbnails: std::collections::HashMap<String, std::path::PathBuf>,
    thumb_requested: std::collections::HashSet<String>,
    grid: bool,
    channel_kind: crate::tg::DriveKind,
    mount_dir: Option<String>,
    /// Entries selected for the current view — cached so the virtualized
    /// list can index them without rebuilding per row.
    entries_cache: Vec<Entry>,
    grid_cols: usize,
    scroll_handle: ScrollHandle,
    /// View key (cwd/mode) — when it changes the scroll handle resets, so
    /// a deep scroll in one folder can't point past the next folder's
    /// end and show a blank list.
    last_view_key: String,
    /// Last rendered entry count — a count change snaps the scroll to the
    /// top so a new folder at index 0 can't hide above a stale offset.
    last_items_len: usize,
    viewer: Option<Viewer>,
    activity_frame: u8,
    transfers_expanded: bool,
    /// Height of the expanded transfers pane (draggable).
    transfers_height: f32,
    /// In-progress pane resize: (grab y, height at grab).
    transfers_drag: Option<(f32, f32)>,
    /// Auto-open the last drive only on the first channel list — a relist
    /// after "Drives" must not reopen it and bounce back.
    auto_open_done: bool,
    /// True while a sync is fetching drive contents — shows the footer
    /// activity bar.
    syncing: bool,
    /// Folder navigation history — M4/M5 (browser back/forward).
    nav_back: Vec<String>,
    nav_fwd: Vec<String>,
}

impl Root {
    pub fn new(cx: &mut Context<Self>, handle: tg::Handle) -> Self {
        let tg::Handle { tx, rx } = handle;

        let f_api_id = cx.new(|cx| TextField::new(cx, "f-api-id", "api_id"));
        let f_api_hash = cx.new(|cx| TextField::new(cx, "f-api-hash", "api_hash"));
        let f_phone = cx.new(|cx| TextField::new(cx, "f-phone", "+1 555 000 0000"));
        let f_code = cx.new(|cx| TextField::new(cx, "f-code", "login code"));
        let f_password = cx.new(|cx| TextField::new(cx, "f-password", "2FA password").masked());
        let f_channel = cx.new(|cx| TextField::new(cx, "f-channel", "drive name"));
        let f_name = cx.new(|cx| TextField::new(cx, "f-name", "name"));
        let f_search = cx.new(|cx| TextField::new(cx, "f-search", "search files…"));
        let f_invite = cx.new(|cx| TextField::new(cx, "f-invite", "t.me/+… invite link"));
        let f_archpw =
            cx.new(|cx| TextField::new(cx, "f-archpw", "archive password (optional)").masked());
        cx.observe(&f_search, |_, _, cx| cx.notify()).detach();

        let mut root = Self {
            tg: tx,
            screen: Screen::Connecting,
            status: "starting…".into(),
            config: None,
            f_api_id,
            f_api_hash,
            f_phone,
            f_code,
            f_password,
            f_channel,
            f_name,
            channels: Vec::new(),
            channel: None,
            namespace: Arc::new(Namespace::new()),
            cwd: String::new(),
            selected: Vec::new(),
            anchor: None,
            drag_anchor: None,
            context_menu: None,
            tools_menu: None,
            empty_menu: None,
            clip: None,
            upload_plan: None,
            styles: load_styles(),
            pending_move: None,
            drag_moving: false,
            drop_hover: None,
            rubber: None,
            cursor: point(px(0.), px(0.)),
            drag_copy: false,
            pending_ops: std::collections::HashMap::new(),
            file_origin: std::rc::Rc::new(std::cell::RefCell::new(point(px(0.), px(0.)))),
            transfers: BTreeMap::new(),
            modal: None,
            signed_in_as: String::new(),
            f_search,
            f_invite,
            f_archpw,
            vault_configured: false,
            vault_unlocked: false,
            vault_hint: String::new(),
            trash: false,
            drive_focus: cx.focus_handle(),
            thumbnails: Default::default(),
            thumb_requested: Default::default(),
            grid: false,
            channel_kind: crate::tg::DriveKind::Personal,
            mount_dir: None,
            entries_cache: Vec::new(),
            last_items_len: 0,
            grid_cols: 4,
            scroll_handle: ScrollHandle::new(),
            last_view_key: String::new(),
            viewer: None,
            activity_frame: 0u8,
            transfers_expanded: false,
            transfers_height: 168.,
            transfers_drag: None,
            auto_open_done: false,
            syncing: false,
            nav_back: Vec::new(),
            nav_fwd: Vec::new(),
        };

        match AppConfig::load() {
            Ok(Some(cfg)) => {
                root.grid = cfg.grid_view;
                root.config = Some(cfg.clone());
                let _ = root.tg.send(Command::Connect {
                    api_id: cfg.api_id,
                    api_hash: cfg.api_hash,
                });
                root.status = "connecting…".into();
            }
            _ => {
                root.screen = Screen::Setup;
                root.status = "Telegram API credentials required".into();
            }
        }

        cx.spawn(async move |this, cx| {
            while let Ok(ev) = rx.recv().await {
                if this
                    .update(cx, |this: &mut Self, cx| this.on_event(ev, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        // Activity ticker — advances the bottom-left spinner while any
        // sync, transfer or thumbnail fetch is in flight.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(120))
                    .await;
                if this
                    .update(cx, |this, cx| {
                        if this.activity_active() {
                            this.activity_frame = this.activity_frame.wrapping_add(1);
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        root
    }

    fn send(&self, cmd: Command) {
        let _ = self.tg.send(cmd);
    }

    fn on_event(&mut self, ev: Event, cx: &mut Context<Self>) {
        match ev {
            Event::Connected { authorized } => {
                if authorized {
                    self.status = "signed in".into();
                    self.screen = Screen::Picker;
                    self.send(Command::ListChannels);
                } else {
                    self.status = "sign in to Telegram".into();
                    self.screen = Screen::AuthPhone;
                }
            }
            Event::CodeSent => {
                self.status = "code sent — check Telegram".into();
                self.screen = Screen::AuthCode;
            }
            Event::NeedPassword { hint } => {
                self.status = format!(
                    "2FA required{}",
                    if hint.is_empty() {
                        "".into()
                    } else {
                        format!(" (hint: {hint})")
                    }
                );
                self.screen = Screen::AuthPassword;
            }
            Event::SignedIn { name } => {
                self.signed_in_as = name;
                self.status = "signed in".into();
                self.screen = Screen::Picker;
                self.send(Command::ListChannels);
            }
            Event::Channels(list) => {
                self.channels = list;
                // Auto-open the last used drive once at sign-in — not on
                // every relist, or the drive reopens behind the picker.
                if self.channel.is_none() && !self.auto_open_done {
                    self.auto_open_done = true;
                    if let Some(last) = self.config.as_ref().and_then(|c| c.last_channel_id) {
                        if let Some(info) = self.channels.iter().find(|c| c.id == last).cloned() {
                            self.open_channel(info);
                        }
                    }
                }
            }
            Event::ChannelOpened {
                info,
                pending_uploads,
            } => {
                self.channel = Some(info.clone());
                self.channel_kind = info.kind;
                self.screen = Screen::Drive;
                self.cwd = String::new();
                self.nav_back.clear();
                self.nav_fwd.clear();
                self.trash = false;
                self.modal = None;
                self.selected.clear();
                self.thumbnails.clear();
                self.thumb_requested.clear();
                self.status = format!("opened {}", info.title);
                if let Some(cfg) = &mut self.config {
                    cfg.last_channel_id = Some(info.id);
                    if let Err(e) = cfg.save() {
                        self.status = format!("config save failed: {e}");
                    }
                }
                if pending_uploads > 0 {
                    self.send(Command::ResumeUploads);
                }
            }
            Event::Namespace(ns) => {
                self.namespace = ns;
                // Refreshes land constantly (thumbnails, syncs, ops) —
                // prune ids that vanished rather than wiping the whole
                // selection out from under the user.
                let live = |id: &String| {
                    self.namespace.folders.contains_key(id)
                        || self.namespace.files.contains_key(id)
                };
                self.selected.retain(live);
                if self.anchor.as_ref().is_some_and(|a| !live(a)) {
                    self.anchor = self.selected.first().cloned();
                }
                // A fresh projection is the ack for in-flight ops:
                // a move is done once the object sits under its dest,
                // a delete once the object is gone; copies/restores
                // clear on the first refresh after being sent.
                let now = std::time::Instant::now();
                let ns = &self.namespace;
                let parent_of = |id: &String| {
                    ns.folders
                        .get(id)
                        .map(|e| e.parent.as_str())
                        .or_else(|| ns.files.get(id).map(|e| e.parent.as_str()))
                };
                self.pending_ops.retain(|id, p| {
                    if now > p.deadline {
                        return false;
                    }
                    match p.kind {
                        PendKind::Move => parent_of(id) != Some(p.dest.as_str()),
                        PendKind::Delete => parent_of(id).is_some(),
                        PendKind::Timed => false,
                    }
                });
                self.rubber = None;
            }
            Event::Transfer(t) => {
                self.transfers.insert(t.id.clone(), t.clone());
                if t.state == crate::tg::TransferState::Done {
                    notify(&format!("{} — finished", t.label));
                    // Fade out finished rows instead of pinning them forever.
                    let id = t.id.clone();
                    let timer = cx
                        .background_executor()
                        .timer(std::time::Duration::from_secs(4));
                    cx.spawn(async move |this, cx| {
                        timer.await;
                        let _ = this.update(cx, |this, cx| {
                            this.transfers.remove(&id);
                            cx.notify();
                        });
                    })
                    .detach();
                }
            }
            Event::VaultState {
                configured,
                unlocked,
                hint,
            } => {
                self.vault_configured = configured;
                self.vault_unlocked = unlocked;
                self.vault_hint = hint;
            }
            Event::InviteLink { link } => {
                self.status = format!("invite link: {link}");
            }
            Event::ThumbnailReady { file_id, path } => {
                self.thumbnails.insert(file_id, path);
            }
            Event::ThumbnailFailed { file_id } => {
                // Keep the id marked — a deterministic failure would
                // otherwise re-request on every render forever.
                let _ = file_id;
            }
            Event::PreviewReady { file_id, path } => {
                if let Some(v) = &mut self.viewer {
                    if v.file_id == file_id {
                        v.path = Some(path);
                    }
                }
            }
            Event::PreviewFailed { file_id } => {
                if let Some(v) = &mut self.viewer {
                    if v.file_id == file_id {
                        v.failed = true;
                    }
                }
            }
            Event::SyncState { active } => {
                self.syncing = active;
            }
            Event::MountState { dir } => {
                self.mount_dir = dir.map(|d| d.display().to_string());
                if let Some(d) = &self.mount_dir {
                    self.status = format!("mounted at {d}");
                }
            }
            Event::DavUrl { url } => {
                self.status = format!("mount: {url}");
            }
            Event::StreamUrl { url } => {
                // The tokenized URL is the capability — hand it to the
                // default handler (browser / media player).
                self.status = format!("streaming: {url}");
                let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
            }
            Event::Error { context, message } => {
                self.status = format!("{context}: {message}");
            }
            Event::Status { message } => {
                self.status = message;
            }
            Event::OptimizePlan { dead, scanned } => {
                if dead > 0 {
                    self.modal = Some(Modal::Optimize { dead });
                    self.status = format!("{dead} obsolete messages found in {scanned} scanned");
                } else {
                    self.status = "drive is already optimized".into();
                }
            }
            Event::SignedOut => {
                self.channel = None;
                self.namespace = Arc::new(Namespace::new());
                self.screen = Screen::AuthPhone;
                self.status = "signed out".into();
            }
        }
        cx.notify();
    }

    fn open_channel(&mut self, info: ChannelInfo) {
        self.status = format!("opening {}…", info.title);
        self.send(Command::OpenChannel {
            id: info.id,
            access_hash: info.access_hash,
            title: info.title,
            kind: info.kind,
        });
    }

    fn save_setup(&mut self, cx: &mut Context<Self>) {
        let api_id: i32 = self.f_api_id.read(cx).text().trim().parse().unwrap_or(0);
        let api_hash = self.f_api_hash.read(cx).text().trim().to_string();
        if api_id == 0 || api_hash.is_empty() {
            self.status = "api_id and api_hash are required".into();
            return;
        }
        let cfg = AppConfig {
            api_id,
            api_hash: api_hash.clone(),
            last_channel_id: None,
            download_dir: None,
            upload_limit: crate::config::default_upload_limit(),
            grid_view: false,
        };
        if let Err(e) = cfg.save() {
            self.status = format!("cannot save config: {e}");
            return;
        }
        self.config = Some(cfg);
        self.screen = Screen::Connecting;
        self.status = "connecting…".into();
        self.send(Command::Connect { api_id, api_hash });
    }

    fn request_code(&mut self, cx: &mut Context<Self>) {
        let phone = self.f_phone.read(cx).text().trim().to_string();
        if phone.len() < 5 {
            self.status = "enter a phone number".into();
            return;
        }
        self.status = "requesting code…".into();
        self.send(Command::RequestCode { phone });
    }

    fn submit_code(&mut self, cx: &mut Context<Self>) {
        let code = self.f_code.read(cx).text().trim().to_string();
        if code.is_empty() {
            return;
        }
        self.status = "signing in…".into();
        self.send(Command::SubmitCode { code });
    }

    fn submit_password(&mut self, cx: &mut Context<Self>) {
        let password = self.f_password.read(cx).text();
        if password.is_empty() {
            return;
        }
        self.status = "checking password…".into();
        self.send(Command::SubmitPassword { password });
    }

    fn create_channel(&mut self, cx: &mut Context<Self>) {
        let title = self.f_channel.read(cx).text().trim().to_string();
        if title.is_empty() {
            self.status = "drive name required".into();
            return;
        }
        self.status = "creating drive…".into();
        self.send(Command::CreateChannel { title });
    }

    /// Upload opens the native FILES picker — the common case. Folders
    /// join via "+ Add folder" in the review modal; no Linux dialog
    /// can select files and folders in a single pass (the portal's
    /// `directory` flag means folders *instead of* files).
    fn pick_upload(&mut self, cx: &mut Context<Self>) {
        // TDRIVE_UPLOAD_DEBUG="/a/b:/c/d" skips the picker — headless
        // boxes and scripted tests have no dialogs.
        if let Ok(debug) = std::env::var("TDRIVE_UPLOAD_DEBUG") {
            let paths: Vec<PathBuf> = debug.split(':').map(PathBuf::from).collect();
            self.open_upload_review(paths, cx);
            return;
        }
        let task = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Select files to upload".into()),
        });
        cx.spawn(async move |this, cx| {
            // Cancel still opens the review — it carries the
            // "+ Add folder" button, the only native path to
            // folder picking.
            let paths = match task.await {
                Ok(Ok(Some(paths))) => paths,
                _ => Vec::new(),
            };
            let _ = this.update(cx, |this, cx| this.open_upload_review(paths, cx));
        })
        .detach();
    }

    /// Native picker for the review modal's "+ Add" buttons — files or
    /// folders per `dirs_only`; results append to the open plan.
    fn add_upload_pick(&mut self, dirs_only: bool, cx: &mut Context<Self>) {
        let task = cx.prompt_for_paths(PathPromptOptions {
            files: !dirs_only,
            directories: dirs_only,
            multiple: true,
            prompt: Some(
                if dirs_only {
                    "Select folders to add"
                } else {
                    "Select files to add"
                }
                .into(),
            ),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = task.await {
                let _ = this.update(cx, |this, cx| this.append_upload(paths, cx));
            }
        })
        .detach();
    }

    /// Build the review plan: one row per picked path, dir trees counted
    /// once. Default is "just upload" — items land as-is. An empty
    /// pick still opens the modal — its "+ Add" buttons are the way in.
    fn open_upload_review(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let items: Vec<UploadPick> = paths.into_iter().filter_map(make_pick).collect();
        let default_name = if items.len() == 1 {
            items[0].name.clone()
        } else {
            "upload".to_string()
        };
        self.upload_plan = Some(UploadPlan {
            items,
            mode: PlanMode::Files,
        });
        self.open_modal(Modal::UploadOpts, &default_name, "name", cx);
    }

    /// "+ Add" — merge more picks into the open review. Covered
    /// duplicates are skipped; a newly added dir swallows items
    /// already nested inside it.
    fn append_upload(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let Some(plan) = &mut self.upload_plan else {
            self.open_upload_review(paths, cx);
            return;
        };
        for path in paths {
            let covered = plan
                .items
                .iter()
                .any(|i| i.path == path || (i.dir && path.starts_with(&i.path)));
            if covered {
                continue;
            }
            if path.is_dir() {
                plan.items.retain(|i| !i.path.starts_with(&path));
            }
            if let Some(pick) = make_pick(path) {
                plan.items.push(pick);
            }
        }
        cx.notify();
    }

    /// OS drag-and-drop — the file manager selects files and folders
    /// together, which no Linux picker dialog can do. Dropped paths
    /// feed the same review as picked ones.
    fn external_drop(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = paths.into_iter().filter(|p| p.exists()).collect();
        if paths.is_empty() {
            return;
        }
        if matches!(self.modal, Some(Modal::UploadOpts)) {
            self.append_upload(paths, cx);
        } else if self.modal.is_none() {
            self.open_upload_review(paths, cx);
        }
    }

    /// Apply one cosmetic change to a folder, drop the entry when it
    /// returns to defaults, and persist the map.
    fn set_style(&mut self, obj: &str, f: impl FnOnce(&mut FolderStyle)) {
        let st = self.styles.entry(obj.to_string()).or_default();
        f(st);
        if st.color == 0 && st.badge == 0 {
            self.styles.remove(obj);
        }
        if let (Ok(p), Ok(j)) = (
            crate::config::styles_path(),
            serde_json::to_string_pretty(&self.styles),
        ) {
            let _ = std::fs::write(p, j);
        }
    }

    fn pick_backup(&mut self, cx: &mut Context<Self>) {
        let task = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Select photos to back up".into()),
        });
        let tx = self.tg.clone();
        cx.spawn(async move |_, _| {
            if let Ok(Ok(Some(paths))) = task.await {
                let _ = tx.send(Command::BackupPhotos { paths });
            }
        })
        .detach();
    }

    fn pick_download_dest(&mut self, file_id: String, name: String, cx: &mut Context<Self>) {
        let dir = self
            .config
            .as_ref()
            .and_then(|c| c.download_dir.clone())
            .or_else(dirs::download_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let task = cx.prompt_for_new_path(&dir, Some(&name));
        let tx = self.tg.clone();
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(dest))) = task.await {
                // Remember the folder the user actually picked — next
                // save dialog opens there.
                let _ = this.update(cx, |this, _| {
                    if let Some(cfg) = &mut this.config {
                        cfg.download_dir = dest.parent().map(|p| p.to_path_buf());
                        if let Err(e) = cfg.save() {
                            this.status = format!("config save failed: {e}");
                        }
                    }
                });
                let _ = tx.send(Command::Download { file_id, dest });
            }
        })
        .detach();
    }

    /// Open a modal with a clean f_name — the shared field otherwise
    /// leaks the previous dialog's value and placeholder.
    fn open_modal(
        &mut self,
        modal: Modal,
        prefill: &str,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.f_name
            .update(cx, |f, cx| f.reset(prefill, placeholder, cx));
        self.modal = Some(modal);
        cx.notify();
    }

    /// Descend/breadcrumb navigation — records history for M4/M5.
    fn set_cwd(&mut self, id: String) {
        if id != self.cwd {
            self.nav_back.push(self.cwd.clone());
            self.nav_fwd.clear();
            self.cwd = id;
            self.selected.clear();
            self.context_menu = None;
            self.tools_menu = None;
            self.empty_menu = None;
            self.pending_move = None;
            self.drag_moving = false;
            self.drop_hover = None;
            self.rubber = None;
        }
    }

    /// M4 — history back; falls back to parent-up when history is empty.
    fn navigate_back(&mut self) {
        if let Some(prev) = self.nav_back.pop() {
            self.nav_fwd.push(self.cwd.clone());
            self.cwd = prev;
        } else {
            self.cwd = self
                .namespace
                .folders
                .get(&self.cwd)
                .map(|d| d.parent.clone())
                .unwrap_or_default();
        }
        self.selected.clear();
    }

    /// M5 — history forward.
    fn navigate_fwd(&mut self) {
        if let Some(next) = self.nav_fwd.pop() {
            self.nav_back.push(self.cwd.clone());
            self.cwd = next;
        }
        self.selected.clear();
    }

    fn confirm_mkdir(&mut self, cx: &mut Context<Self>) {
        let name = self.f_name.read(cx).text().trim().to_string();
        if name.is_empty() {
            return;
        }
        self.send(Command::Mkdir {
            parent: self.cwd.clone(),
            name,
        });
        self.f_name.update(cx, |f, cx| f.clear(cx));
        self.modal = None;
    }

    /// Mkdir under cwd with a minted id, then move every object in —
    /// the folder id must exist before the moves are queued.
    fn confirm_move_to_new(&mut self, objs: &[String], cx: &mut Context<Self>) {
        let name = self.f_name.read(cx).text().trim().to_string();
        if name.is_empty() {
            return;
        }
        let id = crate::wire::new_folder_id();
        self.send(Command::MkdirId {
            id: id.clone(),
            parent: self.cwd.clone(),
            name,
        });
        for obj in objs {
            self.send(Command::Move {
                obj: obj.clone(),
                parent: id.clone(),
            });
        }
        self.mark_pending(objs, PendKind::Move, &id, cx);
        self.f_name.update(cx, |f, cx| f.clear(cx));
        self.modal = None;
        self.selected.clear();
    }

    fn confirm_rename(&mut self, obj: &str, cx: &mut Context<Self>) {
        let name = self.f_name.read(cx).text().trim().to_string();
        if name.is_empty() {
            return;
        }
        self.send(Command::Rename {
            obj: obj.to_string(),
            name,
        });
        self.f_name.update(cx, |f, cx| f.clear(cx));
        self.modal = None;
    }

    fn confirm_move(&mut self, objs: &[String], dest: &str, cx: &mut Context<Self>) {
        for obj in objs {
            self.send(Command::Move {
                obj: obj.clone(),
                parent: dest.to_string(),
            });
        }
        self.mark_pending(objs, PendKind::Move, dest, cx);
        self.modal = None;
        self.selected.clear();
    }

    /// Enter key = the modal's confirm button, Escape = cancel.
    fn modal_confirm(&mut self, cx: &mut Context<Self>) {
        match &self.modal {
            Some(Modal::NewFolder) => self.confirm_mkdir(cx),
            Some(Modal::MoveToNew { objs }) => {
                let objs = objs.clone();
                self.confirm_move_to_new(&objs, cx)
            }
            Some(Modal::Rename { obj }) => {
                let obj = obj.clone();
                self.confirm_rename(&obj, cx)
            }
            Some(Modal::Delete { objs, .. }) => {
                let objs = objs.clone();
                self.confirm_delete(&objs, cx)
            }
            Some(Modal::Unlock { .. }) => {
                let pw = self.f_password.read(cx).text();
                self.send(Command::Unlock { password: pw });
                self.f_password.update(cx, |f, cx| f.clear(cx));
                self.modal = None;
            }
            Some(Modal::Encrypt) => {
                let pw = self.f_password.read(cx).text();
                if !pw.is_empty() {
                    let hint = self.f_channel.read(cx).text();
                    self.send(Command::EnableEncryption { password: pw, hint });
                    self.f_password.update(cx, |f, cx| f.clear(cx));
                    self.modal = None;
                }
            }
            Some(Modal::Mount) => {
                let dir = std::path::PathBuf::from(self.f_name.read(cx).text().trim().to_string());
                self.send(Command::Mount { dir });
                self.modal = None;
            }
            Some(Modal::UploadLimit) => {
                if let Ok(n) = self.f_name.read(cx).text().trim().parse::<usize>() {
                    self.send(Command::SetUploadLimit { n });
                }
                self.modal = None;
            }
            Some(Modal::UploadOpts) => {
                let sendable = self
                    .upload_plan
                    .as_ref()
                    .is_some_and(|p| !p.items.is_empty());
                if !sendable {
                    return;
                }
                if let Some(plan) = self.upload_plan.take() {
                    let name = self.f_name.read(cx).text().trim().to_string();
                    let items: Vec<PathBuf> =
                        plan.items.iter().map(|i| i.path.clone()).collect();
                    match plan.mode {
                        PlanMode::Files => self.send(Command::UploadItems {
                            items,
                            parent: self.cwd.clone(),
                            wrap: None,
                        }),
                        PlanMode::Folder => self.send(Command::UploadItems {
                            items,
                            parent: self.cwd.clone(),
                            wrap: Some(if name.is_empty() {
                                "upload".into()
                            } else {
                                name
                            }),
                        }),
                        PlanMode::Archive => self.send(Command::UploadArchive {
                            items,
                            name,
                            parent: self.cwd.clone(),
                            password: {
                                let p = self.f_archpw.read(cx).text().trim().to_string();
                                (!p.is_empty()).then_some(p)
                            },
                        }),
                    }
                }
                self.modal = None;
            }
            Some(Modal::WatchBackup) => {
                let dir = std::path::PathBuf::from(self.f_name.read(cx).text().trim().to_string());
                self.send(Command::WatchBackup { dir });
                self.modal = None;
            }
            Some(Modal::Optimize { .. }) => {
                self.send(Command::OptimizeApply);
                self.modal = None;
            }
            // Swatch/badge clicks apply live — confirm just dismisses.
            Some(Modal::Style { .. }) => self.modal = None,
            _ => {}
        }
        cx.notify();
    }

    fn confirm_delete(&mut self, objs: &[String], cx: &mut Context<Self>) {
        for obj in objs {
            self.send(Command::Delete {
                obj: obj.clone(),
            });
        }
        self.mark_pending(objs, PendKind::Delete, "", cx);
        self.modal = None;
        self.selected.clear();
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content: AnyElement = match self.screen {
            Screen::Setup => self.render_setup(window, cx).into_any_element(),
            Screen::Connecting => self
                .centered_card(div().text_color(theme().dim).child("connecting to Telegram…"))
                .into_any_element(),
            Screen::AuthPhone | Screen::AuthCode | Screen::AuthPassword => {
                self.render_auth(window, cx).into_any_element()
            }
            Screen::Picker => self.render_picker(window, cx).into_any_element(),
            Screen::Drive => {
                if self.modal.is_none()
                    && !self.drive_focus.is_focused(window)
                    && !self.f_search.read(cx).is_focused(window)
                {
                    self.drive_focus.focus(window);
                }
                self.render_drive(window, cx).into_any_element()
            }
        };

        div()
            .size_full()
            .bg(theme().bg)
            .text_color(theme().text)
            .flex()
            .flex_col()
            .child(if self.screen != Screen::Drive {
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .h(px(44.))
                    .px_4()
                    .bg(theme().panel)
                    .border_b_1()
                    .border_color(theme().border)
                    .child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme().accent)
                            .child("TDrive"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme().dim)
                            .child(self.status.clone()),
                    )
                    .into_any_element()
            } else {
                div().into_any_element()
            })
            .child(div().flex_1().flex().child(content))
    }
}

/// One review row per picked path — `None` for paths that aren't a
/// readable file or dir.
fn make_pick(path: PathBuf) -> Option<UploadPick> {
    let (dir, files, bytes) = walk_pick(&path);
    if !dir && files == 0 {
        return None;
    }
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    Some(UploadPick {
        path,
        name,
        dir,
        files,
        bytes,
    })
}

/// Count files + bytes under a picked path — a file reports (1, size),
/// a dir walks once.
fn walk_pick(path: &PathBuf) -> (bool, u64, u64) {
    if path.is_file() {
        return (
            false,
            1,
            std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        );
    }
    if !path.is_dir() {
        return (false, 0, 0);
    }
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut stack = vec![path.clone()];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.is_file() {
                    files += 1;
                    bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
                }
            }
        }
    }
    (true, files, bytes)
}
