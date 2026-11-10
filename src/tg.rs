//! Telegram worker — owns the grammers client on a dedicated tokio runtime
//! thread and translates UI commands into MTProto calls.

use crate::config;
use crate::crypto::{self, VaultConfig};
use crate::journal::{DownloadJob, Journal, PartReceipt, UploadJob};
use crate::project::Namespace;
use crate::wire::{self, Op, OpType, OpTypeOrUnknown};
use std::collections::HashMap;
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
use std::sync::Arc;
use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::mpsc;

pub const MAX_PART_BYTES: i64 = 1900 * 1024 * 1024;
pub const LARGE_FILE_MAX_BYTES: i64 = 40 * 1024 * 1024 * 1024;
pub const MAX_PARTS: usize = 32;
const DOWNLOAD_CHUNK: i32 = 1024 * 1024;

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
    OpenChannel { id: i64, access_hash: i64, title: String },
    Sync,
    Upload { path: PathBuf, parent: String },
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
    LiveSync,
    SignOut,
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
    SignedOut,
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
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(async move {
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
                master_key: None,
                cancels: HashMap::new(),
                last_live_sync: std::time::Instant::now()
                    - std::time::Duration::from_secs(10),
                journal_path: config::journal_path().expect("journal path"),
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

struct Worker {
    client: Option<Client>,
    api_hash: String,
    login_token: Option<LoginToken>,
    password_token: Option<PasswordToken>,
    channel: Option<PeerRef>,
    channel_id: i64,
    channel_title: String,
    namespace: Arc<Namespace>,
    master_key: Option<[u8; 32]>,
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
            if let Err(e) = self.handle(cmd).await {
                self.emit(Event::Error {
                    context: "telegram",
                    message: format!("{e:#}"),
                });
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
            Command::OpenChannel {
                id,
                access_hash,
                title,
            } => self.open_channel(id, access_hash, title).await,
            Command::Sync => self.sync().await,
            Command::Upload { path, parent } => self.upload(path, parent).await,
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
                self.master_key = None;
                self.emit_vault_state();
                Ok(())
            }
            Command::EnableEncryption { password, hint } => {
                self.enable_encryption(&password, &hint).await
            }
            Command::JoinInvite { link } => self.join_invite(&link).await,
            Command::LiveSync => self.live_sync().await,
            Command::SignOut => self.sign_out().await,
        }
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
        tokio::spawn(async move {
            while updates.recv().await.is_some() {
                if cmd_tx.send(Command::LiveSync).is_err() {
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
        self.list_channels().await
    }

    async fn list_channels(&mut self) -> Result<()> {
        let client = self.client()?.clone();
        let mut dialogs = client.iter_dialogs();
        let mut out = Vec::new();
        while let Some(dialog) = dialogs.next().await? {
            if let Peer::Channel(ch) = dialog.peer() {
                let kind = match ch.kind() {
                    Some(ChannelKind::Broadcast) => "broadcast",
                    Some(ChannelKind::Megagroup) => "group",
                    Some(ChannelKind::Gigagroup) => "gigagroup",
                    _ => continue,
                };
                let peer_ref = dialog.peer_ref();
                out.push(ChannelInfo {
                    id: peer_ref.id.bare_id_unchecked(),
                    access_hash: peer_ref.auth.hash(),
                    title: ch.title().to_string(),
                    kind,
                });
            }
        }
        out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
        self.emit(Event::Channels(out));
        Ok(())
    }

    async fn create_channel(&mut self, title: String) -> Result<()> {
        let client = self.client()?.clone();
        let updates = client
            .invoke(&grammers_client::tl::functions::channels::CreateChannel {
                broadcast: true,
                megagroup: false,
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
                self.open_channel(c.id, c.access_hash.unwrap_or(0), c.title)
                    .await?;
                return Ok(());
            }
        }
        Err(anyhow!("channel not found in create response"))
    }

    async fn open_channel(&mut self, id: i64, access_hash: i64, title: String) -> Result<()> {
        let peer_id = PeerId::channel(id).context("bad channel id")?;
        self.channel = Some(PeerRef {
            id: peer_id,
            auth: PeerAuth::from_hash(access_hash),
        });
        if self.channel_id != id {
            // Different drive → different vault.
            self.master_key = None;
        }
        self.channel_id = id;
        self.channel_title = title.clone();
        self.namespace = Arc::new(Namespace::new());
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
                kind: "broadcast",
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
            if let Err(e) = self.run_download(&job).await {
                self.emit(Event::Error {
                    context: "download",
                    message: format!("{e:#}"),
                });
            }
        }
        Ok(())
    }

    /// History scan of the open drive. A full scan runs when the namespace
    /// is empty; later refreshes apply only messages past the watermark.
    async fn sync(&mut self) -> Result<()> {
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
                ns.apply_message(msg.id() as i64, msg.text());
            }
        } else {
            let mut iter = client.iter_messages(peer).reverse(true);
            while let Some(msg) = iter.next().await? {
                ns.apply_message(msg.id() as i64, msg.text());
            }
        }
        if self.channel_id != 0 {
            self.journal
                .watermarks
                .insert(self.channel_id, ns.last_msg_id);
            self.save_journal();
        }
        self.namespace = Arc::new(ns);
        self.emit(Event::Namespace(self.namespace.clone()));

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

    async fn send_op(&self, header: &str) -> Result<i64> {
        let msg = self
            .client()?
            .send_message(self.channel()?, InputMessage::new().text(header))
            .await?;
        Ok(msg.id() as i64)
    }

    /// Send a control op and fold it into the local namespace.
    async fn send_and_project(&mut self, op: &Op) -> Result<()> {
        let header = wire::format(op);
        let msg_id = self.send_op(&header).await?;
        let mut ns = (*self.namespace).clone();
        ns.apply_message(msg_id, &header);
        self.namespace = Arc::new(ns);
        self.emit(Event::Namespace(self.namespace.clone()));
        Ok(())
    }

    async fn mkdir(&mut self, parent: String, name: String) -> Result<()> {
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
        // Collect body messages before the marker lands.
        let bodies = if is_folder {
            self.namespace.subtree_body_msg_ids(&obj)
        } else {
            self.namespace
                .files
                .get(&obj)
                .map(|f| self.namespace.body_msg_ids(f))
                .unwrap_or_default()
        };
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
        let jobs: Vec<String> = self
            .journal
            .uploads
            .iter()
            .filter(|j| j.channel_id == channel_id && j.manifest_msg_id.is_none())
            .map(|j| j.upload_uuid.clone())
            .collect();
        for uuid in jobs {
            if let Err(e) = self.run_upload_job(&uuid).await {
                self.emit(Event::Error {
                    context: "upload",
                    message: format!("{e:#}"),
                });
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
        if plaintext_size == 0 {
            return Err(anyhow!("empty files can't be uploaded"));
        }
        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "unnamed".into());

        // Encrypt to a staged ciphertext when the drive vault is unlocked.
        let (source, size, encrypted) = if let Some(master) = self.master_key {
            let ct_path = std::env::temp_dir().join(format!(
                "tdrive-enc-{}.bin",
                uuid::Uuid::new_v4()
            ));
            let pt = path.clone();
            let ct = ct_path.clone();
            let stored = tokio::task::spawn_blocking(move || {
                crypto::encrypt_file(&master, &pt, &ct)
            })
            .await??;
            (ct_path, stored as i64, true)
        } else {
            (path.clone(), plaintext_size, false)
        };

        if size > LARGE_FILE_MAX_BYTES {
            if encrypted {
                let _ = std::fs::remove_file(&source);
            }
            return Err(anyhow!("file exceeds the maximum upload size"));
        }

        let res = if size <= MAX_PART_BYTES {
            self.upload_single(source.clone(), name, parent, size, encrypted, plaintext_size)
                .await
        } else {
            let channel_id = self.channel()?.id.bare_id_unchecked();
            let part_count = (size + MAX_PART_BYTES - 1) / MAX_PART_BYTES;
            if part_count as usize > MAX_PARTS {
                if encrypted {
                    let _ = std::fs::remove_file(&source);
                }
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
                local_path: source.clone(),
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
            self.cancels
                .insert(format!("up:{}", job.upload_uuid), CancellationToken::new());
            self.journal.uploads.push(job.clone());
            self.save_journal();
            self.run_upload_job(&job.upload_uuid).await
        };
        if encrypted {
            let _ = std::fs::remove_file(&source);
        }
        res
    }

    async fn upload_single(&mut self, path: PathBuf, name: String, parent: String, size: i64, encrypted: bool, plaintext_size: i64) -> Result<()> {
        let id = format!("up:{}", name);
        self.cancels.insert(id.clone(), CancellationToken::new());
        self.upload_update(&id, &name, 0, size, TransferState::Active);
        let res = self.upload_single_inner(&path, &name, &parent, size, encrypted, plaintext_size).await;
        match res {
            Ok(msg_id) => {
                let mut ns = (*self.namespace).clone();
                ns.apply(msg_id, &Op {
                    op_type: OpTypeOrUnknown::Known(OpType::FileUpload),
                    parent,
                    name: name.clone(),
                    file_size: size,
                    file_upload_time: unix_now(),
                    encrypted,
                    plaintext_size: if encrypted { plaintext_size } else { 0 },
                    ..Default::default()
                });
                self.namespace = Arc::new(ns);
                self.emit(Event::Namespace(self.namespace.clone()));
                self.upload_update(&id, &name, size, size, TransferState::Done);
                Ok(())
            }
            Err(e) => {
                self.upload_update(&id, &name, 0, size, TransferState::Failed);
                Err(e)
            }
        }
    }

    async fn upload_single_inner(&self, path: &PathBuf, name: &str, parent: &str, size: i64, encrypted: bool, plaintext_size: i64) -> Result<i64> {
        let file = tokio::fs::File::open(path).await?;
        let progress = ProgressReader::new(file, {
            let tx = self.tx.clone();
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
        let uploaded = self
            .client()?
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
        let msg = self
            .client()?
            .send_message(
                self.channel()?,
                InputMessage::new().document(uploaded).text(caption),
            )
            .await?;
        Ok(msg.id() as i64)
    }

    /// Continue (or start) a multipart journal job until the manifest lands.
    async fn run_upload_job(&mut self, uuid: &str) -> Result<()> {
        let mut job = self
            .journal
            .uploads
            .iter()
            .find(|j| j.upload_uuid == uuid)
            .cloned()
            .context("upload job not found")?;

        // Encrypted jobs stage ciphertext; a restart may have lost the temp
        // file — re-encrypt from the plaintext source if needed.
        if job.encrypted && !job.local_path.exists() {
            let master = self
                .master_key
                .context("drive locked — cannot resume encrypted upload")?;
            let ct_path = std::env::temp_dir().join(format!(
                "tdrive-enc-{}.bin",
                uuid::Uuid::new_v4()
            ));
            let pt = job.source_path.clone();
            let ct = ct_path.clone();
            tokio::task::spawn_blocking(move || crypto::encrypt_file(&master, &pt, &ct))
                .await??;
            job.local_path = ct_path;
            if let Some(j) = self
                .journal
                .uploads
                .iter_mut()
                .find(|j| j.upload_uuid == uuid)
            {
                j.local_path = job.local_path.clone();
            }
            self.save_journal();
        }

        let token = self
            .cancels
            .entry(format!("up:{uuid}"))
            .or_insert_with(CancellationToken::new)
            .clone();
        let transfer_id = format!("up:{uuid}");
        let already: i64 = job
            .parts
            .iter()
            .filter(|p| p.msg_id.is_some())
            .map(|p| p.size)
            .sum();
        self.upload_update(&transfer_id, &job.name, already, job.stored_size, TransferState::Active);

        let pending: Vec<i32> = job.pending_parts();
        for idx in pending {
            if token.is_cancelled() {
                return Err(anyhow!("upload cancelled"));
            }
            let part = job.parts[idx as usize].clone();
            let offset = idx as i64 * MAX_PART_BYTES;
            let msg_id = self
                .send_part(&job, part.index, offset, part.size)
                .await?;
            if let Some(j) = self
                .journal
                .uploads
                .iter_mut()
                .find(|j| j.upload_uuid == uuid)
            {
                j.parts[idx as usize].msg_id = Some(msg_id);
            }
            self.save_journal();
            let done: i64 = self
                .journal
                .uploads
                .iter()
                .find(|j| j.upload_uuid == uuid)
                .map(|j| j.parts.iter().filter(|p| p.msg_id.is_some()).map(|p| p.size).sum())
                .unwrap_or(0);
            self.upload_update(&transfer_id, &job.name, done, job.stored_size, TransferState::Active);
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
        let msg_id = self.send_op(&header).await?;
        if let Some(j) = self
            .journal
            .uploads
            .iter_mut()
            .find(|j| j.upload_uuid == uuid)
        {
            j.manifest_msg_id = Some(msg_id);
        }
        let mut ns = (*self.namespace).clone();
        ns.apply(msg_id, &manifest);
        self.namespace = Arc::new(ns);
        self.emit(Event::Namespace(self.namespace.clone()));
        self.journal.uploads.retain(|j| j.upload_uuid != uuid);
        self.save_journal();
        self.upload_update(&transfer_id, &job.name, job.stored_size, job.stored_size, TransferState::Done);
        Ok(())
    }

    async fn send_part(&self, job: &UploadJob, index: i32, offset: i64, size: i64) -> Result<i64> {
        let file = tokio::fs::File::open(&job.local_path).await?;
        let mut limited = file;
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        limited.seek(std::io::SeekFrom::Start(offset as u64)).await?;
        let take = limited.take(size as u64);
        let progress = ProgressReader::new(take, {
            let tx = self.tx.clone();
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
        let uploaded = self
            .client()?
            .upload_stream(&mut reader, size as usize, part_name)
            .await?;
        let caption = wire::format(&Op {
            op_type: OpTypeOrUnknown::Known(OpType::FilePart),
            upload_uuid: job.upload_uuid.clone(),
            part_index: index,
            file_size: size,
            ..Default::default()
        });
        let msg = self
            .client()?
            .send_message(
                self.channel()?,
                InputMessage::new().document(uploaded).text(caption),
            )
            .await?;
        Ok(msg.id() as i64)
    }

    // ---------------- downloads ----------------

    async fn download(&mut self, file_id: String, dest_dir: PathBuf) -> Result<()> {
        let file = self
            .namespace
            .files
            .get(&file_id)
            .cloned()
            .context("file not found")?;
        if file.encrypted && self.master_key.is_none() {
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
        self.run_download(&job).await
    }

    async fn run_download(&mut self, job: &DownloadJob) -> Result<()> {
        let client = self.client()?.clone();
        let peer = self.channel()?;
        let transfer_id = format!("dl:{}", job.file_id);
        self.upload_update(&transfer_id, &job.name, job.confirmed_bytes, job.total_size, TransferState::Active);

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

        let token = self
            .cancels
            .entry(transfer_id.clone())
            .or_insert_with(CancellationToken::new)
            .clone();
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
            if let Some(j) = self
                .journal
                .downloads
                .iter_mut()
                .find(|j| j.file_id == job.file_id)
            {
                j.confirmed_bytes = written;
            }

            let mut dl = client.iter_download(&media).chunk_size(DOWNLOAD_CHUNK);
            if skip > 0 {
                dl = dl.skip_chunks(skip);
            }
            while let Some(chunk) = dl.next().await? {
                if token.is_cancelled() {
                    return Err(anyhow!("download cancelled"));
                }
                out.write_all(&chunk).await?;
                written += chunk.len() as i64;
                out.sync_data().await?;
                if let Some(j) = self
                    .journal
                    .downloads
                    .iter_mut()
                    .find(|j| j.file_id == job.file_id)
                {
                    j.confirmed_bytes = written;
                }
                self.save_journal();
                self.upload_update(&transfer_id, &job.name, written, job.total_size, TransferState::Active);
            }
        }
        drop(out);
        if job.encrypted {
            let master = self.master_key.context("drive locked mid-download")?;
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
        if let Some(j) = self
            .journal
            .downloads
            .iter_mut()
            .find(|j| j.file_id == job.file_id)
        {
            j.done = true;
        }
        self.journal
            .downloads
            .retain(|j| j.file_id != job.file_id || !j.done);
        self.save_journal();
        self.upload_update(&transfer_id, &job.name, written, job.total_size, TransferState::Done);
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
            unlocked: self.master_key.is_some(),
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
        self.master_key = Some(key);
        self.emit_vault_state();
        Ok(())
    }

    async fn enable_encryption(&mut self, password: &str, hint: &str) -> Result<()> {
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
        self.master_key = Some(master);
        self.emit_vault_state();
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
                hash,
            })
            .await?;
        use grammers_client::tl::enums::{Updates, messages::ChatInviteJoinResult};
        let ChatInviteJoinResult::Ok(ok) = result else {
            return Err(anyhow!("invite requires an interactive join"));
        };
        let chats = match ok.updates {
            Updates::Combined(u) => u.chats,
            Updates::Updates(u) => u.chats,
            _ => return Err(anyhow!("unexpected updates response")),
        };
        for chat in chats {
            if let grammers_client::tl::enums::Chat::Channel(c) = chat {
                self.open_channel(c.id, c.access_hash.unwrap_or(0), c.title)
                    .await?;
                self.list_channels().await?;
                return Ok(());
            }
        }
        Err(anyhow!("invite did not resolve to a channel"))
    }

    /// Debounced incremental refresh triggered by incoming updates.
    async fn live_sync(&mut self) -> Result<()> {
        if self.channel.is_none() || self.namespace.last_msg_id == 0 {
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
        self.client = None;
        self.channel = None;
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
