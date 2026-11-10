//! GPUI front end — screens, file browser, transfers panel.

use crate::config::AppConfig;
use crate::input::TextField;
use crate::project::{Entry, Namespace};
use crate::tg::{self, ChannelInfo, Command, Event, TransferState, TransferUpdate};
use gpui::*;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

const BG: u32 = 0x101014;
const PANEL: u32 = 0x16161c;
const PANEL2: u32 = 0x1c1c24;
const BORDER: u32 = 0x2a2a35;
const TEXT: u32 = 0xe8e8f0;
const DIM: u32 = 0x8a8a9e;
const ACCENT: u32 = 0x3b82f6;
const DANGER: u32 = 0xef4444;

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
    Rename { obj: String },
    Move { obj: String },
    Delete { obj: String, name: String },
    Unlock { hint: String },
    Encrypt,
    Adopt,
    WatchBackup,
    Mount,
    /// Folders picked for upload — choose structure vs archive.
    UploadDirs { dirs: Vec<PathBuf> },
    /// Max concurrent uploads.
    UploadLimit,
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
    selected: Option<String>,
    transfers: BTreeMap<String, TransferUpdate>,
    pending_uploads: usize,
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
    channel_kind: String,
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
    /// Auto-open the last drive only on the first channel list — a relist
    /// after "Drives" must not reopen it and bounce back.
    auto_open_done: bool,
    /// True while a sync is fetching drive contents — shows the footer
    /// activity bar.
    syncing: bool,
}

