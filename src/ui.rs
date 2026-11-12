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
    /// Folders picked for upload — choose structure vs archive.
    UploadDirs {
        dirs: Vec<PathBuf>,
    },
    /// Max concurrent uploads.
    UploadLimit,
    /// Confirm deleting `dead` obsolete channel messages (scanned by
    /// OptimizeDrive).
    Optimize {
        dead: usize,
    },
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
    transfers: BTreeMap<String, TransferUpdate>,
    modal: Option<Modal>,
    signed_in_as: String,
    f_search: Entity<TextField>,
    f_invite: Entity<TextField>,
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
            transfers: BTreeMap::new(),
            modal: None,
            signed_in_as: String::new(),
            f_search,
            f_invite,
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
                self.selected.clear();
            }
            Event::Transfer(t) => {
                self.transfers.insert(t.id.clone(), t.clone());
                if t.state == crate::tg::TransferState::Done {
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

    /// One picker for files AND folders — files queue directly, folders
    /// get the structure-vs-archive choice.
    fn pick_upload(&mut self, cx: &mut Context<Self>) {
        // The portal has no "files and folders" chooser — directories:true
        // is a folder-only picker. Files here, folders in pick_upload_dirs.
        let task = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Select files to upload".into()),
        });
        let cwd = self.cwd.clone();
        let tx = self.tg.clone();
        cx.spawn(async move |_, _| {
            if let Ok(Ok(Some(paths))) = task.await {
                for path in paths {
                    let _ = tx.send(Command::Upload {
                        path,
                        parent: cwd.clone(),
                    });
                }
            }
        })
        .detach();
    }

    fn pick_upload_dirs(&mut self, cx: &mut Context<Self>) {
        let task = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Select folders to upload".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(dirs))) = task.await {
                if !dirs.is_empty() {
                    let _ = this.update(cx, |this, cx| {
                        this.modal = Some(Modal::UploadDirs { dirs });
                        cx.notify();
                    });
                }
            }
        })
        .detach();
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

    fn confirm_move(&mut self, objs: &[String], dest: &str) {
        for obj in objs {
            self.send(Command::Move {
                obj: obj.clone(),
                parent: dest.to_string(),
            });
        }
        self.modal = None;
        self.selected.clear();
    }

    /// Enter key = the modal's confirm button, Escape = cancel.
    fn modal_confirm(&mut self, cx: &mut Context<Self>) {
        match &self.modal {
            Some(Modal::NewFolder) => self.confirm_mkdir(cx),
            Some(Modal::Rename { obj }) => {
                let obj = obj.clone();
                self.confirm_rename(&obj, cx)
            }
            Some(Modal::Delete { objs, .. }) => {
                let objs = objs.clone();
                self.confirm_delete(&objs)
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
            Some(Modal::UploadDirs { dirs }) => {
                for dir in dirs.clone() {
                    self.send(Command::UploadDir {
                        dir,
                        parent: self.cwd.clone(),
                    });
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
            _ => {}
        }
        cx.notify();
    }

    fn confirm_delete(&mut self, objs: &[String]) {
        for obj in objs {
            self.send(Command::Delete {
                obj: obj.clone(),
            });
        }
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
