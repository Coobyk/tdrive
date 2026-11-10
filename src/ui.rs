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
                // Auto-open the last used drive if it's still there.
                if self.channel.is_none() {
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
                self.pending_uploads = pending_uploads;
                self.screen = Screen::Drive;
                self.cwd = String::new();
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
                self.transfers.insert(t.id.clone(), t);
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

    fn pick_upload(&mut self, cx: &mut Context<Self>) {
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
            .child(tool_button("⟳ Sync", refresh))
            .child(tool_button(
                if self.trash { "Files" } else { "Trash" },
                trash_toggle,
            ));
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
        toolbar = toolbar
            .child(div().w(px(150.)).child(self.f_search.clone()))
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
        let search = self.f_search.read(cx).text().trim().to_lowercase();
        let entries: Vec<Entry> = if self.trash {
            self.namespace.list_trash()
        } else if !search.is_empty() {
            let mut files: Vec<_> = self
                .namespace
                .files
                .values()
                .filter(|f| !f.tombstoned && f.name.to_lowercase().contains(&search))
                .cloned()
                .collect();
            files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            files.into_iter().map(Entry::File).collect()
        } else {
            self.namespace.list(&self.cwd)
        };
        let mut rows = div().id("rows").flex().flex_col().w_full();
        if entries.is_empty() {
            rows = rows.child(
                div()
                    .py_8()
                    .flex()
                    .justify_center()
                    .text_color(rgb(DIM))
                    .child(if self.trash {
                        "trash is empty"
                    } else if !search.is_empty() {
                        "no matches"
                    } else {
                        "empty folder"
                    }),
            );
        }
        for entry in &entries {
            rows = rows.child(self.render_row(entry, cx));
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
                actions = actions.child(tool_button("Restore", rs));
            } else if is_file {
                let file = self.namespace.files.get(sel).cloned();
                if let Some(file) = file {
                    let dl = cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        this.pick_download_dest(file.id.clone(), file.name.clone(), cx)
                    });
                    actions = actions.child(tool_button("Download", dl));
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
            if t.state == TransferState::Active {
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
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    if ev.click_count == 2 && is_folder {
                        this.cwd = id_click.clone();
                        this.selected = None;
                    } else {
                        this.selected = Some(id_click.clone());
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
    div()
        .id(SharedString::from(format!("tb-{label}")))
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
    format!("{:.1} {}B", n as f64 / div, "KMGTPE".chars().nth(exp).unwrap())
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
            Screen::Drive => self.render_drive(window, cx).into_any_element(),
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
