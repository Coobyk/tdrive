//! Telegram worker — owns the grammers client on a dedicated tokio runtime
//! thread and translates UI commands into MTProto calls.

use crate::config;
use crate::crypto::{self, VaultConfig};
use crate::dav;
use crate::fusefs;
use crate::journal::{DownloadJob, Journal, PartReceipt, UploadJob};
use crate::project::Namespace;
use crate::stream;
use crate::wire::{self, Op, OpType, OpTypeOrUnknown};
use std::collections::HashMap;
use futures_util::FutureExt;
use tokio_util::sync::CancellationToken;
use anyhow::{Context, Result, anyhow};
use grammers_client::client::{LoginToken, PasswordToken};
use grammers_client::media::Media;
use grammers_client::message::InputMessage;
use grammers_client::peer::Peer;
use grammers_client::session::storages::SqliteSession;
use grammers_client::{Client, SignInError, sender::SenderPool};
use grammers_client::session::types::{ChannelKind, PeerAuth, PeerId, PeerRef};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::mpsc;

pub const MAX_PART_BYTES: i64 = 1900 * 1024 * 1024;
pub const LARGE_FILE_MAX_BYTES: i64 = 40 * 1024 * 1024 * 1024;
pub const MAX_PARTS: usize = 32;
const DOWNLOAD_CHUNK: i32 = 512 * 1024;

#[derive(Debug, Clone)]
pub struct ChannelInfo {
    pub id: i64,
    pub access_hash: i64,
    pub title: String,
    pub kind: &'static str,
}

#[derive(Debug)]
pub enum Command {
    Connect { api_id: i32, api_hash: String },
    RequestCode { phone: String },
    SubmitCode { code: String },
    SubmitPassword { password: String },
    ListChannels,
    CreateChannel { title: String },
    CreateSharedChannel { title: String },
    EmptyTrash,
    OpenChannel {
        id: i64,
        access_hash: i64,
        title: String,
        kind: String,
    },
    Sync,
    Upload { path: PathBuf, parent: String },
    /// Upload a local folder — preserves structure via mkdir ops.
    UploadDir { dir: PathBuf, parent: String },
    /// 7z-archive a local folder, then upload the archive.
    UploadArchive { dir: PathBuf, parent: String },
    ResumeUploads,
    Download { file_id: String, dest: PathBuf },
    Mkdir { parent: String, name: String },
    Rename { obj: String, name: String },
    Move { obj: String, parent: String },
    Delete { obj: String },
    Restore { obj: String },
    Purge { obj: String },
    Cancel { id: String },
    Unlock { password: String },
    Lock,
    EnableEncryption { password: String, hint: String },
    JoinInvite { link: String },
    FetchThumbnail { file_id: String },
    Adopt { msg_id: i64, parent: String },
    Stream { obj: String },
    /// Write op issued by the WebDAV server; reply carries the result.
    DavWrite {
        op: DavOp,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    Pause { id: String },
    Resume { id: String },
    BackupPhotos { paths: Vec<PathBuf> },
    Mount { dir: PathBuf },
    Unmount,
    /// Continuous photo backup — watch a folder for new images.
    WatchBackup { dir: PathBuf },
    BackupTick,
    LiveSync,
    SignOut,
    // Internal task→worker messages — spawned transfers report journal
    // and namespace mutations through the loop so state stays owned here.
    /// A locally-emitted op that must be folded into the namespace.
    OpApplied { header: String, msg_id: i64 },
    UploadPartConfirmed { uuid: String, pix: i32, msg_id: i64 },
    UploadJobPatch { uuid: String, local_path: PathBuf },
    DlConfirmed { file_id: String, written: i64 },
    TransferDone { id: String },
    TransferFailed { id: String, label: String, err: String },
}

#[derive(Debug, Clone)]
pub struct TransferUpdate {
    pub id: String,
    pub label: String,
    pub sent: i64,
    pub total: i64,
    pub state: TransferState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferState {
    Active,
    Paused,
    Done,
    Failed,
}

#[derive(Debug)]
pub enum Event {
    Connected { authorized: bool },
    CodeSent,
    NeedPassword { hint: String },
    SignedIn { name: String },
    Channels(Vec<ChannelInfo>),
    ChannelOpened { info: ChannelInfo, pending_uploads: usize },
    Namespace(Arc<Namespace>),
    Transfer(TransferUpdate),
    JournalChanged,
    Error { context: &'static str, message: String },
    VaultState { configured: bool, unlocked: bool, hint: String },
    InviteLink { link: String },
    ThumbnailReady { file_id: String, path: PathBuf },
    ThumbnailFailed { file_id: String },
    /// Sync started/finished — drives the UI's fetch indicator.
    SyncState { active: bool },
    StreamUrl { url: String },
    DavUrl { url: String },
    MountState { dir: Option<PathBuf> },
    SignedOut,
}

/// Write operations arriving over the WebDAV surface.
#[derive(Debug)]
pub enum DavOp {
    /// MKCOL: create folder `name` under `parent` (folder id or "").
    Mkdir { parent: String, name: String },
    /// DELETE: permanently remove an object (TDrive mount flow = harddel).
    Delete { obj: String },
    /// MOVE: relocate `obj` under `dst_parent` as `name`.
    Move {
        obj: String,
        dst_parent: String,
        name: String,
    },
    /// PUT: write `bytes` as `name` under `parent` — single-body fcommit.
    Put {
        parent: String,
        name: String,
        bytes: Vec<u8>,
    },
    /// Overwrite an existing file's content — single-body freplace.
    Replace { obj: String, bytes: Vec<u8> },
}

pub struct Handle {
    pub tx: mpsc::UnboundedSender<Command>,
    pub rx: async_channel::Receiver<Event>,
}

pub fn spawn() -> Handle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Command>();
    let (ev_tx, ev_rx) = async_channel::unbounded::<Event>();
    let cmd_tx_worker = cmd_tx.clone();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            // Grammers' recursive TL deserialization (nested Update vectors
            // in Updates responses) overflows the default 2 MiB stack.
            .thread_stack_size(16 * 1024 * 1024)
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(async move {
            // Periodic backup watcher — the poll loop TDrive runs for its
            // photo-backup lifecycle.
            {
                let tx = cmd_tx_worker.clone();
                tokio::spawn(async move {
                    let mut iv = tokio::time::interval(std::time::Duration::from_secs(60));
                    loop {
                        iv.tick().await;
                        if tx.send(Command::BackupTick).is_err() {
                            return;
                        }
                    }
                });
            }
            let mut worker = Worker {
                cmd_tx: cmd_tx_worker,
                client: None,
                api_hash: String::new(),
                login_token: None,
                password_token: None,
                channel: None,
                channel_id: 0,
                channel_title: String::new(),
                namespace: Arc::new(Namespace::new()),
                master_key: Arc::new(Mutex::new(None)),
                actor_id: 0,
                channel_kind: String::new(),
                paused_downloads: HashMap::new(),
                live_sync_queued: Default::default(),
                ns_shared: Arc::new(Mutex::new(Arc::new(Namespace::new()))),
                fuse_session: None,
                thumb_sem: Arc::new(tokio::sync::Semaphore::new(8)),
                cancels: HashMap::new(),
                last_live_sync: std::time::Instant::now()
                    - std::time::Duration::from_secs(10),
                journal_path: config::journal_path()
                    .unwrap_or_else(|_| std::env::temp_dir().join("tdrive-rs-journal.json")),
                journal: Journal::default(),
                rx: cmd_rx,
                tx: ev_tx,
            };
            worker.journal = Journal::load(&worker.journal_path);
            worker.run().await;
        });
    });

    Handle {
        tx: cmd_tx,
        rx: ev_rx,
    }
}


/// Clones handed to a spawned transfer task — everything it needs to do
/// IO off the command loop. Journal/namespace mutations are reported
/// back as Commands so the worker keeps sole ownership of that state.
#[derive(Clone)]
struct TransferCtx {
    client: Client,
    peer: PeerRef,
    master: Arc<Mutex<Option<[u8; crypto::MASTER_KEY_LEN]>>>,
    tx: async_channel::Sender<Event>,
    cmd: mpsc::UnboundedSender<Command>,
    token: CancellationToken,
}

impl TransferCtx {
    fn emit(&self, ev: Event) {
        let _ = self.tx.try_send(ev);
    }
    fn update(&self, id: &str, label: &str, sent: i64, total: i64, state: TransferState) {
        self.emit(Event::Transfer(TransferUpdate {
            id: id.to_string(),
            label: label.to_string(),
            sent,
            total,
            state,
        }));
    }
    fn journal(&self, c: Command) {
        let _ = self.cmd.send(c);
    }
    fn op_applied(&self, header: String, msg_id: i64) {
        self.journal(Command::OpApplied { header, msg_id });
    }
    async fn send_op(&self, header: &str) -> Result<i64> {
        let msg = self
            .client
            .send_message(self.peer, InputMessage::new().text(header))
            .await?;
        let id = msg.id() as i64;
        if id <= 0 {
            anyhow::bail!("send outcome unknown (msg id 0)");
        }
        Ok(id)
    }
}

struct Worker {
    client: Option<Client>,
    api_hash: String,
    login_token: Option<LoginToken>,
    password_token: Option<PasswordToken>,
    channel: Option<PeerRef>,
    channel_id: i64,
    channel_title: String,
    namespace: Arc<Namespace>,
    master_key: Arc<Mutex<Option<[u8; crypto::MASTER_KEY_LEN]>>>,
    /// Our own user id — adoption ownership checks.
    actor_id: i64,
    /// "broadcast" (personal) or group kind — encryption is personal-only.
    channel_kind: String,
    /// Paused downloads: transfer id -> (file_id, dest dir) for Resume.
    paused_downloads: HashMap<String, (String, PathBuf)>,
    live_sync_queued: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Latest namespace snapshot shared with the WebDAV server.
    ns_shared: Arc<Mutex<Arc<Namespace>>>,
    /// Live FUSE mount session — dropping it unmounts.
    fuse_session: Option<fuser::BackgroundSession>,
    /// Bounds concurrent thumbnail fetches — they spawn as tasks so the
    /// command loop stays free instead of serializing RTTs.
    thumb_sem: Arc<tokio::sync::Semaphore>,
    cancels: HashMap<String, CancellationToken>,
    last_live_sync: std::time::Instant,
    journal: Journal,
    journal_path: PathBuf,
    rx: mpsc::UnboundedReceiver<Command>,
    tx: async_channel::Sender<Event>,
    cmd_tx: mpsc::UnboundedSender<Command>,
}

impl Worker {
    fn emit(&self, ev: Event) {
        let _ = self.tx.try_send(ev);
    }

    fn save_journal(&self) {
        if let Err(e) = self.journal.save(&self.journal_path) {
            self.emit(Event::Error {
                context: "journal",
                message: e.to_string(),
            });
        }
        self.emit(Event::JournalChanged);
    }