impl Root {
    pub fn new(cx: &mut Context<Self>, handle: tg::Handle) -> Self {
        let tg::Handle { tx, rx } = handle;

        let f_api_id = cx.new(|cx| TextField::new(cx, "f-api-id", "api_id"));
        let f_api_hash = cx.new(|cx| TextField::new(cx, "f-api-hash", "api_hash"));
        let f_phone = cx.new(|cx| TextField::new(cx, "f-phone", "+1 555 000 0000"));
        let f_code = cx.new(|cx| TextField::new(cx, "f-code", "login code"));
        let f_password =
            cx.new(|cx| TextField::new(cx, "f-password", "2FA password").masked());
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
            selected: None,
            transfers: BTreeMap::new(),
            pending_uploads: 0,
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
            channel_kind: String::new(),
            mount_dir: None,
            entries_cache: Vec::new(),
            last_items_len: 0,
            grid_cols: 4,
            scroll_handle: ScrollHandle::new(),
            last_view_key: String::new(),
            auto_open_done: false,
            syncing: false,
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
                self.status = format!("2FA required{}", if hint.is_empty() { "".into() } else { format!(" (hint: {hint})") });
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
                self.channel_kind = info.kind.to_string();
                self.pending_uploads = pending_uploads;
                self.screen = Screen::Drive;
                self.cwd = String::new();
                self.trash = false;
                self.modal = None;
                self.selected = None;
                self.thumbnails.clear();
                self.thumb_requested.clear();
                self.status = format!("opened {}", info.title);
                if let Some(cfg) = &mut self.config {
                    cfg.last_channel_id = Some(info.id);
                    let _ = cfg.save();
                }
                if pending_uploads > 0 {
                    self.send(Command::ResumeUploads);
                }
            }
            Event::Namespace(ns) => {
                self.namespace = ns;
                if self.selected.is_some() {
                    self.selected = None;
                }
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
                self.thumb_requested.remove(&file_id);
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
            Event::JournalChanged => {}
            Event::Error { context, message } => {
                self.status = format!("{context}: {message}");
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
            kind: info.kind.to_string(),
        });
    }

    // ---------- actions ----------

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
            upload_limit: 3,
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
        let task = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: Some("Select files or folders to upload".into()),
        });
        let cwd = self.cwd.clone();
        let tx = self.tg.clone();
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = task.await {
                let mut dirs = Vec::new();
                for path in paths {
                    if path.is_dir() {
                        dirs.push(path);
                    } else {
                        let _ = tx.send(Command::Upload {
                            path,
                            parent: cwd.clone(),
                        });
                    }
                }
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
        if let Some(cfg) = &mut self.config {
            cfg.download_dir = Some(dir.clone());
            let _ = cfg.save();
        }
        let task = cx.prompt_for_new_path(&dir, Some(&name));
        let tx = self.tg.clone();
        cx.spawn(async move |_, _| {
            if let Ok(Ok(Some(dest))) = task.await {
                let _ = tx.send(Command::Download { file_id, dest });
            }
        })
        .detach();
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

    fn confirm_move(&mut self, obj: &str, dest: &str) {
        self.send(Command::Move {
            obj: obj.to_string(),
            parent: dest.to_string(),
        });
        self.modal = None;
    }

    /// Enter key = the modal's confirm button, Escape = cancel.
    fn modal_confirm(&mut self, cx: &mut Context<Self>) {
        match &self.modal {
            Some(Modal::NewFolder) => self.confirm_mkdir(cx),
            Some(Modal::Rename { obj }) => {
                let obj = obj.clone();
                self.confirm_rename(&obj, cx)
            }
            Some(Modal::Delete { obj, .. }) => {
                let obj = obj.clone();
                self.confirm_delete(&obj)
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
                let dir = std::path::PathBuf::from(
                    self.f_name.read(cx).text().trim().to_string(),
                );
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
                let dir = std::path::PathBuf::from(
                    self.f_name.read(cx).text().trim().to_string(),
                );
                self.send(Command::WatchBackup { dir });
                self.modal = None;
            }
            _ => {}
        }
        cx.notify();
    }

    fn confirm_delete(&mut self, obj: &str) {
        self.send(Command::Delete {
            obj: obj.to_string(),
        });
        self.modal = None;
        self.selected = None;
    }

    // ---------- rendering ----------

    fn centered_card(&self, content: impl IntoElement) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_center()
            .size_full()
            .child(
                div()
                    .w(px(380.))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .p_6()
                    .rounded_lg()
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .child(content),
            )
    }

    fn render_setup(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let save = cx.listener(|this, _: &MouseDownEvent, _, cx| this.save_setup(cx));
        self.centered_card(
            div()
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "enter" {
                        this.save_setup(cx);
                    }
                }))
                .flex()
                .flex_col()
                .gap_3()
                .child(div().font_weight(FontWeight::BOLD).child("Connect to Telegram"))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(DIM))
                        .child("Create an app at my.telegram.org to get an api_id and api_hash."),
                )
                .child(self.f_api_id.clone())
                .child(self.f_api_hash.clone())
                .child(button("Continue", ACCENT, save)),
        )
    }

    fn render_auth(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let inner = match self.screen {
            Screen::AuthPhone => {
                let submit = cx.listener(|this, _: &MouseDownEvent, _, cx| this.request_code(cx));
                div()
                    .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                        if ev.keystroke.key == "enter" {
                            this.request_code(cx);
                        }
                    }))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(div().font_weight(FontWeight::BOLD).child("Sign in"))
                    .child(div().text_sm().text_color(rgb(DIM)).child(
                        "Enter your phone number with country code.",
                    ))
                    .child(self.f_phone.clone())
                    .child(button("Send code", ACCENT, submit))
                    .into_any_element()
            }
            Screen::AuthCode => {
                let submit = cx.listener(|this, _: &MouseDownEvent, _, cx| this.submit_code(cx));
                div()
                    .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                        if ev.keystroke.key == "enter" {
                            this.submit_code(cx);
                        }
                    }))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(div().font_weight(FontWeight::BOLD).child("Login code"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(DIM))
                            .child("Enter the code sent to your Telegram app."),
                    )
                    .child(self.f_code.clone())
                    .child(button("Sign in", ACCENT, submit))
                    .into_any_element()
            }
            _ => {
                let submit =
                    cx.listener(|this, _: &MouseDownEvent, _, cx| this.submit_password(cx));
                div()
                    .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                        if ev.keystroke.key == "enter" {
                            this.submit_password(cx);
                        }
                    }))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(div().font_weight(FontWeight::BOLD).child("Password"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(DIM))
                            .child("Two-factor authentication is enabled."),
                    )
                    .child(self.f_password.clone())
                    .child(button("Unlock", ACCENT, submit))
                    .into_any_element()
            }
        };
        self.centered_card(inner)
    }

    fn render_picker(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let create = cx.listener(|this, _: &MouseDownEvent, _, cx| this.create_channel(cx));
        let mut list = div().flex().flex_col().gap_1().w_full();
        for ch in &self.channels {
            let info = ch.clone();
            let id = format!("ch-{}", ch.id);
            list = list.child(
                div()
                    .id(SharedString::from(id))
                    .flex()
                    .items_center()
                    .justify_between()
                    .w_full()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(rgb(PANEL2))
                    .hover(|s| s.bg(rgb(0x23232e)))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.open_channel(info.clone());
                            cx.notify();
                        }),
                    )
                    .child(div().text_color(rgb(TEXT)).child(ch.title.clone()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(DIM))
                            .child(ch.kind),
                    ),
            );
        }
        if self.channels.is_empty() {
            list = list.child(
                div()
                    .text_sm()
                    .text_color(rgb(DIM))
                    .child("No channels found — create a drive below."),
            );
        }

        self.centered_card(
            div()
                .flex()
                .flex_col()
                .gap_3()
                .child(div().font_weight(FontWeight::BOLD).child("Choose a drive"))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(DIM))
                        .child("TDrive stores files in a private Telegram channel."),
                )
                .child(
                    div()
                        .id("channel-list")
                        .flex()
                        .flex_col()
                        .gap_2()
                        .max_h(px(280.))
                        .overflow_y_scroll()
                        .child(list),
                )
                .child(self.f_channel.clone())
                .child(button("Create new drive", ACCENT, create))
                .child(button(
                    "New shared drive",
                    ACCENT,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        let title = this.f_channel.read(cx).text().trim().to_string();
                        if !title.is_empty() {
                            this.send(Command::CreateSharedChannel { title });
                            this.status = "creating shared drive…".into();
                        }
                        cx.notify();
                    }),
                ))
                .child(self.f_invite.clone())
                .child(button(
                    "Join shared drive",
                    ACCENT,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        let link = this.f_invite.read(cx).text().trim().to_string();
                        if !link.is_empty() {
                            this.send(Command::JoinInvite { link });
                            this.status = "joining…".into();
                        }
                        cx.notify();
                    }),
                )),
        )
    }

    fn render_drive(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let channel_name = self
            .channel
            .as_ref()
            .map(|c| c.title.clone())
            .unwrap_or_default();

        // Toolbar
        let upload = cx.listener(|this, _: &MouseDownEvent, _, cx| this.pick_upload(cx));
        let backup = cx.listener(|this, _: &MouseDownEvent, _, cx| this.pick_backup(cx));
        let watch = cx.listener(|this, _: &MouseDownEvent, _, cx| {
            this.modal = Some(Modal::WatchBackup);
            cx.notify();
        });
        let mount = cx.listener(|this, _: &MouseDownEvent, _, cx| {
            if this.mount_dir.is_some() {
                this.send(Command::Unmount);
            } else {
                this.modal = Some(Modal::Mount);
            }
            cx.notify();
        });
        let uplim = cx.listener(|this, _: &MouseDownEvent, _, cx| {
            this.modal = Some(Modal::UploadLimit);
            cx.notify();
        });
        let refresh = cx.listener(|this, _: &MouseDownEvent, _, cx| {
            this.send(Command::Sync);
            this.status = "syncing…".into();
            cx.notify();
        });
        let new_folder = cx.listener(|this, _: &MouseDownEvent, _, cx| {
            this.modal = Some(Modal::NewFolder);
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
                this.modal = Some(Modal::Unlock { hint: v_hint.clone() });
            } else {
                this.modal = Some(Modal::Encrypt);
            }
            cx.notify();
        });

        let mut toolbar = div()
            .flex()
            .items_center()
            .gap_2()
            .child(tool_button("↑ Upload", upload))
            .child(tool_button("+ Folder", new_folder))
            .child(tool_button("📷 Backup", backup))
            .child(tool_button("👁 Watch", watch))
            .child(tool_button(
                if self.mount_dir.is_some() { "⏏ Unmount" } else { "⬇ Mount" },
                mount,
            ))
            .child(tool_button("⟳ Sync", refresh))
            .child(tool_button("⇅ Limit", uplim))
            .child(tool_button(
                if self.trash { "Files" } else { "Trash" },
                trash_toggle,
            ));
        if self.trash {
            let et = cx.listener(|this, _: &MouseDownEvent, _, cx| {
                this.send(Command::EmptyTrash);
                cx.notify();
            });
            toolbar = toolbar.child(tool_button_danger("Empty trash", et));
        }
        if self.channel_kind == "broadcast" {
            let lock_label = if self.vault_configured {
                if self.vault_unlocked {
                    "🔓 Unlocked"
                } else {
                    "🔒 Unlock"
                }
            } else {
                "🔒 Encrypt"
            };
            toolbar = toolbar.child(tool_button(lock_label, lock_btn));
        }
        toolbar = toolbar
            .child(div().w(px(150.)).child(self.f_search.clone()));
        if !self.namespace.unadopted.is_empty() && !self.trash {
            let n = self.namespace.unadopted.len();
            let ad = cx.listener(|this, _: &MouseDownEvent, _, cx| {
                this.modal = Some(Modal::Adopt);
                cx.notify();
            });
            toolbar = toolbar.child(tool_button(
                &format!("Unadopted ({n})"),
                ad,
            ));
        }
        if !self.trash {
            let label = if self.grid { "List" } else { "Grid" };
            let gt = cx.listener(|this, _: &MouseDownEvent, _, cx| {
                this.grid = !this.grid;
                cx.notify();
            });
            toolbar = toolbar.child(tool_button(label, gt));
        }
        toolbar = toolbar
            .child(div().flex_1())
            .child(tool_button("Drives", back))
            .child(tool_button("Sign out", sign_out));

        // Breadcrumb
        let mut crumbs = div()
            .flex()
            .items_center()
            .gap_1()
            .text_sm()
            .child(
                div()
                    .id("crumb-root")
                    .cursor_pointer()
                    .text_color(rgb(ACCENT))
                    .child("root")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.cwd = String::new();
                            this.selected = None;
                            cx.notify();
                        }),
                    ),
            );
        for (id, name) in self.namespace.folder_path(&self.cwd) {
            crumbs = crumbs
                .child(div().text_color(rgb(DIM)).child(" / "))
                .child(
                    div()
                        .id(SharedString::from(format!("crumb-{id}")))
                        .cursor_pointer()
                        .text_color(rgb(ACCENT))
                        .child(name)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.cwd = id.clone();
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
        let mut rows = div()
            .id("rows")
            .flex()
            .flex_col()
            .w_full()
            .flex_1()
            .overflow_hidden();
        if self.entries_cache.is_empty() {
            rows = rows.child(
                div()
                    .py_8()
                    .flex()
                    .justify_center()
                    .text_color(rgb(DIM))
                    .child(if self.trash {
                        "trash is empty"
                    } else if !self.f_search.read(cx).text().trim().is_empty() {
                        "no matches"
                    } else {
                        "empty folder"
                    }),
            );
        } else {
            // Plain scrollable list — no virtualization machinery that can
            // silently drop items. For each entry render its row/cell; in
            // grid mode entries are chunked into flex rows.
            let mut body = div()
                .id("entries-scroll")
                .flex()
                .flex_col()
                .w_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll_handle)
                .flex_1();
            if self.grid && !self.trash {
                self.grid_cols = ((f32::from(window.viewport_size().width)
                    / 110.) as usize)
                    .max(1);
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
            rows = rows.child(body);
        }

        // Selected actions
        let mut actions = div().flex().items_center().gap_2().h(px(36.)).px_3();
        if let Some(sel) = &self.selected {
            let name = self
                .namespace
                .files
                .get(sel)
                .map(|f| f.name.clone())
                .or_else(|| self.namespace.folders.get(sel).map(|f| f.name.clone()))
                .unwrap_or_default();
            let sel_c = sel.clone();
            let is_file = sel.starts_with('f');
            if let Some(path) = self.thumbnails.get(&sel_c) {
                actions = actions.child(img(path.clone()).w(px(28.)).h(px(28.)));
            }
            actions = actions
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(TEXT))
                        .child(name.clone()),
                )
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
                    .child(tool_button("Restore", rs))
                    .child(tool_button_danger("Delete forever", pg));
            } else if is_file {
                let file = self.namespace.files.get(sel).cloned();
                if let Some(file) = file {
                    let dl = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        this.pick_download_dest(file.id.clone(), file.name.clone(), cx)
                    });
                    actions = actions.child(tool_button("Download", dl));
                    let id = sel_c.clone();
                    let st = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        this.send(Command::Stream { obj: id.clone() });
                        this.status = "opening stream…".into();
                        cx.notify();
                    });
                    actions = actions.child(tool_button("Open", st));
                }
            }
            if !self.trash {
                let obj = sel_c.clone();
                let rn = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    this.modal = Some(Modal::Rename { obj: obj.clone() });
                    cx.notify();
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
                    .child(tool_button("Rename", rn))
                    .child(tool_button("Move", mv))
                    .child(tool_button_danger("Delete", del));
            }
        }

        // Transfers
        let mut transfers = div().flex().flex_col().gap_1().px_3().py_2();
        for t in self.transfers.values() {
            let frac = if t.total > 0 {
                (t.sent as f32 / t.total as f32).clamp(0., 1.)
            } else {
                0.
            };
            let (state_label, color) = match t.state {
                TransferState::Active => ("", ACCENT),
                TransferState::Paused => ("paused", DIM),
                TransferState::Done => ("done", 0x22c55e),
                TransferState::Failed => ("failed", DANGER),
            };
            let mut row = div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .w(px(200.))
                        .text_sm()
                        .text_color(rgb(TEXT))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(t.label.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .h(px(6.))
                        .rounded_full()
                        .bg(rgb(BORDER))
                        .child(
                            div()
                                .h(px(6.))
                                .w(relative(frac))
                                .rounded_full()
                                .bg(rgb(color)),
                        ),
                )
                .child(
                    div()
                        .w(px(110.))
                        .text_sm()
                        .text_color(rgb(DIM))
                        .child(if state_label.is_empty() {
                            format!("{} / {}", human_size(t.sent), human_size(t.total))
                        } else {
                            state_label.to_string()
                        }),
                );
            if t.state == TransferState::Active || t.state == TransferState::Paused {
                let id = t.id.clone();
                let resume_id = t.id.clone();
                if t.state == TransferState::Active {
                    row = row.child(
                        div()
                            .id(SharedString::from(format!("pa-{id}")))
                            .px_2()
                            .rounded_md()
                            .text_sm()
                            .text_color(rgb(TEXT))
                            .hover(|s| s.bg(rgb(PANEL2)))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                    this.send(Command::Pause { id: id.clone() });
                                    cx.notify();
                                }),
                            )
                            .child("⏸"),
                    );
                } else {
                    row = row.child(
                        div()
                            .id(SharedString::from(format!("rs-{resume_id}")))
                            .px_2()
                            .rounded_md()
                            .text_sm()
                            .text_color(rgb(TEXT))
                            .hover(|s| s.bg(rgb(PANEL2)))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                    this.send(Command::Resume { id: resume_id.clone() });
                                    cx.notify();
                                }),
                            )
                            .child("▶"),
                    );
                }
                let id = t.id.clone();
                row = row.child(
                    div()
                        .id(SharedString::from(format!("cx-{id}")))
                        .px_2()
                        .rounded_md()
                        .hover(|s| s.bg(rgb(PANEL2)))
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.send(Command::Cancel { id: id.clone() });
                                cx.notify();
                            }),
                        )
                        .child(div().text_sm().text_color(rgb(DANGER)).child("✕")),
                );
            }
            transfers = transfers.child(row);
        }

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(BG))
            .track_focus(&self.drive_focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if this.modal.is_some() || this.f_search.read(cx).is_focused(window) {
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
                        let Some(sel) = this.selected.clone() else { return };
                        if sel.starts_with('d') {
                            this.cwd = sel;
                            this.selected = None;
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
                        let Some(sel) = this.selected.clone() else { return };
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
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(ACCENT))
                            .child("TDrive"),
                    )
                    .child(div().text_color(rgb(DIM)).child(channel_name))
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
            .child(
                div()
                    .id("file-scroll")
                    .flex_1()
                    .overflow_y_scroll()
                    .child(rows),
            )
            .child(
                div()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(PANEL))
                    .child(transfers)
                    .child({
                        let pending = self
                            .thumb_requested
                            .len()
                            .saturating_sub(self.thumbnails.len());
                        let label = if self.syncing && pending > 0 {
                            format!("Fetching files… · {pending} thumbnails in flight")
                        } else if self.syncing {
                            "Fetching files…".to_string()
                        } else if pending > 0 {
                            format!("Fetching thumbnails · {pending} in flight")
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
                                        .w(px(90.))
                                        .h(px(3.))
                                        .rounded_sm()
                                        .bg(rgb(ACCENT)),
                                )
                                .child(
                                    div().text_xs().text_color(rgb(DIM)).child(label),
                                )
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
    }

    /// Ordered object ids matching the visible listing (folders first),
    /// for keyboard navigation.
    fn visible_ids(&self, cx: &App) -> Vec<String> {
        let search = self.f_search.read(cx).text().trim().to_lowercase();
        if self.trash {
            return self.namespace.list_trash().iter().map(|t| t.object_id.clone()).collect();
        }
        if !search.is_empty() {
            let mut ids: Vec<String> = self
                .namespace
                .folders
                .values()
                .filter(|d| !d.tombstoned && d.name.to_lowercase().contains(&search))
                .map(|d| d.id.clone())
                .collect();
            let mut files: Vec<String> = self
                .namespace
                .files
                .values()
                .filter(|f| !f.tombstoned && f.name.to_lowercase().contains(&search))
                .map(|f| f.id.clone())
                .collect();
            ids.sort();
            files.sort();
            ids.extend(files);
            return ids;
        }
        self.namespace
            .list(&self.cwd)
            .iter()
            .map(|e| match e {
                Entry::Folder(d) => d.id.clone(),
                Entry::File(f) => f.id.clone(),
            })
            .collect()
    }

    /// Entries for the current view (trash / search / folder listing).
    fn current_entries(&self, cx: &App) -> Vec<Entry> {
        let search = self.f_search.read(cx).text().trim().to_lowercase();
        if self.trash {
            self.namespace
                .list_trash()
                .into_iter()
                .map(|t| {
                    if t.is_folder {
                        self.namespace
                            .folders
                            .get(&t.object_id)
                            .map(|d| Entry::Folder(d.clone()))
                            .unwrap_or(Entry::Folder(crate::project::FolderEntry {
                                id: t.object_id,
                                name: t.original_name,
                                parent: t.original_parent,
                                tombstoned: true,
                                revision: t.original_revision,
                            }))
                    } else {
                        self.namespace
                            .files
                            .get(&t.object_id)
                            .map(|f| Entry::File(f.clone()))
                            .unwrap_or(Entry::File(crate::project::FileEntry {
                                id: t.object_id,
                                msg_id: 0,
                                name: t.original_name,
                                size: 0,
                                parent: t.original_parent,
                                upload_time: t.deleted_at,
                                tombstoned: true,
                                encrypted: false,
                                plaintext_size: 0,
                                upload_uuid: String::new(),
                                part_count: 0,
                                content_msg_id: 0,
                                revision: t.original_revision,
                            }))
                    }
                })
                .collect()
        } else if !search.is_empty() {
            let mut out: Vec<Entry> = self
                .namespace
                .folders
                .values()
                .filter(|d| !d.tombstoned && d.name.to_lowercase().contains(&search))
                .cloned()
                .map(Entry::Folder)
                .collect();
            let mut files: Vec<_> = self
                .namespace
                .files
                .values()
                .filter(|f| !f.tombstoned && f.name.to_lowercase().contains(&search))
                .cloned()
                .collect();
            files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            out.sort_by(|a, b| {
                let an = match a {
                    Entry::Folder(d) => &d.name,
                    Entry::File(f) => &f.name,
                };
                let bn = match b {
                    Entry::Folder(d) => &d.name,
                    Entry::File(f) => &f.name,
                };
                an.to_lowercase().cmp(&bn.to_lowercase())
            });
            out.extend(files.into_iter().map(Entry::File));
            out
        } else {
            self.namespace.list(&self.cwd)
        }
    }

    /// Gallery-style cell: thumbnail (fetched on demand) or type icon.
    fn render_cell(&mut self, entry: &Entry, cx: &mut Context<Self>) -> impl IntoElement {
        let (id, name, is_folder) = match entry {
            Entry::Folder(d) => (d.id.clone(), d.name.clone(), true),
            Entry::File(f) => (f.id.clone(), f.name.clone(), false),
        };
        // Ask the worker for this file's rendition once.
        if !is_folder
            && self.namespace.renditions.contains_key(&id)
            && !self.thumb_requested.contains(&id)
        {
            self.thumb_requested.insert(id.clone());
            self.send(Command::FetchThumbnail { file_id: id.clone() });
        }
        let selected = self.selected.as_deref() == Some(id.as_str());
        let click_id = id.clone();
        let thumb = self.thumbnails.get(&id).cloned();
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .w(px(96.))
            .p_2()
            .bg(if selected { rgb(PANEL2) } else { rgb(BG) })
            .hover(|s| s.bg(rgb(PANEL2)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    this.drive_focus.focus(window);
                    if ev.click_count == 2 && is_folder {
                        this.cwd = click_id.clone();
                        this.selected = None;
                    } else {
                        this.selected = Some(click_id.clone());
                    }
                    cx.notify();
                }),
            )
            .child(
                div()
                    .w(px(80.))
                    .h(px(80.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_2xl()
                    .child(if let Some(p) = thumb {
                        img(p).w(px(80.)).h(px(80.)).into_any_element()
                    } else {
                        div().child(if is_folder { "📁" } else { "📄" }).into_any_element()
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

    fn render_row(&mut self, entry: &Entry, cx: &mut Context<Self>) -> impl IntoElement {
        let (id, name, meta, icon, is_folder) = match entry {
            Entry::Folder(f) => (
                f.id.clone(),
                f.name.clone(),
                "folder".to_string(),
                "📁",
                true,
            ),
            Entry::File(f) => (
                f.id.clone(),
                f.name.clone(),
                if f.encrypted {
                    format!("{} 🔒", human_size(f.plaintext_size))
                } else {
                    human_size(f.size)
                },
                "📄",
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
            .py_2()
            .bg(if selected { rgb(PANEL2) } else { rgb(BG) })
            .hover(|s| s.bg(rgb(PANEL2)))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    this.drive_focus.focus(window);
                    if ev.click_count == 2 && is_folder {
                        this.cwd = id_click.clone();
                        this.selected = None;
                    } else {
                        this.selected = Some(id_click.clone());
                        this.send(Command::FetchThumbnail { file_id: id_click.clone() });
                    }
                    cx.notify();
                }),
            )
            .child(div().w(px(24.)).child(icon))
            .child(
                div()
                    .flex_1()
                    .text_color(rgb(TEXT))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(name),
            )
            .child(div().w(px(140.)).text_sm().text_color(rgb(DIM)).child(meta))
    }

    fn render_modal(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
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
                        let ok = cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            this.confirm_mkdir(cx)
                        });
                        button("Create", ACCENT, ok)
                    })
                    .into_any_element(),
            ),
            Modal::Rename { obj } => {
                let obj2 = obj.clone();
                (
                    "Rename".into(),
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(self.f_name.clone())
                        .child({
                            let ok = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                this.confirm_rename(&obj2, cx)
                            });
                            button("Rename", ACCENT, ok)
                        })
                        .into_any_element(),
                )
            }
            Modal::Move { obj } => {
                let obj = obj.clone();
                let mut list = div()
                    .id("move-list")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .max_h(px(200.))
                    .overflow_y_scroll();
                let root_dest = "".to_string();
                let o = obj.clone();
                list = list.child(move_dest_row("root", "", {
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        this.confirm_move(&o, &root_dest);
                        cx.notify();
                    })
                }));
                for d in self.namespace.folders.values() {
                    if d.tombstoned || d.id == obj {
                        continue;
                    }
                    let dest = d.id.clone();
                    let o = obj.clone();
                    list = list.child(move_dest_row(&d.name, &d.id, {
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.confirm_move(&o, &dest);
                            cx.notify();
                        })
                    }));
                }
                ("Move to…".into(), list.into_any_element())
            }
            Modal::Unlock { hint } => (
                format!("Unlock drive{}", if hint.is_empty() { "".into() } else { format!(" (hint: {hint})") }).into(),
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(self.f_password.clone())
                    .child({
                        let ok = cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            let pw = this.f_password.read(cx).text();
                            this.send(Command::Unlock { password: pw });
                            this.f_password.update(cx, |f, cx| f.clear(cx));
                            this.modal = None;
                            cx.notify();
                        });
                        button("Unlock", ACCENT, ok)
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
                            .text_color(rgb(DIM))
                            .child("Creates a password vault on this drive. There is no recovery without the password."),
                    )
                    .child(self.f_password.clone())
                    .child(self.f_channel.clone())
                    .child({
                        let ok = cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            let pw = this.f_password.read(cx).text();
                            if pw.is_empty() {
                                return;
                            }
                            let hint = this.f_channel.read(cx).text();
                            this.send(Command::EnableEncryption { password: pw, hint });
                            this.f_password.update(cx, |f, cx| f.clear(cx));
                            this.modal = None;
                            cx.notify();
                        });
                        button("Create vault", ACCENT, ok)
                    })
                    .into_any_element(),
            ),
            Modal::Mount => {
                (
                    "Mount drive (read-only)".into(),
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(div().text_sm().text_color(rgb(DIM)).child(
                            "Mount point path — the drive appears as a read-only filesystem.",
                        ))
                        .child(self.f_name.clone())
                        .child({
                            let ok = cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                let dir = std::path::PathBuf::from(
                                    this.f_name.read(cx).text().trim().to_string(),
                                );
                                this.send(Command::Mount { dir });
                                this.modal = None;
                                cx.notify();
                            });
                            button("Mount", ACCENT, ok)
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
                        .child(div().text_sm().text_color(rgb(DIM)).child(
                            "New images in this folder are uploaded to Photos periodically.",
                        ))
                        .child(self.f_name.clone())
                        .child({
                            let ok = cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                let dir = std::path::PathBuf::from(
                                    this.f_name.read(cx).text().trim().to_string(),
                                );
                                this.send(Command::WatchBackup { dir });
                                this.modal = None;
                                cx.notify();
                            });
                            button("Watch", ACCENT, ok)
                        })
                        .into_any_element(),
                )
            }
            Modal::Adopt => {
                let mut list = div().flex().flex_col().gap_1();
                for (id, name, size, _, _) in self.namespace.unadopted.iter() {
                    let msg_id = *id;
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
                            .child(div().flex_1().text_sm().child(format!("{name} ({})", human_size(*size))))
                            .child(tool_button_id(format!("adopt-{id}"), "Adopt", ab)),
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
                let dirs2 = dirs.clone();
                let dirs3 = dirs.clone();
                (
                    "Upload folder".into(),
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(div().text_sm().text_color(rgb(DIM)).child(label))
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child({
                                    let ok = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                        for dir in dirs2.clone() {
                                            this.send(Command::UploadDir {
                                                dir,
                                                parent: this.cwd.clone(),
                                            });
                                        }
                                        this.modal = None;
                                        cx.notify();
                                    });
                                    button("As folder", ACCENT, ok)
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
                                    button(".7z archive", ACCENT, ok)
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
                            .text_color(rgb(DIM))
                            .child("Max parallel uploads (1–16, default 3)"),
                    )
                    .child(self.f_name.clone())
                    .child({
                        let ok = cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            if let Ok(n) =
                                this.f_name.read(cx).text().trim().parse::<usize>()
                            {
                                this.send(Command::SetUploadLimit { n });
                            }
                            this.modal = None;
                            cx.notify();
                        });
                        button("Set", ACCENT, ok)
                    })
                    .into_any_element(),
            ),
            Modal::Delete { obj, name } => {
                let obj = obj.clone();
                (
                    "Delete".into(),
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(DIM))
                                .child(format!("Delete “{name}”?")),
                        )
                        .child({
                            let ok = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                this.confirm_delete(&obj);
                                cx.notify();
                            });
                            button("Delete", DANGER, ok)
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
                    .w(px(360.))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .p_5()
                    .rounded_lg()
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(rgb(BORDER))
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
                    .child(div().font_weight(FontWeight::BOLD).child(title))
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
        .id(SharedString::from(format!("dest-{}", if id.is_empty() { "root" } else { id })))
        .px_3()
        .py_2()
        .rounded_md()
        .bg(rgb(PANEL2))
        .hover(|s| s.bg(rgb(0x23232e)))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, on_click)
        .child(div().text_sm().text_color(rgb(TEXT)).child(name.to_string()))
}

