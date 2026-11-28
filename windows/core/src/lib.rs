//! Windows native FFI bridge (`tdrive_core.dll`) for the WinUI 3 frontend.
//!
//! Reuses the desktop Rust engine (`tdrive::tg`, `tdrive::config`, `tdrive::project`)
//! directly with zero duplicated protocol, crypto, or sync logic, exposing a thin
//! JSON-in / JSON-out C ABI.

use std::ffi::{CStr, CString, c_char, c_int};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tdrive::config::{self, AppConfig, InstanceLock};
use tdrive::project::{FileEntry, FolderEntry, Namespace, TrashEntry, UnadoptedEntry};
use tdrive::tg::{self, ChannelInfo, Command, DriveKind, Event, TransferState};

struct CoreRuntime {
    tx: tokio::sync::mpsc::UnboundedSender<Command>,
    rx: Mutex<mpsc::Receiver<Event>>,
    _lock: Mutex<Option<InstanceLock>>,
}

static RUNTIME: OnceLock<CoreRuntime> = OnceLock::new();
static VERSION_CSTR: OnceLock<CString> = OnceLock::new();

fn into_c_string(s: String) -> *mut c_char {
    let sanitized = s.replace('\0', " ");
    CString::new(sanitized)
        .unwrap_or_else(|_| CString::new("{\"ok\":false,\"err\":\"invalid string\"}").unwrap())
        .into_raw()
}

fn ok_json() -> *mut c_char {
    into_c_string("{\"ok\":true}".to_string())
}