    async fn run(&mut self) {
        while let Some(cmd) = self.rx.recv().await {
            // Panics inside grammers (min entities, malformed TL) must not
            // unwind through the loop — a dead worker leaves the UI dumb.
            let res = std::panic::AssertUnwindSafe(self.handle(cmd))
                .catch_unwind()
                .await;
            match res {
                Ok(Err(e)) => self.emit(Event::Error {
                    context: "telegram",
                    message: format!("{e:#}"),
                }),
                Err(panic) => self.emit(Event::Error {
                    context: "panic",
                    message: format!("{panic:?}"),
                }),
                Ok(Ok(())) => {}
            }
        }
    }

    async fn handle(&mut self, cmd: Command) -> Result<()> {
        match cmd {
            Command::Connect { api_id, api_hash } => self.connect(api_id, api_hash).await,
            Command::RequestCode { phone } => self.request_code(phone).await,
            Command::SubmitCode { code } => self.submit_code(code).await,
            Command::SubmitPassword { password } => self.submit_password(password).await,
            Command::ListChannels => self.list_channels().await,
            Command::CreateChannel { title } => self.create_channel(title).await,
            Command::CreateSharedChannel { title } => self.create_channel_inner(title, true).await,
            Command::EmptyTrash => self.empty_trash().await,
            Command::OpenChannel {
                id,
                access_hash,
                title,
                kind,
            } => self.open_channel(id, access_hash, title, kind).await,
            Command::Sync => self.sync().await,
            Command::Upload { path, parent } => self.upload(path, parent).await,
            Command::UploadDir { dir, parent } => self.upload_dir(dir, parent).await,
            Command::UploadArchive { dir, parent } => {
                self.upload_archive(dir, parent).await
            }
            Command::ResumeUploads => self.resume_uploads().await,
            Command::Download { file_id, dest } => self.download(file_id, dest).await,
            Command::Mkdir { parent, name } => self.mkdir(parent, name).await,
            Command::Rename { obj, name } => self.rename(obj, name).await,
            Command::Move { obj, parent } => self.move_obj(obj, parent).await,
            Command::Delete { obj } => self.delete(obj).await,
            Command::Restore { obj } => self.restore(obj).await,
            Command::Purge { obj } => self.purge(obj).await,
            Command::Cancel { id } => self.cancel(&id),
            Command::Unlock { password } => self.unlock(&password).await,
            Command::Lock => {
                *self.master_key.lock().unwrap() = None;
                self.emit_vault_state();
                Ok(())
            }
            Command::EnableEncryption { password, hint } => {
                self.enable_encryption(&password, &hint).await
            }
            Command::JoinInvite { link } => self.join_invite(&link).await,
            Command::FetchThumbnail { file_id } => self.spawn_thumbnail_fetch(&file_id),
            Command::Adopt { msg_id, parent } => self.adopt(msg_id, parent).await,
            Command::Stream { obj } => self.stream(&obj).await,
            Command::DavWrite { op, reply } => {
                let _ = reply.send(self.dav_write(op).await.map_err(|e| format!("{e:#}")));
                Ok(())
            }
            Command::Pause { id } => self.pause(&id).await,
            Command::Resume { id } => self.resume(&id).await,
            Command::BackupPhotos { paths } => self.backup_photos(paths).await,
            Command::WatchBackup { dir } => {
                self.journal.backup_dir = Some(dir.clone());
                self.save_journal();
                self.backup_photos(std::fs::read_dir(dir)
                    .map(|rd| rd.flatten().map(|e| e.path()).collect())
                    .unwrap_or_default())
                .await
            }
            Command::BackupTick => self.backup_tick().await,
            Command::Mount { dir } => self.mount_drive(dir).await,
            Command::Unmount => {
                self.fuse_session = None;
                self.emit(Event::MountState { dir: None });
                Ok(())
            }
            Command::LiveSync => self.live_sync().await,
            Command::SignOut => self.sign_out().await,
            Command::OpApplied { header, msg_id } => {
                let mut ns = (*self.namespace).clone();
                if !ns.apply_message(msg_id, &header) {
                    self.emit(Event::Error {
                        context: "projection",
                        message: format!("locally-emitted op at {msg_id} failed to apply"),
                    });
                }
                self.namespace = Arc::new(ns);
                self.save_namespace();
                self.push_namespace();
                Ok(())
            }
            Command::UploadPartConfirmed { uuid, pix, msg_id } => {
                if let Some(j) = self
                    .journal
                    .uploads
                    .iter_mut()
                    .find(|j| j.upload_uuid == uuid)
                {
                    if let Some(p) = j.parts.get_mut(pix as usize) {
                        p.msg_id = Some(msg_id);
                    }
                }
                self.save_journal();
                Ok(())
            }
            Command::UploadJobPatch { uuid, local_path } => {
                if let Some(j) = self
                    .journal
                    .uploads
                    .iter_mut()
                    .find(|j| j.upload_uuid == uuid)
                {
                    j.local_path = local_path;
                }
                self.save_journal();
                Ok(())
            }
            Command::DlConfirmed { file_id, written } => {
                if let Some(j) = self
                    .journal
                    .downloads
                    .iter_mut()
                    .find(|j| j.file_id == file_id)
                {
                    j.confirmed_bytes = written;
                }
                self.save_journal();
                Ok(())
            }
            Command::TransferDone { id } => {
                self.cancels.remove(&id);
                if let Some(uuid) = id.strip_prefix("up:") {
                    self.journal.uploads.retain(|j| j.upload_uuid != uuid);
                } else if let Some(fid) = id.strip_prefix("dl:") {
                    self.journal.downloads.retain(|j| j.file_id != fid);
                }
                self.save_journal();
                Ok(())
            }
            Command::TransferFailed { id, label, err } => {
                self.cancels.remove(&id);
                // A pause also unwinds as an error — don't overwrite the
                // Paused state the pause() handler already set.
                if !self.paused_downloads.contains_key(&id) {
                    self.emit(Event::Error {
                        context: "transfer",
                        message: format!("{label}: {err}"),
                    });
                    self.emit(Event::Transfer(TransferUpdate {
                        id: id.clone(),
                        label,
                        sent: 0,
                        total: 0,
                        state: TransferState::Failed,
                    }));
                }
                Ok(())
            }
        }
    }

    /// Bundle the worker's shared handles for a detached transfer task.
    fn transfer_ctx(&self, token: CancellationToken) -> Result<TransferCtx> {
        Ok(TransferCtx {
            client: self.client()?.clone(),
            peer: self.channel()?,
            master: self.master_key.clone(),
            tx: self.tx.clone(),
            cmd: self.cmd_tx.clone(),
            token,
        })
    }

    /// Spawn a transfer task and route its failure back as a Command —
    /// the loop is never blocked by transfer IO.
    fn spawn_transfer(
        &self,
        id: &str,
        label: &str,
        fut: impl std::future::Future<Output = Result<()>> + Send + 'static,
    ) {
        let cmd = self.cmd_tx.clone();
        let id = id.to_string();
        let label = label.to_string();
        tokio::spawn(async move {
            if let Err(e) = fut.await {
                let _ = cmd.send(Command::TransferFailed {
                    id,
                    label,
                    err: format!("{e:#}"),
                });
            }
        });
    }

    fn client(&self) -> Result<&Client> {
        self.client.as_ref().context("not connected")
    }

    fn channel(&self) -> Result<PeerRef> {
        self.channel.context("no drive open")
    }

    async fn connect(&mut self, api_id: i32, api_hash: String) -> Result<()> {
        let session_path = config::session_path()?;
        let session = SqliteSession::open(session_path).await?;
        let pool = SenderPool::new(Arc::new(session), api_id);
        let client = Client::new(pool.handle.clone());
        let mut updates = pool.updates;
        tokio::spawn(pool.runner.run());
        // Any incoming update hints at new history — trigger a (cheap)
        // incremental sync back through the command queue.
        let cmd_tx = self.cmd_tx.clone();
        let queued = self.live_sync_queued.clone();
        tokio::spawn(async move {
            while updates.recv().await.is_some() {
                // Coalesce — a chatty account must not queue a resync
                // backlog that starves user commands.
                if !queued.swap(true, std::sync::atomic::Ordering::Relaxed)
                    && cmd_tx.send(Command::LiveSync).is_err()
                {
                    break;
                }
            }
        });
        self.api_hash = api_hash;
        self.client = Some(client);
        let authorized = self.client()?.is_authorized().await.unwrap_or(false);
        self.emit(Event::Connected { authorized });
        Ok(())
    }

    async fn request_code(&mut self, phone: String) -> Result<()> {
        let token = self
            .client()?
            .request_login_code(&phone, &self.api_hash)
            .await
            .map_err(|e| anyhow!("request code: {e}"))?;
        self.login_token = Some(token);
        self.emit(Event::CodeSent);
        Ok(())
    }

    async fn submit_code(&mut self, code: String) -> Result<()> {
        let token = self.login_token.as_ref().context("no login in progress")?;
        match self.client()?.sign_in(token, &code).await {
            Ok(user) => {
                self.emit(Event::SignedIn {
                    name: user.full_name(),
                });
                self.after_login().await?;
            }
            Err(SignInError::PasswordRequired(token)) => {
                let hint = token.hint().unwrap_or("").to_string();
                self.password_token = Some(token);
                self.emit(Event::NeedPassword { hint });
            }
            Err(SignInError::InvalidCode) => {
                return Err(anyhow!("invalid login code"));
            }
            Err(SignInError::InvalidPassword(token)) => {
                self.password_token = Some(token);
                return Err(anyhow!("invalid 2FA password"));
            }
            Err(SignInError::SignUpRequired) => {
                return Err(anyhow!(
                    "this phone number has no Telegram account"
                ));
            }
            Err(SignInError::Other(e)) => return Err(anyhow!("sign in: {e}")),
        }
        Ok(())
    }

    async fn submit_password(&mut self, password: String) -> Result<()> {
        let token = self
            .password_token
            .take()
            .context("no password requested")?;
        match self.client()?.check_password(token, password).await {
            Ok(user) => {
                self.emit(Event::SignedIn {
                    name: user.full_name(),
                });
                self.after_login().await?;
            }
            Err(SignInError::InvalidPassword(token)) => {
                self.password_token = Some(token);
                return Err(anyhow!("invalid 2FA password"));
            }
            Err(e) => return Err(anyhow!("password check: {e}")),
        }
        Ok(())
    }

    async fn after_login(&mut self) -> Result<()> {
        self.login_token = None;
        self.password_token = None;
        if let Ok(me) = self.client()?.get_me().await {
            self.actor_id = me.id().bare_id_unchecked();
        }
        self.list_channels().await
    }