fn button(
    label: &str,
    color: u32,
    on_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("btn-{label}")))
        .flex()
        .justify_center()
        .items_center()
        .h(px(36.))
        .rounded_md()
        .bg(rgb(color))
        .hover(|s| s.opacity(0.85))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, on_click)
        .child(
            div()
                .text_color(rgb(0xffffff))
                .font_weight(FontWeight::MEDIUM)
                .child(label.to_string()),
        )
}

fn tool_button(
    label: &str,
    on_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    tool_button_id(format!("tb-{label}"), label, on_click)
}

fn tool_button_id(
    id: String,
    label: &str,
    on_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(id))
        .px_3()
        .py_1()
        .rounded_md()
        .bg(rgb(PANEL2))
        .border_1()
        .border_color(rgb(BORDER))
        .hover(|s| s.bg(rgb(0x23232e)))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, on_click)
        .child(div().text_sm().text_color(rgb(TEXT)).child(label.to_string()))
}

fn tool_button_danger(
    label: &str,
    on_click: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("tb-{label}")))
        .px_3()
        .py_1()
        .rounded_md()
        .bg(rgb(PANEL2))
        .border_1()
        .border_color(rgb(DANGER))
        .hover(|s| s.bg(rgb(0x23232e)))
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, on_click)
        .child(div().text_sm().text_color(rgb(DANGER)).child(label.to_string()))
}

pub fn human_size(n: i64) -> String {
    const UNIT: i64 = 1024;
    if n < UNIT {
        return format!("{n} B");
    }
    let mut div = UNIT as f64;
    let mut exp = 0;
    while (n as f64) / div >= UNIT as f64 && exp < 6 {
        div *= UNIT as f64;
        exp += 1;
    }
    format!("{:.1} {}B", n as f64 / div, "KMGTPE".chars().nth(exp.min(5)).unwrap_or('E'))
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content: AnyElement = match self.screen {
            Screen::Setup => self.render_setup(window, cx).into_any_element(),
            Screen::Connecting => self
                .centered_card(
                    div()
                        .text_color(rgb(DIM))
                        .child("connecting to Telegram…"),
                )
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
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
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
                    .bg(rgb(PANEL))
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(
                        div()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(ACCENT))
                            .child("TDrive"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(DIM))
                            .child(self.status.clone()),
                    )
                    .into_any_element()
            } else {
                div().into_any_element()
            })
            .child(div().flex_1().flex().child(content))
    }
}