fn err_json(msg: impl std::fmt::Display) -> *mut c_char {
    into_c_string(serde_json::json!({ "ok": false, "err": msg.to_string() }).to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriveKindDto {
    Broadcast,
    Megagroup,
}

impl From<DriveKind> for DriveKindDto {
    fn from(k: DriveKind) -> Self {
        match k {
            DriveKind::Personal => Self::Broadcast,
            DriveKind::Shared => Self::Megagroup,
        }
    }
}

impl From<DriveKindDto> for DriveKind {
    fn from(k: DriveKindDto) -> Self {
        match k {
            DriveKindDto::Broadcast => DriveKind::Personal,
            DriveKindDto::Megagroup => DriveKind::Shared,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelDto {
    pub id: i64,
    pub access_hash: i64,
    pub title: String,
    pub kind: DriveKindDto,
    pub is_personal: bool,
}

impl From<ChannelInfo> for ChannelDto {
    fn from(c: ChannelInfo) -> Self {
        Self {
            id: c.id,
            access_hash: c.access_hash,
            title: c.title,
            is_personal: c.kind.is_personal(),
            kind: c.kind.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountDto {
    pub id: String,
    pub label: String,
    pub drives: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderDto {
    pub id: String,
    pub name: String,
    pub parent: String,
    pub tombstoned: bool,
    pub revision: i64,
}

impl From<&FolderEntry> for FolderDto {
    fn from(f: &FolderEntry) -> Self {
        Self {
            id: f.id.clone(),
            name: f.name.clone(),
            parent: f.parent.clone(),
            tombstoned: f.tombstoned,
            revision: f.revision,
        }
    }
}

fn is_image_ext(name: &str) -> bool {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "ico"
    )
}

fn is_video_ext(name: &str) -> bool {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(ext.as_str(), "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v")
}

#[derive(Debug, Clone, Serialize)]
pub struct FileDto {
    pub id: String,
    pub msg_id: i64,
    pub name: String,
    pub size: i64,
    pub logical_size: i64,
    pub parent: String,
    pub upload_time: i64,
    pub tombstoned: bool,
    pub encrypted: bool,
    pub part_count: i32,
    pub revision: i64,
    pub has_thumb: bool,
    pub has_preview: bool,
    pub is_image: bool,
    pub is_video: bool,
}

impl FileDto {
    fn from_entry(f: &FileEntry, ns: &Namespace) -> Self {
        let rend = ns.renditions.get(&f.id);
        let has_thumb = rend.is_some_and(|m| m.contains_key("thumb"));
        let has_preview = rend.is_some_and(|m| m.contains_key("preview"));
        Self {
            id: f.id.clone(),
            msg_id: f.msg_id,
            name: f.name.clone(),
            size: f.size,
            logical_size: f.logical_size(),
            parent: f.parent.clone(),
            upload_time: f.upload_time,
            tombstoned: f.tombstoned,
            encrypted: f.encrypted,
            part_count: f.part_count,
            revision: f.revision,
            has_thumb,
            has_preview,
            is_image: is_image_ext(&f.name),
            is_video: is_video_ext(&f.name),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TrashDto {
    pub object_id: String,
    pub is_folder: bool,
    pub original_parent: String,
    pub original_name: String,
    pub deleted_at: i64,
    pub purge_after: i64,
}

impl From<&TrashEntry> for TrashDto {
    fn from(t: &TrashEntry) -> Self {
        Self {
            object_id: t.object_id.clone(),
            is_folder: t.is_folder,
            original_parent: t.original_parent.clone(),
            original_name: t.original_name.clone(),
            deleted_at: t.deleted_at,
            purge_after: t.purge_after,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct UnadoptedDto {
    pub msg_id: i64,
    pub name: String,
    pub size: i64,
    pub sender_id: i64,
    pub date: i64,
}

impl From<&UnadoptedEntry> for UnadoptedDto {
    fn from(u: &UnadoptedEntry) -> Self {
        Self {
            msg_id: u.msg_id,
            name: u.name.clone(),
            size: u.size,
            sender_id: u.sender_id,
            date: u.date,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd")]
pub enum CommandDto {
    Connect {
        api_id: i32,
        api_hash: String,
    },
    RequestCode {
        phone: String,
    },
    SubmitCode {
        code: String,
    },
    SubmitPassword {
        password: String,
    },
    ListChannels,
    CreateChannel {
        title: String,
    },
    CreateSharedChannel {
        title: String,
    },
    EmptyTrash,
    OptimizeDrive,
    OptimizeApply,
    OpenChannel {
        id: i64,
        access_hash: i64,
        title: String,
        kind: DriveKindDto,
    },
    Sync,
    LiveSync,
    Upload {
        path: String,
        parent: String,
    },
    UploadItems {
        items: Vec<String>,
        parent: String,
        #[serde(default)]
        wrap: Option<String>,
    },
    UploadArchive {
        items: Vec<String>,
        name: String,
        parent: String,
        #[serde(default)]
        password: Option<String>,
    },
    SetUploadLimit {
        n: usize,
    },
    ResumeUploads,
    Download {
        file_id: String,
        dest: String,
    },
    Mkdir {
        parent: String,
        name: String,
    },
    MkdirId {
        id: String,
        parent: String,
        name: String,
    },
    Rename {
        obj: String,
        name: String,
    },
    Move {
        obj: String,
        parent: String,
    },
    Copy {
        obj: String,
        parent: String,
    },
    Delete {
        obj: String,
    },
    Restore {
        obj: String,
    },
    Purge {
        obj: String,
    },
    Cancel {
        id: String,
    },
    Pause {
        id: String,
    },
    Resume {
        id: String,
    },
    Unlock {
        password: String,
    },
    Lock,
    EnableEncryption {
        password: String,
        #[serde(default)]
        hint: String,
    },
    JoinInvite {
        link: String,
    },
    FetchThumbnail {
        file_id: String,
    },
    FetchPreview {
        file_id: String,
    },
    Adopt {
        msg_id: i64,
        parent: String,
    },
    Stream {
        obj: String,
    },
    BackupPhotos {
        paths: Vec<String>,
    },
    WatchBackup {
        dir: String,
    },
    AddSyncPair {
        local: String,
        remote: String,
    },
    SignOut,
    ListAccounts,
    SwitchAccount {
        id: String,
    },
    AddAccount,
    RemoveAccount {
        id: String,
    },
}

impl From<CommandDto> for Command {
    fn from(dto: CommandDto) -> Self {
        match dto {
            CommandDto::Connect { api_id, api_hash } => Command::Connect { api_id, api_hash },
            CommandDto::RequestCode { phone } => Command::RequestCode { phone },
            CommandDto::SubmitCode { code } => Command::SubmitCode { code },
            CommandDto::SubmitPassword { password } => Command::SubmitPassword { password },
            CommandDto::ListChannels => Command::ListChannels,
            CommandDto::CreateChannel { title } => Command::CreateChannel { title },
            CommandDto::CreateSharedChannel { title } => Command::CreateSharedChannel { title },
            CommandDto::EmptyTrash => Command::EmptyTrash,
            CommandDto::OptimizeDrive => Command::OptimizeDrive,
            CommandDto::OptimizeApply => Command::OptimizeApply,
            CommandDto::OpenChannel {
                id,
                access_hash,
                title,
                kind,
            } => Command::OpenChannel {
                id,
                access_hash,
                title,
                kind: kind.into(),
            },
            CommandDto::Sync => Command::Sync,
            CommandDto::LiveSync => Command::LiveSync,
            CommandDto::Upload { path, parent } => Command::Upload {
                path: PathBuf::from(path),
                parent,
            },
            CommandDto::UploadItems {
                items,
                parent,
                wrap,
            } => Command::UploadItems {
                items: items.into_iter().map(PathBuf::from).collect(),
                parent,
                wrap,
            },
            CommandDto::UploadArchive {
                items,
                name,
                parent,
                password,
            } => Command::UploadArchive {
                items: items.into_iter().map(PathBuf::from).collect(),
                name,
                parent,
                password,
            },
            CommandDto::SetUploadLimit { n } => Command::SetUploadLimit { n },
            CommandDto::ResumeUploads => Command::ResumeUploads,
            CommandDto::Download { file_id, dest } => Command::Download {
                file_id,
                dest: PathBuf::from(dest),
            },
            CommandDto::Mkdir { parent, name } => Command::Mkdir { parent, name },
            CommandDto::MkdirId { id, parent, name } => Command::MkdirId { id, parent, name },
            CommandDto::Rename { obj, name } => Command::Rename { obj, name },
            CommandDto::Move { obj, parent } => Command::Move { obj, parent },
            CommandDto::Copy { obj, parent } => Command::Copy { obj, parent },
            CommandDto::Delete { obj } => Command::Delete { obj },
            CommandDto::Restore { obj } => Command::Restore { obj },
            CommandDto::Purge { obj } => Command::Purge { obj },
            CommandDto::Cancel { id } => Command::Cancel { id },
            CommandDto::Pause { id } => Command::Pause { id },
            CommandDto::Resume { id } => Command::Resume { id },
            CommandDto::Unlock { password } => Command::Unlock { password },
            CommandDto::Lock => Command::Lock,
            CommandDto::EnableEncryption { password, hint } => {
                Command::EnableEncryption { password, hint }
            }
            CommandDto::JoinInvite { link } => Command::JoinInvite { link },
            CommandDto::FetchThumbnail { file_id } => Command::FetchThumbnail { file_id },
            CommandDto::FetchPreview { file_id } => Command::FetchPreview { file_id },
            CommandDto::Adopt { msg_id, parent } => Command::Adopt { msg_id, parent },
            CommandDto::Stream { obj } => Command::Stream { obj },
            CommandDto::BackupPhotos { paths } => Command::BackupPhotos {
                paths: paths.into_iter().map(PathBuf::from).collect(),
            },
            CommandDto::WatchBackup { dir } => Command::WatchBackup {
                dir: PathBuf::from(dir),
            },
            CommandDto::AddSyncPair { local, remote } => Command::AddSyncPair {
                local: PathBuf::from(local),
                remote,
            },
            CommandDto::SignOut => Command::SignOut,
            CommandDto::ListAccounts => Command::ListAccounts,
            CommandDto::SwitchAccount { id } => Command::SwitchAccount { id },
            CommandDto::AddAccount => Command::AddAccount,
            CommandDto::RemoveAccount { id } => Command::RemoveAccount { id },
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "event")]
pub enum EventDto {
    Connected {
        authorized: bool,
    },
    CodeSent,
    NeedPassword {
        hint: String,
    },
    SignedIn {
        name: String,
    },
    Channels {
        channels: Vec<ChannelDto>,
    },
    ChannelOpened {
        info: ChannelDto,
        pending_uploads: usize,
    },
    Namespace {
        folders: Vec<FolderDto>,
        files: Vec<FileDto>,
        trash: Vec<TrashDto>,
        unadopted: Vec<UnadoptedDto>,
        has_vault: bool,
    },
    Transfer {
        id: String,
        label: String,
        sent: i64,
        total: i64,
        state: &'static str,
    },
    Error {
        context: &'static str,
        message: String,
    },
    VaultState {
        configured: bool,
        unlocked: bool,
        hint: String,
    },
    InviteLink {
        link: String,
    },
    ThumbnailReady {
        file_id: String,
        path: String,
    },
    ThumbnailFailed {
        file_id: String,
    },
    PreviewReady {
        file_id: String,
        path: String,
    },
    PreviewFailed {
        file_id: String,
    },
    SyncState {
        active: bool,
    },
    Status {
        message: String,
    },
    OptimizePlan {
        dead: usize,
        scanned: usize,
    },
    StreamUrl {
        url: String,
    },
    DavUrl {
        url: String,
    },
    MountState {
        dir: Option<String>,
    },
    Accounts {
        accounts: Vec<AccountDto>,
        active: Option<String>,
    },
    SignedOut,
}

impl From<Event> for EventDto {
    fn from(ev: Event) -> Self {
        match ev {
            Event::Connected { authorized } => Self::Connected { authorized },
            Event::CodeSent => Self::CodeSent,
            Event::NeedPassword { hint } => Self::NeedPassword { hint },
            Event::SignedIn { name } => Self::SignedIn { name },
            Event::Channels(channels) => Self::Channels {
                channels: channels.into_iter().map(ChannelDto::from).collect(),
            },
            Event::ChannelOpened {
                info,
                pending_uploads,
            } => Self::ChannelOpened {
                info: info.into(),
                pending_uploads,
            },
            Event::Namespace(ns) => {
                let mut folders: Vec<FolderDto> = ns.folders.values().map(FolderDto::from).collect();
                folders.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                let mut files: Vec<FileDto> = ns
                    .files
                    .values()
                    .map(|f| FileDto::from_entry(f, &ns))
                    .collect();
                files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                let mut trash: Vec<TrashDto> = ns.trash.values().map(TrashDto::from).collect();
                trash.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
                let unadopted: Vec<UnadoptedDto> =
                    ns.unadopted.iter().map(UnadoptedDto::from).collect();
                Self::Namespace {
                    folders,
                    files,
                    trash,
                    unadopted,
                    has_vault: ns.encfg.is_some(),
                }
            }
            Event::Transfer(t) => Self::Transfer {
                id: t.id,
                label: t.label,
                sent: t.sent,
                total: t.total,
                state: match t.state {
                    TransferState::Active => "active",
                    TransferState::Paused => "paused",
                    TransferState::Done => "done",
                    TransferState::Failed => "failed",
                },
            },
            Event::Error { context, message } => Self::Error { context, message },
            Event::VaultState {
                configured,
                unlocked,
                hint,
            } => Self::VaultState {
                configured,
                unlocked,
                hint,
            },
            Event::InviteLink { link } => Self::InviteLink { link },
            Event::ThumbnailReady { file_id, path } => Self::ThumbnailReady {
                file_id,
                path: path.to_string_lossy().into_owned(),
            },
            Event::ThumbnailFailed { file_id } => Self::ThumbnailFailed { file_id },
            Event::PreviewReady { file_id, path } => Self::PreviewReady {
                file_id,
                path: path.to_string_lossy().into_owned(),
            },
            Event::PreviewFailed { file_id } => Self::PreviewFailed { file_id },
            Event::SyncState { active } => Self::SyncState { active },
            Event::Status { message } => Self::Status { message },
            Event::OptimizePlan { dead, scanned } => Self::OptimizePlan { dead, scanned },
            Event::StreamUrl { url } => Self::StreamUrl { url },
            Event::DavUrl { url } => Self::DavUrl { url },
            Event::MountState { dir } => Self::MountState {
                dir: dir.map(|p| p.to_string_lossy().into_owned()),
            },
            Event::Accounts { accounts, active } => Self::Accounts {
                accounts: accounts
                    .into_iter()
                    .map(|a| AccountDto {
                        id: a.id,
                        label: a.label,
                        drives: a.drives,
                    })
                    .collect(),
                active,
            },
            Event::SignedOut => Self::SignedOut,
        }
    }
}

/// Returns the static null-terminated version string of `tdrive_core`.
#[unsafe(no_mangle)]
pub extern "C" fn tdrive_core_version() -> *const c_char {
    VERSION_CSTR
        .get_or_init(|| CString::new(env!("CARGO_PKG_VERSION")).unwrap())
        .as_ptr()
}

/// Loads `AppConfig` as `{"ok":true,"config":{...}|null}`. Caller frees with `tdrive_core_free_string`.
#[unsafe(no_mangle)]
pub extern "C" fn tdrive_core_load_config() -> *mut c_char {
    match AppConfig::load() {
        Ok(cfg) => into_c_string(serde_json::json!({ "ok": true, "config": cfg }).to_string()),
        Err(e) => err_json(e),
    }
}

/// Saves `AppConfig` JSON atomically. Caller frees the returned status JSON with `tdrive_core_free_string`.
///
/// # Safety
/// `json` must be a valid null-terminated UTF-8 C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tdrive_core_save_config(json: *const c_char) -> *mut c_char {
    if json.is_null() {
        return err_json("null config json");
    }
    let text = match unsafe { CStr::from_ptr(json) }.to_str() {
        Ok(s) => s,
        Err(e) => return err_json(e),
    };
    let cfg: AppConfig = match serde_json::from_str(text) {
        Ok(c) => c,
        Err(e) => return err_json(e),
    };
    match cfg.save() {
        Ok(()) => ok_json(),
        Err(e) => err_json(e),
    }
}

/// Acquires the single-instance lock and spawns the Telegram worker thread. Idempotent.
#[unsafe(no_mangle)]
pub extern "C" fn tdrive_core_spawn() -> *mut c_char {
    if RUNTIME.get().is_some() {
        return ok_json();
    }
    let lock = match config::instance_lock() {
        Ok(l) => l,
        Err(e) => return err_json(e),
    };
    let handle = tg::spawn();
    let (std_tx, std_rx) = mpsc::channel::<Event>();
    let async_rx = handle.rx;
    std::thread::spawn(move || {
        while let Ok(ev) = async_rx.recv_blocking() {
            if std_tx.send(ev).is_err() {
                break;
            }
        }
    });
    let _ = RUNTIME.set(CoreRuntime {
        tx: handle.tx,
        rx: Mutex::new(std_rx),
        _lock: Mutex::new(Some(lock)),
    });
    ok_json()
}

/// Dispatches a JSON-encoded command to the worker. Caller frees the returned JSON string.
///
/// # Safety
/// `json` must be a valid null-terminated UTF-8 C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tdrive_core_dispatch(json: *const c_char) -> *mut c_char {
    let Some(rt) = RUNTIME.get() else {
        return err_json("worker not running");
    };
    if json.is_null() {
        return err_json("null command json");
    }
    let text = match unsafe { CStr::from_ptr(json) }.to_str() {
        Ok(s) => s,
        Err(e) => return err_json(e),
    };
    let dto: CommandDto = match serde_json::from_str(text) {
        Ok(c) => c,
        Err(e) => return err_json(format!("invalid command: {e}")),
    };
    match rt.tx.send(dto.into()) {
        Ok(()) => ok_json(),
        Err(_) => err_json("worker channel closed"),
    }
}

/// Waits up to `timeout_ms` for the next event and returns it as JSON, or `null` on timeout.
/// Caller frees non-null pointers with `tdrive_core_free_string`.
#[unsafe(no_mangle)]
pub extern "C" fn tdrive_core_poll(timeout_ms: c_int) -> *mut c_char {
    let Some(rt) = RUNTIME.get() else {
        return std::ptr::null_mut();
    };
    let rx = match rt.rx.lock() {
        Ok(g) => g,
        Err(_) => return std::ptr::null_mut(),
    };
    let ev = if timeout_ms <= 0 {
        rx.try_recv().ok()
    } else {
        rx.recv_timeout(Duration::from_millis(timeout_ms as u64)).ok()
    };
    match ev {
        Some(e) => {
            let dto = EventDto::from(e);
            match serde_json::to_string(&dto) {
                Ok(s) => into_c_string(s),
                Err(err) => err_json(err),
            }
        }
        None => std::ptr::null_mut(),
    }
}

/// Releases the instance lock on clean application shutdown.
#[unsafe(no_mangle)]
pub extern "C" fn tdrive_core_shutdown() {
    if let Some(rt) = RUNTIME.get() {
        if let Ok(mut guard) = rt._lock.lock() {
            guard.take();
        }
    }
}

/// Frees a heap-allocated C string previously returned by `tdrive_core_*`.
///
/// # Safety
/// `ptr` must be null or a pointer returned by a `tdrive_core_*` function in this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tdrive_core_free_string(ptr: *mut c_char) {
    if !ptr.is_null() {
        unsafe {
            let _ = CString::from_raw(ptr);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_dto_deserializes_core_actions() {
        let cases = [
            r#"{"cmd":"Connect","api_id":12345,"api_hash":"abcdef"}"#,
            r#"{"cmd":"RequestCode","phone":"+15551234567"}"#,
            r#"{"cmd":"SubmitCode","code":"12345"}"#,
            r#"{"cmd":"SubmitPassword","password":"secret"}"#,
            r#"{"cmd":"ListChannels"}"#,
            r#"{"cmd":"OpenChannel","id":42,"access_hash":99,"title":"Drive","kind":"broadcast"}"#,
            r#"{"cmd":"Mkdir","parent":"","name":"Docs"}"#,
            r#"{"cmd":"UploadItems","items":["C:\\a.txt"],"parent":"","wrap":null}"#,
            r#"{"cmd":"EnableEncryption","password":"pw","hint":"h"}"#,
            r#"{"cmd":"ListAccounts"}"#,
        ];
        for raw in cases {
            let dto: CommandDto = serde_json::from_str(raw).expect(raw);
            let _cmd: Command = dto.into();
        }
    }

    #[test]
    fn event_dto_serializes_with_event_tag() {
        let ev = EventDto::from(Event::ChannelOpened {
            info: ChannelInfo {
                id: 7,
                access_hash: 8,
                title: "My Drive".into(),
                kind: DriveKind::Personal,
            },
            pending_uploads: 2,
        });
        let v: serde_json::Value = serde_json::to_value(ev).unwrap();
        assert_eq!(v["event"], "ChannelOpened");
        assert_eq!(v["info"]["kind"], "broadcast");
        assert_eq!(v["info"]["is_personal"], true);
        assert_eq!(v["pending_uploads"], 2);
    }

    #[test]
    fn version_ffi_returns_non_empty_utf8() {
        let ptr = tdrive_core_version();
        assert!(!ptr.is_null());
        let s = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap();
        assert!(!s.is_empty());
    }
}