    async fn list_channels(&mut self) -> Result<()> {
        let client = self.client()?.clone();
        let mut dialogs = client.iter_dialogs();
        let mut out = Vec::new();
        while let Some(dialog) = dialogs.next().await? {
            // Min/incomplete entities yield None from to_ref — never call
            // Dialog::peer_ref (it unwraps and kills the worker thread).
            let (kind, title) = match dialog.peer() {
                Peer::Channel(ch) => {
                    if ch.raw.left {
                        continue;
                    }
                    match ch.kind() {
                        Some(ChannelKind::Broadcast) => {
                            ("broadcast", ch.title().to_string())
                        }
                        _ => continue,
                    }
                }
                Peer::Group(g) if g.is_megagroup() => {
                    ("megagroup", g.title().unwrap_or_default().to_string())
                }
                _ => continue,
            };
            let Ok(Some(peer_ref)) = dialog.peer().to_ref().await else {
                continue;
            };
            out.push(ChannelInfo {
                id: peer_ref.id.bare_id_unchecked(),
                access_hash: peer_ref.auth.hash(),
                title,
                kind,
            });
        }
        out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
        self.emit(Event::Channels(out));
        Ok(())
    }

    async fn create_channel(&mut self, title: String) -> Result<()> {
        self.create_channel_inner(title, false).await
    }

    /// `shared`: megagroup + exported invite link (TDrive shared drive).
    async fn create_channel_inner(&mut self, title: String, shared: bool) -> Result<()> {
        let client = self.client()?.clone();
        let updates = client
            .invoke(&grammers_client::tl::functions::channels::CreateChannel {
                broadcast: !shared,
                megagroup: shared,
                for_import: false,
                forum: false,
                title: title.clone(),
                about: "TDrive storage".to_string(),
                geo_point: None,
                address: None,
                ttl_period: None,
            })
            .await?;
        use grammers_client::tl::enums::Updates;
        let chats = match updates {
            Updates::Combined(u) => u.chats,
            Updates::Updates(u) => u.chats,
            _ => return Err(anyhow!("unexpected updates response")),
        };
        for chat in chats {
            if let grammers_client::tl::enums::Chat::Channel(c) = chat {
                self.open_channel(
                    c.id,
                    c.access_hash.unwrap_or(0),
                    c.title,
                    if shared { "megagroup" } else { "broadcast" }.to_string(),
                )
                .await?;
                if shared {
                    self.export_invite().await?;
                }
                return Ok(());
            }
        }
        Err(anyhow!("channel not found in create response"))
    }

    /// Export the invite link for a shared drive and surface it to the UI.
    async fn export_invite(&mut self) -> Result<()> {
        use grammers_client::tl::enums::ExportedChatInvite;
        let invite = self
            .client()?
            .invoke(&grammers_client::tl::functions::messages::ExportChatInvite {
                peer: self.channel()?.into(),
                legacy_revoke_permanent: false,
                request_needed: false,
                expire_date: None,
                usage_limit: None,
                title: None,
                subscription_pricing: None,
            })
            .await?;
        if let ExportedChatInvite::ChatInviteExported(i) = invite {
            self.emit(Event::InviteLink { link: i.link });
        }
        Ok(())
    }

    async fn open_channel(
        &mut self,
        id: i64,
        access_hash: i64,
        title: String,
        kind: String,
    ) -> Result<()> {
        let peer_id = PeerId::channel(id).context("bad channel id")?;
        self.channel = Some(PeerRef {
            id: peer_id,
            auth: PeerAuth::from_hash(access_hash),
        });
        // Read-write WebDAV surface for this drive.
        dav::ensure_server(
            self.client()?.clone(),
            self.channel()?,
            self.ns_shared.clone(),
            self.master_key.clone(),
            self.cmd_tx.clone(),
        )
        .await;
        if let Some(url) = dav::url() {
            self.emit(Event::DavUrl { url });
        }
        if self.channel_id != id {
            // Different drive → different vault.
            *self.master_key.lock().unwrap() = None;
        }
        self.channel_id = id;
        self.channel_title = title.clone();
        self.channel_kind = kind;
        // Load the persisted projection so the first sync is incremental.
        self.namespace = Arc::new(
            config::namespace_path(id)
                .ok()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|s| serde_json::from_str::<crate::project::NamespaceSnapshot>(&s).ok())
                .map(crate::project::Namespace::restore_snapshot)
                .unwrap_or_default(),
        );
        let pending = self
            .journal
            .uploads
            .iter()
            .filter(|j| j.channel_id == id && j.manifest_msg_id.is_none())
            .count();
        self.emit(Event::ChannelOpened {
            info: ChannelInfo {
                id,
                access_hash,
                title,
                kind: match self.channel_kind.as_str() {
                    "megagroup" => "megagroup",
                    "gigagroup" => "gigagroup",
                    _ => "broadcast",
                },
            },
            pending_uploads: pending,
        });
        self.sync().await?;
        self.emit_vault_state();

        // Resume interrupted downloads for this drive.
        let pending_downloads: Vec<DownloadJob> = self
            .journal
            .downloads
            .iter()
            .filter(|j| j.channel_id == id && !j.done)
            .cloned()
            .collect();
        for job in pending_downloads {
            let id = format!("dl:{}", job.file_id);
            let token = CancellationToken::new();
            self.cancels.insert(id.clone(), token.clone());
            match self.transfer_ctx(token) {
                Ok(ctx) => {
                    let name = job.name.clone();
                    self.spawn_transfer(&id, &name, async move {
                        Self::run_download_task(&ctx, job).await
                    });
                }
                Err(e) => self.emit(Event::Error {
                    context: "download",
                    message: format!("{e:#}"),
                }),
            }
        }
        Ok(())
    }

    /// History scan of the open drive. A full scan runs when the namespace
    /// is empty; later refreshes apply only messages past the watermark.
    async fn sync(&mut self) -> Result<()> {
        self.emit(Event::SyncState { active: true });
        let r = self.sync_inner().await;
        self.emit(Event::SyncState { active: false });
        r
    }

    async fn sync_inner(&mut self) -> Result<()> {
        let client = self.client()?.clone();
        let peer = self.channel()?;
        let watermark = self.namespace.last_msg_id;
        let mut ns = (*self.namespace).clone();
        if watermark > 0 {
            // Incremental: forward-iterate from the last seen message.
            let mut iter = client
                .iter_messages(peer)
                .offset_id(watermark as i32)
                .reverse(true);
            while let Some(msg) = iter.next().await? {
                if !ns.apply_message(msg.id() as i64, msg.text()) {
                    if let Some(Media::Document(d)) = msg.media() {
                        ns.record_unadopted(
                            msg.id() as i64,
                            d.name().unwrap_or("unnamed").to_string(),
                            d.size().unwrap_or(0) as i64,
                            msg.sender_id().map(|p| p.bare_id_unchecked()).unwrap_or(0),
                            msg.date().as_second(),
                        );
                    }
                }
            }
        } else {
            let mut iter = client.iter_messages(peer).reverse(true);
            while let Some(msg) = iter.next().await? {
                if !ns.apply_message(msg.id() as i64, msg.text()) {
                    if let Some(Media::Document(d)) = msg.media() {
                        ns.record_unadopted(
                            msg.id() as i64,
                            d.name().unwrap_or("unnamed").to_string(),
                            d.size().unwrap_or(0) as i64,
                            msg.sender_id().map(|p| p.bare_id_unchecked()).unwrap_or(0),
                            msg.date().as_second(),
                        );
                    }
                }
            }
        }
        if self.channel_id != 0 {
            self.journal
                .watermarks
                .insert(self.channel_id, ns.last_msg_id);
            self.save_journal();
        }
        self.namespace = Arc::new(ns);
        self.save_namespace();
        self.push_namespace();

        // Retention sweep: physically purge entries past purge_after, like
        // TDrive's expired-trash sweep. Failures are surfaced, not fatal.
        let now = unix_now();
        let expired: Vec<String> = self
            .namespace
            .trash
            .values()
            .filter(|e| e.purge_after > 0 && e.purge_after <= now)
            .map(|e| e.object_id.clone())
            .collect();
        for obj in expired {
            if let Err(e) = self.purge(obj).await {
                self.emit(Event::Error {
                    context: "trash purge",
                    message: format!("{e:#}"),
                });
            }
        }
        Ok(())
    }

    /// Publish the namespace to the UI and the shared DAV snapshot.
    fn push_namespace(&self) {
        *self.ns_shared.lock().unwrap() = self.namespace.clone();
        self.emit(Event::Namespace(self.namespace.clone()));
    }

    fn save_namespace(&self) {
        if self.channel_id == 0 {
            return;
        }
        if let Ok(path) = config::namespace_path(self.channel_id) {
            if let Ok(json) = serde_json::to_string(&self.namespace.snapshot()) {
                let _ = std::fs::write(path, json);
            }
        }
    }

    async fn send_op(&self, header: &str) -> Result<i64> {
        let msg = self
            .client()?
            .send_message(self.channel()?, InputMessage::new().text(header))
            .await?;
        // grammers may return Empty{id:0} when the send outcome isn't in
        // the response updates — the op IS on the channel, so id 0 is
        // only "uncertain", never fatal. Control ops still apply locally
        // (the next sync dedupes), while f:-id consumers treat 0 as
        // "wait for sync".
        Ok(msg.id() as i64)
    }

    /// Send a control op and fold it into the local namespace.
    async fn send_and_project(&mut self, op: &Op) -> Result<()> {
        let header = wire::format(op);
        let msg_id = self.send_op(&header).await?;
        let mut ns = (*self.namespace).clone();
        if !ns.apply_message(msg_id, &header) {
            anyhow::bail!("op emitted but rejected by projection");
        }
        self.namespace = Arc::new(ns);
        self.save_namespace();
        self.push_namespace();
        Ok(())
    }

    async fn mkdir(&mut self, parent: String, name: String) -> Result<()> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(anyhow!("folder name required"));
        }
        let op = Op {
            op_type: OpTypeOrUnknown::Known(OpType::Mkdir),
            obj: wire::new_folder_id(),
            parent,
            name,
            ..Default::default()
        };
        self.send_and_project(&op).await
    }

    async fn rename(&mut self, obj: String, name: String) -> Result<()> {
        let op = Op {
            op_type: OpTypeOrUnknown::Known(OpType::Rename),
            obj,
            name,
            ..Default::default()
        };
        self.send_and_project(&op).await
    }

    async fn move_obj(&mut self, obj: String, parent: String) -> Result<()> {
        let op = Op {
            op_type: OpTypeOrUnknown::Known(OpType::Move),
            obj,
            parent,
            ..Default::default()
        };
        self.send_and_project(&op).await
    }

    /// Move a live object to the trash — same `trash` op TDrive emits, with
    /// the deterministic op id and 30-day retention window.
    async fn delete(&mut self, obj: String) -> Result<()> {
        let Some((_, revision, tombstoned, _, _)) = self.namespace.dirent_info(&obj)
        else {
            return Err(anyhow!("item not found"));
        };
        if tombstoned {
            return Err(anyhow!("item is already in the trash"));
        }
        let now = unix_now();
        let op = Op {
            op_type: OpTypeOrUnknown::Known(OpType::TrashTree),
            protocol_version: 1,
            op_id: wire::deterministic_op_id("trash", &obj, revision),
            obj,
            expected_revision: revision,
            deleted_at: now,
            purge_after: now + 30 * 24 * 3600,
            ..Default::default()
        };
        self.send_and_project(&op).await
    }

    /// Permanently purge a trashed object: `harddel` marker plus physical
    /// deletion of the file's Telegram body messages.
    async fn purge(&mut self, obj: String) -> Result<()> {
        let Some((is_folder, revision, tombstoned, _, _)) =
            self.namespace.dirent_info(&obj)
        else {
            return Err(anyhow!("item not found"));
        };
        if !tombstoned {
            return Err(anyhow!("only trashed items can be purged"));
        }
        // Collect body messages before the marker lands — content bodies
        // plus any rendition bodies bound to the object.
        let mut bodies = if is_folder {
            self.namespace.subtree_body_msg_ids(&obj)
        } else {
            self.namespace
                .files
                .get(&obj)
                .map(|f| self.namespace.body_msg_ids(f))
                .unwrap_or_default()
        };
        if let Some(kinds) = self.namespace.renditions.get(&obj) {
            bodies.extend(kinds.values().map(|(id, _)| *id));
        }
        let op = Op {
            op_type: OpTypeOrUnknown::Known(OpType::HardDeleteTree),
            protocol_version: 1,
            op_id: wire::deterministic_op_id("purge", &obj, revision),
            obj,
            expected_revision: revision,
            ..Default::default()
        };
        self.send_and_project(&op).await?;
        if !bodies.is_empty() {
            let ids: Vec<i32> = bodies.iter().map(|&m| m as i32).collect();
            let _ = self
                .client()?
                .delete_messages(self.channel()?, &ids)
                .await;
        }
        Ok(())
    }

    // ---------------- uploads ----------------

    async fn resume_uploads(&mut self) -> Result<()> {
        let channel_id = self
            .channel()
            .map(|p| p.id.bare_id_unchecked())
            .unwrap_or(0);
        let jobs: Vec<UploadJob> = self
            .journal
            .uploads
            .iter()
            .filter(|j| j.channel_id == channel_id && j.manifest_msg_id.is_none())
            .cloned()
            .collect();
        for job in jobs {
            let id = format!("up:{}", job.upload_uuid);
            let token = CancellationToken::new();
            self.cancels.insert(id.clone(), token.clone());
            match self.transfer_ctx(token) {
                Ok(ctx) => {
                    let name = job.name.clone();
                    self.spawn_transfer(&id, &name, async move {
                        Self::run_upload_task(&ctx, job).await
                    });
                }
                Err(e) => self.emit(Event::Error {
                    context: "upload",
                    message: format!("{e:#}"),
                }),
            }
        }
        Ok(())
    }

    fn upload_update(&self, id: &str, label: &str, sent: i64, total: i64, state: TransferState) {
        self.emit(Event::Transfer(TransferUpdate {
            id: id.to_string(),
            label: label.to_string(),
            sent,
            total,
            state,
        }));
    }

    async fn upload(&mut self, path: PathBuf, parent: String) -> Result<()> {
        let plaintext_size = std::fs::metadata(&path).context("stat upload")?.len() as i64;
        // TDrive rejects empty plaintext only when unencrypted — the TDE1
        // header makes the stored ciphertext non-empty.
        if plaintext_size == 0 && self.master_key.lock().unwrap().is_none() {
            return Err(anyhow!("empty files can't be uploaded"));
        }
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed".into());

        // Encryption is staged inside the task — even multi-GB encrypts
        // never block the command loop.
        let encrypted = self.master_key.lock().unwrap().is_some();
        let size = if encrypted { plaintext_size } else { plaintext_size };

        if size > LARGE_FILE_MAX_BYTES {
            return Err(anyhow!("file exceeds the maximum upload size"));
        }

        let res: Result<()> = if size <= MAX_PART_BYTES {
            let id = format!("up:{name}");
            let token = CancellationToken::new();
            self.cancels.insert(id.clone(), token.clone());
            let ctx = self.transfer_ctx(token)?;
            let name2 = name.clone();
            self.spawn_transfer(&id, &name, async move {
                Self::upload_single_task(&ctx, path.clone(), path, name2, parent, size, encrypted, plaintext_size).await
            });
            Ok(())
        } else {
            let channel_id = self.channel()?.id.bare_id_unchecked();
            let part_count = (size + MAX_PART_BYTES - 1) / MAX_PART_BYTES;
            if part_count as usize > MAX_PARTS {
                return Err(anyhow!("file would exceed the {MAX_PARTS} part limit"));
            }
            let parts: Vec<PartReceipt> = (0..part_count as usize)
                .map(|i| {
                    let start = i as i64 * MAX_PART_BYTES;
                    PartReceipt {
                        index: i as i32,
                        msg_id: None,
                        size: (size - start).min(MAX_PART_BYTES),
                    }
                })
                .collect();
            let job = UploadJob {
                upload_uuid: uuid::Uuid::new_v4().to_string(),
                channel_id,
                local_path: path.clone(),
                source_path: path.clone(),
                name,
                parent,
                stored_size: size,
                plaintext_size,
                encrypted,
                parts,
                manifest_msg_id: None,
                started_at: unix_now(),
            };
            let id = format!("up:{}", job.upload_uuid);
            let token = CancellationToken::new();
            self.cancels.insert(id.clone(), token.clone());
            // TDrive only journals resumable *unencrypted* uploads —
            // encrypted multipart resume would re-encrypt under a fresh
            // salt and produce permanently corrupt output.
            if !job.encrypted {
                self.journal.uploads.push(job.clone());
                self.save_journal();
            }
            let ctx = self.transfer_ctx(token)?;
            let name = job.name.clone();
            self.spawn_transfer(&id, &name, async move {
                Self::run_upload_task(&ctx, job).await
            });
            Ok(())
        };
        res
    }

    /// Upload a local directory tree: mkdir ops for each subdir (the obj
    /// id is client-generated so we know parents before files go up),
    /// then a spawned upload task per file.
    async fn upload_dir(&mut self, dir: PathBuf, parent: String) -> Result<()> {
        if !dir.is_dir() {
            return Err(anyhow!("not a directory"));
        }
        let mut queue = vec![(dir, parent)];
        let mut files = 0usize;
        while let Some((d, pid)) = queue.pop() {
            let mut entries: Vec<_> = std::fs::read_dir(&d)?
                .filter_map(|e| e.ok())
                .collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let p = e.path();
                if p.is_dir() {
                    let obj = wire::new_folder_id();
                    let op = Op {
                        op_type: OpTypeOrUnknown::Known(OpType::Mkdir),
                        obj: obj.clone(),
                        parent: pid.clone(),
                        name: e.file_name().to_string_lossy().to_string(),
                        ..Default::default()
                    };
                    self.send_and_project(&op).await?;
                    queue.push((p, obj));
                } else if p.is_file() {
                    self.upload(p, pid.clone()).await?;
                    files += 1;
                }
            }
        }
        let _ = files;
        Ok(())
    }

    /// `7z a` the folder to staging, then upload the archive.
    async fn upload_archive(&mut self, dir: PathBuf, parent: String) -> Result<()> {
        let name = dir
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "archive".into());
        let out = config::staging_dir()
            .unwrap_or_else(|_| std::env::temp_dir())
            .join(format!("{name}.7z"));
        let out2 = out.clone();
        let dir2 = dir.clone();
        let status = tokio::task::spawn_blocking(move || {
            std::process::Command::new("7z")
                .arg("a")
                .arg("-t7z")
                .arg(&out2)
                .arg(&dir2)
                .status()
        })
        .await??;
        if !status.success() {
            return Err(anyhow!("7z archive failed (exit {status})"));
        }
        self.upload(out, parent).await
    }

    async fn upload_single_task(
        ctx: &TransferCtx,
        path: PathBuf,
        pt_source: PathBuf,
        name: String,
        parent: String,
        size: i64,
        encrypted: bool,
        plaintext_size: i64,
    ) -> Result<()> {
        let id = format!("up:{}", name);
        ctx.update(&id, &name, 0, size, TransferState::Active);
        // Encrypt to staged ciphertext first when the vault is unlocked.
        let stored = if encrypted {
            let master = (*ctx.master.lock().unwrap()).context("drive locked")?;
            let ct_path = config::staging_dir()
                .unwrap_or_else(|_| std::env::temp_dir())
                .join(format!("tdrive-enc-{}.bin", uuid::Uuid::new_v4()));
            let pt = path.clone();
            let ct = ct_path.clone();
            let stored_len = tokio::task::spawn_blocking(move || {
                crypto::encrypt_file(&master, &pt, &ct)
            })
            .await??;
            Some((ct_path, stored_len as i64))
        } else {
            None
        };
        let (send_path, send_size) = stored.clone().unwrap_or((path.clone(), size));
        let res = Self::upload_single_inner(ctx, &send_path, &name, &parent, send_size, encrypted, plaintext_size).await;
        if let Some((ct, _)) = stored {
            let _ = std::fs::remove_file(ct);
        }
        match res {
            Ok(msg_id) => {
                let header = wire::format(&Op {
                    op_type: OpTypeOrUnknown::Known(OpType::FileUpload),
                    parent,
                    name: name.clone(),
                    file_size: size,
                    file_upload_time: unix_now(),
                    encrypted,
                    plaintext_size: if encrypted { plaintext_size } else { 0 },
                    ..Default::default()
                });
                ctx.op_applied(header, msg_id);
                ctx.update(&id, &name, size, size, TransferState::Done);
                Self::publish_renditions(ctx, &pt_source, &name, msg_id, msg_id, "", encrypted)
                    .await;
                Ok(())
            }
            Err(e) => {
                ctx.update(&id, &name, 0, size, TransferState::Failed);
                Err(e)
            }
        }
    }

    async fn upload_single_inner(ctx: &TransferCtx, path: &PathBuf, name: &str, parent: &str, size: i64, encrypted: bool, plaintext_size: i64) -> Result<i64> {
        let file = tokio::fs::File::open(path).await?;
        let progress = ProgressReader::new(file, {
            let tx = ctx.tx.clone();
            let id = format!("up:{name}");
            let label = name.to_string();
            move |sent, _| {
                let _ = tx.try_send(Event::Transfer(TransferUpdate {
                    id: id.clone(),
                    label: label.clone(),
                    sent,
                    total: size,
                    state: TransferState::Active,
                }));
            }
        });
        let mut reader = progress;
        let uploaded = ctx
            .client
            .upload_stream(&mut reader, size as usize, name.to_string())
            .await?;
        let caption = wire::format(&Op {
            op_type: OpTypeOrUnknown::Known(OpType::FileUpload),
            parent: parent.to_string(),
            name: name.to_string(),
            file_size: size,
            file_upload_time: unix_now(),
            encrypted,
            plaintext_size: if encrypted { plaintext_size } else { 0 },
            ..Default::default()
        });
        let msg = ctx
            .client
            .send_message(
                ctx.peer,
                InputMessage::new().document(uploaded).text(caption),
            )
            .await?;
        let id = msg.id() as i64;
        if id <= 0 {
            anyhow::bail!("send outcome unknown (msg id 0)");
        }
        Ok(id)
    }

    /// Continue (or start) a multipart journal job until the manifest lands.
    /// The upload state machine — runs on a spawned task so the command
    /// loop stays responsive; journal/namespace writes flow back via
    /// Commands on `ctx.cmd`.
    async fn run_upload_task(ctx: &TransferCtx, mut job: UploadJob) -> Result<()> {
        // Encrypt to staged ciphertext when the vault is unlocked — done
        // inside the task so even a multi-GB encrypt never blocks the loop.
        if job.encrypted
            && (!job.local_path.exists() || job.local_path == job.source_path)
        {
            let master = (*ctx.master.lock().unwrap())
                .context("drive locked — cannot resume encrypted upload")?;
            let ct_path = config::staging_dir().unwrap_or_else(|_| std::env::temp_dir()).join(format!(
                "tdrive-enc-{}.bin",
                uuid::Uuid::new_v4()
            ));
            let pt = job.source_path.clone();
            let ct = ct_path.clone();
            tokio::task::spawn_blocking(move || crypto::encrypt_file(&master, &pt, &ct))
                .await??;
            job.local_path = ct_path.clone();
            job.stored_size = std::fs::metadata(&ct_path)
                .map(|m| m.len() as i64)
                .unwrap_or(job.stored_size);
            let n = job.parts.len() as i64;
            for (i, p) in job.parts.iter_mut().enumerate() {
                let start = i as i64 * MAX_PART_BYTES;
                p.size = (job.stored_size - start).min(MAX_PART_BYTES);
            }
            let _ = n;
            ctx.journal(Command::UploadJobPatch {
                uuid: job.upload_uuid.clone(),
                local_path: ct_path,
            });
        }

        let token = &ctx.token;
        let transfer_id = format!("up:{}", job.upload_uuid);
        let already: i64 = job
            .parts
            .iter()
            .filter(|p| p.msg_id.is_some())
            .map(|p| p.size)
            .sum();
        ctx.update(&transfer_id, &job.name, already, job.stored_size, TransferState::Active);

        let pending: Vec<i32> = job.pending_parts();
        for idx in pending {
            if token.is_cancelled() {
                return Err(anyhow!("upload cancelled"));
            }
            let part = job.parts[idx as usize].clone();
            let offset = idx as i64 * MAX_PART_BYTES;
            let msg_id = Self::send_part(ctx, &job, part.index, offset, part.size).await?;
            job.parts[idx as usize].msg_id = Some(msg_id);
            ctx.journal(Command::UploadPartConfirmed {
                uuid: job.upload_uuid.clone(),
                pix: idx,
                msg_id,
            });
            let done: i64 = job
                .parts
                .iter()
                .filter(|p| p.msg_id.is_some())
                .map(|p| p.size)
                .sum();
            ctx.update(&transfer_id, &job.name, done, job.stored_size, TransferState::Active);
        }

        // Manifest — the file's visibility boundary.
        let manifest = Op {
            op_type: OpTypeOrUnknown::Known(OpType::FileManifest),
            upload_uuid: job.upload_uuid.clone(),
            parent: job.parent.clone(),
            name: job.name.clone(),
            part_count: job.parts.len() as i32,
            file_size: job.stored_size,
            file_upload_time: job.started_at,
            encrypted: job.encrypted,
            plaintext_size: if job.encrypted { job.plaintext_size } else { 0 },
            ..Default::default()
        };
        let header = wire::format(&manifest);
        let msg_id = ctx.send_op(&header).await?;
        ctx.op_applied(header, msg_id);
        ctx.journal(Command::TransferDone { id: transfer_id.clone() });
        ctx.update(&transfer_id, &job.name, job.stored_size, job.stored_size, TransferState::Done);
        Self::publish_renditions(ctx, &job.source_path, &job.name, msg_id, 0, &job.upload_uuid, job.encrypted)
            .await;
        Ok(())
    }

    /// Best-effort rendition publish — TDrive generates a 512px "thumb" and
    /// 1600px "preview" JPEG for image uploads, sent as `rend` part ops.
    /// Failures never invalidate the stored original.
    async fn publish_renditions(
        ctx: &TransferCtx,
        source: &PathBuf,
        name: &str,
        file_msg_id: i64,
        content_msg_id: i64,
        upload_uuid: &str,
        encrypted: bool,
    ) {
        let owned_source;
        let source = if is_image_name(name) {
            source
        } else if is_video_name(name) {
            // TDrive renders video thumbs through ffmpeg; extract a frame
            // locally and feed the same rendition pipeline.
            match extract_video_frame(source).await {
                Some(p) => {
                    owned_source = p;
                    &owned_source
                }
                None => return,
            }
        } else {
            return;
        };
        for (kind, edge) in [("thumbnail", 512u32), ("preview", 1600u32)] {
            if let Err(e) = Self::publish_rendition(
                    ctx,
                    source,
                    file_msg_id,
                    content_msg_id,
                    upload_uuid,
                    encrypted,
                    kind,
                    edge,
                )
                .await
            {
                ctx.emit(Event::Error {
                    context: "rendition",
                    message: format!("{e:#}"),
                });
            }
        }
    }

    async fn publish_rendition(
        ctx: &TransferCtx,
        source: &PathBuf,
        file_msg_id: i64,
        content_msg_id: i64,
        upload_uuid: &str,
        encrypted: bool,
        kind: &str,
        edge: u32,
    ) -> Result<()> {
        let src = source.clone();
        let (jpeg, w, h) = tokio::task::spawn_blocking(move || {
            let img = image::open(&src).context("decode")?;
            let t = img.thumbnail(edge, edge);
            let mut cur = std::io::Cursor::new(Vec::new());
            t.write_to(&mut cur, image::ImageFormat::Jpeg)
                .context("jpeg encode")?;
            Ok::<_, anyhow::Error>((cur.into_inner(), t.width(), t.height()))
        })
        .await??;
        let pt_len = jpeg.len() as i64;
        let (body, stored) = if encrypted {
            let master = (*ctx.master.lock().unwrap()).context("drive locked")?;
            // TDrive wraps encrypted renditions in a TDR1 envelope binding
            // the descriptor (s=0) — Go's decryptRendition requires it.
            let desc = serde_json::json!({
                "c": ctx.peer.id.bare_id_unchecked(), "f": file_msg_id, "m": content_msg_id,
                "u": upload_uuid, "k": kind, "v": 1, "s": 0,
                "p": pt_len, "w": w, "h": h, "e": true,
            });
            let desc_bytes = serde_json::to_vec(&desc)?;
            let mut plain =
                Vec::with_capacity(8 + desc_bytes.len() + jpeg.len());
            plain.extend_from_slice(b"TDR1");
            plain.extend_from_slice(&(desc_bytes.len() as u32).to_be_bytes());
            plain.extend_from_slice(&desc_bytes);
            plain.extend_from_slice(&jpeg);
            let ct = tokio::task::spawn_blocking(move || crypto::encrypt_bytes(&master, &plain))
                .await??;
            let len = ct.len() as i64;
            (ct, len)
        } else {
            (jpeg, pt_len)
        };
        let rend = wire::Rendition {
            channel_id: ctx.peer.id.bare_id_unchecked(),
            file_msg_id,
            content_msg_id,
            upload_uuid: upload_uuid.to_string(),
            kind: kind.to_string(),
            version: 1,
            size: stored,
            plaintext_size: pt_len,
            width: w as i32,
            height: h as i32,
            encrypted,
        };
        let mut reader = std::io::Cursor::new(&body);
        let uploaded = ctx
            .client
            .upload_stream(&mut reader, body.len(), "rendition.jpg".to_string())
            .await?;
        let caption = wire::format(&Op {
            op_type: OpTypeOrUnknown::Known(OpType::FilePart),
            upload_uuid: uuid::Uuid::new_v4().to_string(),
            part_index: 0,
            file_size: stored,
            rendition: rend.encode(),
            ..Default::default()
        });
        ctx.client
            .send_message(
                ctx.peer,
                InputMessage::new().document(uploaded).text(caption),
            )
            .await?;
        Ok(())
    }

    async fn send_part(ctx: &TransferCtx, job: &UploadJob, index: i32, offset: i64, size: i64) -> Result<i64> {
        let file = tokio::fs::File::open(&job.local_path).await?;
        let mut limited = file;
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        limited.seek(std::io::SeekFrom::Start(offset as u64)).await?;
        let take = limited.take(size as u64);
        let progress = ProgressReader::new(take, {
            let tx = ctx.tx.clone();
            let id = format!("up:{}", job.upload_uuid);
            let label = job.name.clone();
            let total = job.stored_size;
            let base: i64 = job
                .parts
                .iter()
                .filter(|p| p.msg_id.is_some())
                .map(|p| p.size)
                .sum();
            move |sent, _| {
                let _ = tx.try_send(Event::Transfer(TransferUpdate {
                    id: id.clone(),
                    label: label.clone(),
                    sent: base + sent,
                    total,
                    state: TransferState::Active,
                }));
            }
        });
        let mut reader = progress;
        let part_name = part_attachment_name(&job.name, index, job.parts.len() as i32);
        let uploaded = ctx
            .client
            .upload_stream(&mut reader, size as usize, part_name)
            .await?;
        let caption = wire::format(&Op {
            op_type: OpTypeOrUnknown::Known(OpType::FilePart),
            upload_uuid: job.upload_uuid.clone(),
            part_index: index,
            file_size: size,
            ..Default::default()
        });
        let msg = ctx
            .client
            .send_message(
                ctx.peer,
                InputMessage::new().document(uploaded).text(caption),
            )
            .await?;
        let id = msg.id() as i64;
        if id <= 0 {
            anyhow::bail!("send outcome unknown (msg id 0)");
        }
        Ok(id)
    }

    // ---------------- downloads ----------------

    async fn download(&mut self, file_id: String, dest_dir: PathBuf) -> Result<()> {
        let file = self
            .namespace
            .files
            .get(&file_id)
            .cloned()
            .context("file not found")?;
        if file.encrypted && self.master_key.lock().unwrap().is_none() {
            return Err(anyhow!("drive is locked — unlock it to download"));
        }
        let body_ids = self.namespace.body_msg_ids(&file);
        if body_ids.is_empty() {
            return Err(anyhow!("file body messages are missing"));
        }
        let channel_id = self.channel()?.id.bare_id_unchecked();
        let dest = if dest_dir.is_dir() {
            unique_dest(dest_dir.join(&file.name))
        } else {
            dest_dir
        };
        let job = DownloadJob {
            channel_id,
            file_id: file.id.clone(),
            name: file.name.clone(),
            body_msg_ids: body_ids,
            total_size: file.size,
            dest,
            confirmed_bytes: 0,
            done: false,
            encrypted: file.encrypted,
        };
        self.cancels.insert(
            format!("dl:{}", job.file_id),
            CancellationToken::new(),
        );
        self.journal.downloads.push(job.clone());
        self.save_journal();
        let id = format!("dl:{}", job.file_id);
        let ctx = self.transfer_ctx(CancellationToken::new())?;
        let name = job.name.clone();
        self.spawn_transfer(&id, &name, async move {
            Self::run_download_task(&ctx, job).await
        });
        Ok(())
    }

    /// Download state machine — spawned task; journal/namespace writes
    /// flow back through Commands on `ctx.cmd`.
    async fn run_download_task(ctx: &TransferCtx, job: DownloadJob) -> Result<()> {
        let client = ctx.client.clone();
        let peer = ctx.peer;
        let transfer_id = format!("dl:{}", job.file_id);
        ctx.update(&transfer_id, &job.name, job.confirmed_bytes, job.total_size, TransferState::Active);

        let staging = job.dest.with_extension(format!(
            "{}.tdrive-part",
            job.dest
                .extension()
                .map(|e| e.to_string_lossy().to_string())
                .unwrap_or_default()
        ));
        let mut out = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&staging)
            .await?;
        use tokio::io::{AsyncSeekExt, AsyncWriteExt};

        // Fetch every body message once to learn each part's size and
        // assemble byte offsets — needed to map confirmed_bytes to a
        // (part, chunk) resume point.
        let ids: Vec<i32> = job.body_msg_ids.iter().map(|&id| id as i32).collect();
        let msgs = client.get_messages_by_id(peer, &ids).await?;
        let mut parts: Vec<(i64, Media)> = Vec::with_capacity(msgs.len()); // (offset, media)
        let mut offset = 0i64;
        for (m, &msg_id) in msgs.iter().zip(&job.body_msg_ids) {
            let Some(msg) = m else {
                return Err(anyhow!("body message {msg_id} is gone"));
            };
            let Some(media) = msg.media() else {
                return Err(anyhow!("body message {msg_id} has no media"));
            };
            parts.push((offset, media));
            offset += media_size(&parts.last().unwrap().1) as i64;
        }

        let token = &ctx.token;
        let mut written = job.confirmed_bytes;
        for (part_offset, media) in parts {
            let doc_size = media_size(&media) as i64;
            let part_end = part_offset + doc_size;
            if written >= part_end {
                continue; // this part is fully confirmed
            }
            // Resume mid-part at a whole-chunk boundary; bytes past the last
            // confirmed chunk are re-fetched.
            let within = (written - part_offset).max(0);
            let skip = (within / DOWNLOAD_CHUNK as i64) as i32;
            written = part_offset + skip as i64 * DOWNLOAD_CHUNK as i64;
            out.set_len(written as u64).await?;
            out.seek(std::io::SeekFrom::Start(written as u64)).await?;
            ctx.journal(Command::DlConfirmed {
                file_id: job.file_id.clone(),
                written,
            });

            let mut dl = client.iter_download(&media).chunk_size(DOWNLOAD_CHUNK);
            if skip > 0 {
                dl = dl.skip_chunks(skip);
            }
            let mut since_ckpt = 0u32;
            while let Some(chunk) = dl.next().await? {
                if token.is_cancelled() {
                    ctx.journal(Command::DlConfirmed {
                        file_id: job.file_id.clone(),
                        written,
                    });
                    return Err(anyhow!("download cancelled"));
                }
                out.write_all(&chunk).await?;
                written += chunk.len() as i64;
                since_ckpt += 1;
                if since_ckpt >= 8 {
                    // TDrive checkpoints every 8 blocks — per-chunk fsync +
                    // full-journal rewrite is pathologically slow.
                    since_ckpt = 0;
                    out.sync_data().await?;
                    ctx.journal(Command::DlConfirmed {
                        file_id: job.file_id.clone(),
                        written,
                    });
                    ctx.update(&transfer_id, &job.name, written, job.total_size, TransferState::Active);
                }
            }
            out.sync_data().await?;
        }
        drop(out);
        if job.encrypted {
            let master = (*ctx.master.lock().unwrap()).context("drive locked mid-download")?;
            let staging2 = staging.clone();
            let dest2 = job.dest.clone();
            tokio::task::spawn_blocking(move || {
                crypto::decrypt_file(&master, &staging2, &dest2)
            })
            .await??;
            let _ = std::fs::remove_file(&staging);
        } else {
            std::fs::rename(&staging, &job.dest)?;
        }
        ctx.journal(Command::TransferDone {
            id: transfer_id.clone(),
        });
        ctx.update(&transfer_id, &job.name, written, job.total_size, TransferState::Done);
        Ok(())
    }

    // ---------------- vault ----------------

    fn emit_vault_state(&self) {
        let configured = self.namespace.encfg.is_some();
        let hint = self
            .namespace
            .encfg
            .as_ref()
            .map(|o| o.hint.clone())
            .unwrap_or_default();
        self.emit(Event::VaultState {
            configured,
            unlocked: self.master_key.lock().unwrap().is_some(),
            hint,
        });
    }

    async fn unlock(&mut self, password: &str) -> Result<()> {
        let op = self
            .namespace
            .encfg
            .clone()
            .context("drive has no encryption vault")?;
        let vault = VaultConfig::from_op(
            &op.kdf_salt_b64,
            &op.kdf_params_json,
            &op.wrapped_key_b64,
            &op.key_check_b64,
            &op.hint,
            op.config_version,
        )?;
        let key = tokio::task::spawn_blocking({
            let vault = vault.clone();
            let password = password.to_string();
            move || vault.unlock(password.as_bytes())
        })
        .await??;
        *self.master_key.lock().unwrap() = Some(key);
        self.emit_vault_state();
        Ok(())
    }

    async fn enable_encryption(&mut self, password: &str, hint: &str) -> Result<()> {
        if self.channel_kind != "broadcast" {
            return Err(anyhow!("encryption is only supported on personal drives"));
        }
        if self.namespace.encfg.is_some() {
            return Err(anyhow!("encryption already configured"));
        }
        let password = password.to_string();
        let hint = hint.to_string();
        let (vault, master) = tokio::task::spawn_blocking(move || {
            crypto::create_vault(password.as_bytes(), &hint)
        })
        .await??;
        let (salt, kdf, wrap, check, _, cv) = vault.to_op_fields();
        let op = Op {
            op_type: OpTypeOrUnknown::Known(OpType::EncConfig),
            kdf_salt_b64: salt,
            kdf_params_json: kdf,
            wrapped_key_b64: wrap,
            key_check_b64: check,
            hint: vault.hint.clone(),
            config_version: cv,
            ..Default::default()
        };
        self.send_and_project(&op).await?;
        *self.master_key.lock().unwrap() = Some(master);
        self.emit_vault_state();
        Ok(())
    }

    async fn empty_trash(&mut self) -> Result<()> {
        let ids: Vec<String> = self.namespace.trash.keys().cloned().collect();
        for obj in ids {
            if let Err(e) = self.purge(obj.clone()).await {
                self.emit(Event::Error {
                    context: "empty trash",
                    message: format!("{obj}: {e:#}"),
                });
            }
        }
        Ok(())
    }

    // ---------------- misc ops ----------------

    /// Restore from the trash — TDrive's rule: land at the original parent
    /// (root if it no longer exists) under a conflict-free sibling name.
    async fn restore(&mut self, obj: String) -> Result<()> {
        let entry = self
            .namespace
            .trash
            .get(&obj)
            .cloned()
            .context("item is not in the trash")?;
        let Some((_, revision, tombstoned, _, _)) = self.namespace.dirent_info(&obj)
        else {
            return Err(anyhow!("item not found"));
        };
        if !tombstoned {
            return Err(anyhow!("item is not in the trash"));
        }
        let parent = if !entry.original_parent.is_empty()
            && self
                .namespace
                .folders
                .get(&entry.original_parent)
                .map(|d| !d.tombstoned)
                .unwrap_or(false)
        {
            entry.original_parent.clone()
        } else {
            String::new()
        };
        let name = self.namespace.next_free_sibling_name(
            &parent,
            &entry.original_name,
            entry.is_folder,
        );
        let op = Op {
            op_type: OpTypeOrUnknown::Known(OpType::RestoreTree),
            protocol_version: 1,
            op_id: wire::deterministic_op_id("restore", &obj, revision),
            obj,
            parent,
            name,
            expected_revision: revision,
            ..Default::default()
        };
        self.send_and_project(&op).await
    }

    /// Pause suspends the transfer but keeps journal state, unlike cancel
    /// which discards it — the same distinction TDrive makes.
    async fn pause(&mut self, id: &str) -> Result<()> {
        if let Some(token) = self.cancels.remove(id) {
            // Drop the cancelled token — resume must mint a fresh one or
            // is_cancelled() fires instantly and the job can never resume.
            token.cancel();
        }
        if let Some(fid) = id.strip_prefix("dl:") {
            if let Some(j) = self.journal.downloads.iter().find(|j| j.file_id == fid) {
                let dir = j
                    .dest
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_default();
                self.paused_downloads
                    .insert(id.to_string(), (j.file_id.clone(), dir));
            }
        }
        self.emit(Event::Transfer(TransferUpdate {
            id: id.to_string(),
            label: String::new(),
            sent: 0,
            total: 0,
            state: TransferState::Paused,
        }));
        Ok(())
    }

    async fn resume(&mut self, id: &str) -> Result<()> {
        if id.starts_with("up:") {
            return self.resume_uploads().await;
        }
        if let Some((file_id, _dir)) = self.paused_downloads.remove(id) {
            // Reuse the existing journal row — a fresh DownloadJob would
            // reset confirmed_bytes and re-download from scratch.
            let job = self
                .journal
                .downloads
                .iter()
                .find(|j| j.file_id == file_id)
                .cloned();
            if let Some(job) = job {
                let token = CancellationToken::new();
                self.cancels.insert(id.to_string(), token.clone());
                let ctx = self.transfer_ctx(token)?;
                let name = job.name.clone();
                self.spawn_transfer(id, &name, async move {
                    Self::run_download_task(&ctx, job).await
                });
                return Ok(());
            }
        }
        Err(anyhow!("nothing to resume"))
    }

    fn cancel(&mut self, id: &str) -> Result<()> {
        if let Some(token) = self.cancels.get(id) {
            token.cancel();
        }
        // Discard queued journal state for the transfer.
        if let Some(uuid) = id.strip_prefix("up:") {
            self.journal.uploads.retain(|j| j.upload_uuid != uuid);
            self.save_journal();
        }
        if let Some(fid) = id.strip_prefix("dl:") {
            let dest = self
                .journal
                .downloads
                .iter()
                .find(|j| j.file_id == fid)
                .map(|j| {
                    j.dest.with_extension(format!(
                        "{}.tdrive-part",
                        j.dest
                            .extension()
                            .map(|e| e.to_string_lossy().to_string())
                            .unwrap_or_default()
                    ))
                });
            self.journal.downloads.retain(|j| j.file_id != fid);
            if let Some(staging) = dest {
                let _ = std::fs::remove_file(staging);
            }
            self.save_journal();
        }
        self.emit(Event::Transfer(TransferUpdate {
            id: id.to_string(),
            label: String::new(),
            sent: 0,
            total: 0,
            state: TransferState::Failed,
        }));
        Ok(())
    }

    async fn join_invite(&mut self, link: &str) -> Result<()> {
        // t.me/+hash or t.me/joinchat/hash
        let hash = link
            .trim()
            .rsplit('/')
            .next()
            .unwrap_or("")
            .trim_start_matches('+')
            .to_string();
        if hash.is_empty() {
            return Err(anyhow!("not an invite link"));
        }
        let client = self.client()?.clone();
        let result = client
            .invoke(&grammers_client::tl::functions::messages::ImportChatInvite {
                hash: hash.clone(),
            })
            .await;
        use grammers_client::tl::enums::{Updates, messages::ChatInviteJoinResult};
        let ok = match result {
            Ok(ChatInviteJoinResult::Ok(ok)) => ok,
            Err(e) if format!("{e}").contains("INVITE_REQUEST_SENT") => {
                // Join-request links: poll CheckChatInvite until approved,
                // mirroring TDrive's CheckPendingJoin loop.
                self.poll_pending_join(&hash).await?;
                return Ok(());
            }
            _ => return Err(anyhow!("invite requires an interactive join")),
        };
        let chats = match ok.updates {
            Updates::Combined(u) => u.chats,
            Updates::Updates(u) => u.chats,
            _ => return Err(anyhow!("unexpected updates response")),
        };
        for chat in chats {
            if let grammers_client::tl::enums::Chat::Channel(c) = chat {
                self.open_channel(
                    c.id,
                    c.access_hash.unwrap_or(0),
                    c.title,
                    if c.megagroup { "megagroup" } else { "broadcast" }.to_string(),
                )
                .await?;
                self.list_channels().await?;
                return Ok(());
            }
        }
        Err(anyhow!("invite did not resolve to a channel"))
    }

    /// Poll `CheckChatInvite` until a join-request is approved (or give up).
    async fn poll_pending_join(&mut self, hash: &str) -> Result<()> {
        use grammers_client::tl::enums::{Chat, ChatInvite};
        self.emit(Event::Error {
            context: "join",
            message: "waiting for admin approval…".to_string(),
        });
        for _ in 0..24 {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let inv = self
                .client()?
                .invoke(&grammers_client::tl::functions::messages::CheckChatInvite {
                    hash: hash.to_string(),
                })
                .await;
            if let Ok(ChatInvite::Already(already)) = inv {
                if let Chat::Channel(c) = already.chat {
                    self.open_channel(
                        c.id,
                        c.access_hash.unwrap_or(0),
                        c.title,
                        if c.megagroup { "megagroup" } else { "broadcast" }.to_string(),
                    )
                    .await?;
                    self.list_channels().await?;
                    return Ok(());
                }
            }
        }
        Err(anyhow!("join request still pending"))
    }

    /// Download a file's "thumb" rendition into the local cache and tell
    /// the UI where to read it. Encrypted renditions decrypt with TDE1.
    /// Thumbnails fetch off the command loop — each is a bounded parallel
    /// task so an image-heavy folder doesn't queue N×2 sequential RTTs.
    fn spawn_thumbnail_fetch(&mut self, file_id: &str) -> Result<()> {
        let Some((body_id, r)) = self
            .namespace
            .renditions
            .get(file_id)
            .and_then(|m| m.get("thumbnail"))
            .cloned()
        else {
            return Ok(());
        };
        let dir = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("tdrive-rs/thumbs");
        std::fs::create_dir_all(&dir)?;
        let out = dir.join(format!("{body_id}.bin"));
        if out.exists() {
            self.emit(Event::ThumbnailReady {
                file_id: file_id.to_string(),
                path: out,
            });
            return Ok(());
        }
        let client = self.client()?.clone();
        let peer = self.channel()?;
        let master = self.master_key.clone();
        let sem = self.thumb_sem.clone();
        let tx = self.tx.clone();
        let file_id = file_id.to_string();
        tokio::spawn(async move {
            let _permit = sem.acquire().await;
            let key = *master.lock().unwrap();
            if let Ok(msgs) = client.get_messages_by_id(peer, &[body_id as i32]).await {
                if let Some(media) = msgs.into_iter().flatten().next().and_then(|m| m.media()) {
                    let ok = if r.encrypted {
                        match key {
                            Some(key) => {
                                let staging = dir.join(format!("{body_id}.enc"));
                                let dl = client.download_media(&media, &staging).await;
                                if dl.is_ok() {
                                    let out2 = out.clone();
                                    let staging2 = staging.clone();
                                    let dec = tokio::task::spawn_blocking(move || {
                                        crypto::decrypt_file(&key, &staging2, &out2)?;
                                        // Encrypted renditions are TDR1-
                                        // enveloped — strip to the JPEG.
                                        let data = std::fs::read(&out2)?;
                                        if data.len() > 8
                                            && &data[..4] == b"TDR1"
                                        {
                                            let n = u32::from_be_bytes(
                                                data[4..8].try_into().unwrap(),
                                            ) as usize;
                                            if 8 + n <= data.len() {
                                                std::fs::write(&out2, &data[8 + n..])?;
                                            }
                                        }
                                        Ok::<_, anyhow::Error>(())
                                    })
                                    .await;
                                    let _ = std::fs::remove_file(&staging);
                                    dec.map(|r| r.is_ok()).unwrap_or(false)
                                } else {
                                    false
                                }
                            }
                            None => false,
                        }
                    } else {
                        client.download_media(&media, &out).await.is_ok()
                    };
                    let _ = tx
                        .send(if ok {
                            Event::ThumbnailReady {
                                file_id,
                                path: out,
                            }
                        } else {
                            Event::ThumbnailFailed { file_id }
                        })
                        .await;
                }
            }
        });
        Ok(())
    }

    /// Adopt a raw channel attachment into the namespace with a `meta` op —
    /// the same op TDrive emits when claiming an unop'd document.
    async fn adopt(&mut self, msg_id: i64, parent: String) -> Result<()> {
        let (_, name, size, sender, date) = self
            .namespace
            .unadopted
            .iter()
            .find(|(id, _, _, _, _)| *id == msg_id)
            .cloned()
            .context("no adoptable attachment with that id")?;
        // TDrive's rule: only the uploader can manage a raw attachment.
        if self.actor_id != 0 && sender != self.actor_id {
            return Err(anyhow!("only the uploader can manage this file"));
        }
        let op = Op {
            op_type: OpTypeOrUnknown::Known(OpType::Meta),
            obj: wire::file_obj_id(msg_id),
            parent,
            name,
            file_size: size,
            file_upload_time: date,
            ..Default::default()
        };
        self.send_and_project(&op).await
    }

    /// Open a tokenized stream session for a file and hand the URL to the
    /// UI — TDrive's media-service model: the URL is the capability.
    async fn stream(&mut self, obj: &str) -> Result<()> {
        let Some(file) = self.namespace.files.get(obj).cloned() else {
            return Err(anyhow!("not a file"));
        };
        if file.encrypted && self.master_key.lock().unwrap().is_none() {
            return Err(anyhow!("unlock the drive to stream encrypted files"));
        }
        let parts: Vec<(i64, i64)> = if !file.upload_uuid.is_empty() {
            self.namespace.part_sizes(&file.upload_uuid)
        } else {
            vec![(file.content_msg_id, file.size)]
        };
        if parts.is_empty() {
            return Err(anyhow!("no body messages for this file"));
        }
        let client = self.client()?.clone();
        let peer = self.channel()?;
        stream::ensure_server(client, peer, self.master_key.clone());
        let url = stream::open(stream::Session {
            parts,
            plaintext_size: if file.encrypted {
                file.plaintext_size
            } else {
                file.size
            },
            name: file.name,
            encrypted: file.encrypted,
        })?;
        self.emit(Event::StreamUrl { url });
        Ok(())
    }

    /// One-shot photo backup: uploads the given images into a "Photos"
    /// folder at the drive root, creating it when missing. Renditions are
    /// generated by the normal upload path.
    async fn backup_photos(&mut self, paths: Vec<PathBuf>) -> Result<()> {
        let images: Vec<PathBuf> = paths
            .into_iter()
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .map(is_image_name)
                    .unwrap_or(false)
            })
            .collect();
        if images.is_empty() {
            return Err(anyhow!("no image files selected"));
        }
        // Reuse the existing "Photos" folder or create it.
        let folder_id = self
            .namespace
            .folders
            .values()
            .find(|d| d.parent.is_empty() && d.name == "Photos" && !d.tombstoned)
            .map(|d| d.id.clone());
        let folder_id = match folder_id {
            Some(id) => id,
            None => {
                let id = format!("d:{}", uuid::Uuid::new_v4());
                let op = Op {
                    op_type: OpTypeOrUnknown::Known(OpType::Mkdir),
                    obj: id.clone(),
                    parent: String::new(),
                    name: "Photos".to_string(),
                    ..Default::default()
                };
                self.send_and_project(&op).await?;
                id
            }
        };
        for path in images {
            let key = path.to_string_lossy().to_string();
            if self.journal.backup_seen.contains(&key) {
                continue;
            }
            if let Err(e) = self.upload(path.clone(), folder_id.clone()).await {
                self.emit(Event::Error {
                    context: "photo backup",
                    message: format!("{}: {e:#}", path.display()),
                });
            } else {
                self.journal.backup_seen.insert(key);
                self.save_journal();
            }
        }
        Ok(())
    }

    /// Poll the watched backup folder for new images (ledger-driven, like
    /// TDrive's backup lifecycle — uploaded paths are never re-sent).
    async fn backup_tick(&mut self) -> Result<()> {
        let Some(dir) = self.journal.backup_dir.clone() else {
            return Ok(());
        };
        let paths: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        self.backup_photos(paths).await
    }

    /// Mount the drive read-write at `dir` via FUSE — TDrive's mount surface
    /// for Linux file managers. Writes emit durable mount-write ops.
    async fn mount_drive(&mut self, dir: PathBuf) -> Result<()> {
        let fs = fusefs::TdFs::new(
            self.ns_shared.clone(),
            self.client()?.clone(),
            self.channel()?,
            self.master_key.clone(),
            tokio::runtime::Handle::current(),
            self.cmd_tx.clone(),
        );
        let session = fusefs::mount(fs, &dir)?;
        self.fuse_session = Some(session);
        self.emit(Event::MountState { dir: Some(dir) });
        Ok(())
    }

    /// Apply a WebDAV write through the durable mount-write ops TDrive's
    /// mount emits (dcommit/relocate/harddel/fcommit).
    async fn dav_write(&mut self, op: DavOp) -> Result<()> {
        match op {
            DavOp::Mkdir { parent, name } => {
                let op = Op {
                    op_type: OpTypeOrUnknown::Known(OpType::FolderCommit),
                    protocol_version: 1,
                    op_id: format!("dcommit-{}", uuid::Uuid::new_v4()),
                    obj: wire::new_folder_id(),
                    parent,
                    name: name.trim().to_string(),
                    file_upload_time: unix_now(),
                    ..Default::default()
                };
                self.send_and_project(&op).await
            }
            DavOp::Delete { obj } => {
                // Mounted deletes go through the durable hard-delete flow.
                let Some((_is_folder, rev, tomb, _name, _parent)) =
                    self.namespace.dirent_info(&obj)
                else {
                    return Err(anyhow!("no such object"));
                };
                if tomb {
                    return Err(anyhow!("already deleted"));
                }
                // Collect body msg ids BEFORE applying — the projection
                // drops the records so ids are empty afterwards.
                let mut bodies: Vec<i64> = if obj.starts_with('d') {
                    self.namespace.subtree_body_msg_ids(&obj)
                } else {
                    self.namespace
                        .files
                        .get(&obj)
                        .map(|f| self.namespace.body_msg_ids(f))
                        .unwrap_or_default()
                };
                if let Some(kinds) = self.namespace.renditions.get(&obj) {
                    bodies.extend(kinds.values().map(|(id, _)| *id));
                }
                let op = Op {
                    op_type: OpTypeOrUnknown::Known(OpType::HardDeleteTree),
                    protocol_version: 1,
                    op_id: wire::deterministic_op_id("purge", &obj, rev),
                    obj: obj.clone(),
                    expected_revision: rev,
                    ..Default::default()
                };
                self.send_and_project(&op).await?;
                for body in bodies {
                    let _ = self
                        .client()?
                        .delete_messages(self.channel()?, &[body as i32])
                        .await;
                }
                Ok(())
            }
            DavOp::Move {
                obj,
                dst_parent,
                name,
            } => {
                let Some((_f, rev, tomb, _n, _p)) = self.namespace.dirent_info(&obj)
                else {
                    return Err(anyhow!("no such object"));
                };
                if tomb {
                    return Err(anyhow!("deleted"));
                }
                let op = Op {
                    op_type: OpTypeOrUnknown::Known(OpType::Relocate),
                    protocol_version: 1,
                    op_id: format!("relocate-{}", uuid::Uuid::new_v4()),
                    obj,
                    parent: dst_parent,
                    name,
                    expected_revision: rev,
                    ..Default::default()
                };
                self.send_and_project(&op).await
            }
            DavOp::Put {
                parent,
                name,
                bytes,
            } => {
                // Single-body commit: upload the doc, then fcommit binds it —
                // the same two-step TDrive's writable mount performs.
                let pt_len = bytes.len() as i64;
                let encrypted = self.master_key.lock().unwrap().is_some();
                let (body, stored) = if encrypted {
                    let key = (*self.master_key.lock().unwrap()).unwrap();
                    let ct = tokio::task::spawn_blocking(move || {
                        crypto::encrypt_bytes(&key, &bytes)
                    })
                    .await??;
                    let len = ct.len() as i64;
                    (ct, len)
                } else {
                    (bytes, pt_len)
                };
                let mut reader = std::io::Cursor::new(&body);
                let uploaded = self
                    .client()?
                    .upload_stream(&mut reader, body.len(), name.clone())
                    .await?;
                let msg = self
                    .client()?
                    .send_message(
                        self.channel()?,
                        InputMessage::new().document(uploaded),
                    )
                    .await?;
                let op = Op {
                    op_type: OpTypeOrUnknown::Known(OpType::FileCommit),
                    protocol_version: 1,
                    op_id: format!("fcommit-{}", uuid::Uuid::new_v4()),
                    parent,
                    name,
                    file_size: stored,
                    file_upload_time: unix_now(),
                    encrypted,
                    plaintext_size: if encrypted { pt_len } else { 0 },
                    content_msg_id: msg.id() as i64,
                    ..Default::default()
                };
                self.send_and_project(&op).await
            }
            DavOp::Replace { obj, bytes } => {
                // freplace rebinds an existing file to fresh content —
                // TDrive's mount overwrite path.
                let Some((_f, rev, tomb, _n, _p)) = self.namespace.dirent_info(&obj)
                else {
                    return Err(anyhow!("no such object"));
                };
                if tomb {
                    return Err(anyhow!("deleted"));
                }
                let pt_len = bytes.len() as i64;
                let encrypted = self.master_key.lock().unwrap().is_some();
                let (body, stored) = if encrypted {
                    let key = (*self.master_key.lock().unwrap()).unwrap();
                    let ct = tokio::task::spawn_blocking(move || {
                        crypto::encrypt_bytes(&key, &bytes)
                    })
                    .await??;
                    let len = ct.len() as i64;
                    (ct, len)
                } else {
                    (bytes, pt_len)
                };
                let mut reader = std::io::Cursor::new(&body);
                let uploaded = self
                    .client()?
                    .upload_stream(&mut reader, body.len(), String::new())
                    .await?;
                let msg = self
                    .client()?
                    .send_message(
                        self.channel()?,
                        InputMessage::new().document(uploaded),
                    )
                    .await?;
                let op = Op {
                    op_type: OpTypeOrUnknown::Known(OpType::FileReplace),
                    protocol_version: 1,
                    retained_until: unix_now() + 30 * 24 * 60 * 60,
                    op_id: format!("freplace-{}", uuid::Uuid::new_v4()),
                    obj,
                    file_size: stored,
                    file_upload_time: unix_now(),
                    encrypted,
                    plaintext_size: if encrypted { pt_len } else { 0 },
                    content_msg_id: msg.id() as i64,
                    expected_revision: rev,
                    ..Default::default()
                };
                self.send_and_project(&op).await
            }
        }
    }

    /// Debounced incremental refresh triggered by incoming updates.
    async fn live_sync(&mut self) -> Result<()> {
        self.live_sync_queued
            .store(false, std::sync::atomic::Ordering::Relaxed);
        // A zero watermark just means empty-or-unsynced — full scan is the
        // correct response, not a permanent wedge.
        if self.channel.is_none() {
            return Ok(());
        }
        let now = std::time::Instant::now();
        if now.duration_since(self.last_live_sync) < std::time::Duration::from_millis(1500) {
            return Ok(());
        }
        self.last_live_sync = now;
        self.sync().await
    }

    async fn sign_out(&mut self) -> Result<()> {
        if let Some(client) = &self.client {
            let _ = client.sign_out().await;
        }
        // Keep the client — grammers supports re-login on the same pool;
        // dropping it strands the worker with no path back.
        self.channel = None;
        self.channel_id = 0;
        self.login_token = None;
        self.password_token = None;
        *self.master_key.lock().unwrap() = None;
        self.namespace = Arc::new(Namespace::new());
        self.emit(Event::SignedOut);
        Ok(())
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn media_size(media: &Media) -> usize {
    match media {
        Media::Document(d) => d.size().unwrap_or(0),
        Media::Photo(p) => p.size().unwrap_or(0),
        _ => 0,
    }
}

fn unique_dest(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let parent = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    for i in 1..1000 {
        let cand = parent.join(format!("{stem} ({i}){ext}"));
        if !cand.exists() {
            return cand;
        }
    }
    path
}

/// TDrive's `partAttachmentName`: cosmetic attachment name for part docs.
fn part_attachment_name(original: &str, index: i32, count: i32) -> String {
    if original.is_empty() {
        return format!("part-{index:05}");
    }
    if count <= 1 {
        return original.to_string();
    }
    format!("{original}.part{index}")
}

/// AsyncRead wrapper that reports bytes consumed upstream (throttled).
struct ProgressReader<R> {
    inner: R,
    sent: u64,
    total: u64,
    last_emit: u64,
    on_progress: Box<dyn Fn(i64, i64) + Send + Sync>,
}

impl<R: AsyncRead + Unpin> ProgressReader<R> {
    fn new(inner: R, on_progress: impl Fn(i64, i64) + Send + Sync + 'static) -> Self {
        Self {
            inner,
            sent: 0,
            total: 0,
            last_emit: 0,
            on_progress: Box::new(on_progress),
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for ProgressReader<R> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        match std::pin::Pin::new(&mut self.inner).poll_read(cx, buf) {
            std::task::Poll::Ready(Ok(())) => {
                let got = (buf.filled().len() - before) as u64;
                self.sent += got;
                // Throttle: at most one event per ~256 KiB.
                if self.sent - self.last_emit > 256 * 1024 {
                    self.last_emit = self.sent;
                    (self.on_progress)(self.sent as i64, self.total as i64);
                }
                std::task::Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

/// Extract a mid-video frame to a temp JPEG via ffmpeg (TDrive's video
/// thumbnail approach — best-effort, returns None without ffmpeg).
async fn extract_video_frame(source: &PathBuf) -> Option<PathBuf> {
    let out = std::env::temp_dir().join(format!("tdrive-vf-{}.jpg", uuid::Uuid::new_v4()));
    let src = source.clone();
    let out2 = out.clone();
    let status = tokio::task::spawn_blocking(move || {
        std::process::Command::new("ffmpeg")
            .args(["-ss", "10%", "-i"])
            .arg(&src)
            .args(["-frames:v", "1", "-q:v", "5", "-y"])
            .arg(&out2)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
    })
    .await
    .ok()?
    .ok()?;
    (status.success() && out.exists()).then_some(out)
}

fn is_video_name(name: &str) -> bool {
    matches!(
        name.rsplit('.').next().unwrap_or("").to_lowercase().as_str(),
        "mp4" | "mkv" | "webm" | "mov" | "m4v" | "avi" | "ts"
    )
}

fn is_image_name(name: &str) -> bool {
    matches!(
        name.rsplit('.').next().unwrap_or("").to_lowercase().as_str(),
        "jpg" | "jpeg" | "png" | "webp" | "gif"
    )
}
