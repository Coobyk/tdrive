//! Android path + config surface — replaces the desktop `config.rs`.
//! The Kotlin side passes the app's files dir at init; everything else
//! derives from it.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::OnceLock;

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn init(dir: PathBuf) -> Result<()> {
    std::fs::create_dir_all(&dir)?;
    DATA_DIR.set(dir).map_err(|_| anyhow::anyhow!("data dir already set"))
}

fn data_dir() -> Result<PathBuf> {
    DATA_DIR
        .get()
        .cloned()
        .context("tdrive core not initialised — call init() first")
}

// ── Multi-account ────────────────────────────────────────────────
// Every Telegram login owns a slot under `accounts/<id>/` holding its
// session, journal, namespace snapshots and staging. Path fns below
// resolve under the *active* slot — set once before the worker spawns
// and swapped only on SwitchAccount, after the worker has torn down
// the old session.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountMeta {
    pub id: String,
    pub label: String,
    /// Drive titles known for this account — runtime-only, filled by
    /// the worker from that account's drives.json when emitting.
    #[serde(default, skip_serializing)]
    pub drives: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Accounts {
    #[serde(default)]
    pub accounts: Vec<AccountMeta>,
    #[serde(default)]
    pub active: Option<String>,
}

pub fn accounts_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("accounts.json"))
}

pub fn accounts_dir() -> Result<PathBuf> {
    let d = data_dir()?.join("accounts");
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

pub fn account_dir(id: &str) -> Result<PathBuf> {
    let d = accounts_dir()?.join(id);
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

static ACTIVE_DIR: std::sync::RwLock<Option<PathBuf>> = std::sync::RwLock::new(None);

fn active_dir() -> Result<PathBuf> {
    ACTIVE_DIR
        .read()
        .unwrap()
        .clone()
        .context("no active account — accounts not initialised")
}

fn set_active_dir(dir: PathBuf) {
    *ACTIVE_DIR.write().unwrap() = Some(dir);
}

impl Accounts {
    pub fn load() -> Result<Self> {
        let path = accounts_path()?;
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok(serde_json::from_str(&s)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self) -> Result<()> {
        atomic_json_write(&accounts_path()?, self)
    }

    pub fn meta(&self, id: &str) -> Option<&AccountMeta> {
        self.accounts.iter().find(|a| a.id == id)
    }

    /// Point the runtime at `id`'s slot.
    pub fn activate(&mut self, id: &str) -> Result<()> {
        self.active = Some(id.to_string());
        self.save()?;
        set_active_dir(account_dir(id)?);
        Ok(())
    }
}

/// Bring the registry and ACTIVE_DIR up before the worker spawns:
/// migrate a pre-accounts data dir into `accounts/default/`, prune
/// abandoned slots (no session file ever written), and activate the
/// stored choice — falling back to the first account or a fresh slot.
pub fn init_accounts() -> Result<Accounts> {
    let mut acc = Accounts::load()?;
    let base = data_dir()?;

    // Legacy single-session layout → accounts/default/.
    if !accounts_path()?.exists()
        && [base.join("session.bin"), base.join("journal.json")]
            .iter()
            .any(|p| p.exists())
    {
        let dir = account_dir("default")?;
        for name in ["session.bin", "journal.json"] {
            let src = base.join(name);
            if src.exists() {
                let _ = std::fs::rename(&src, dir.join(name));
            }
        }
        // Namespace snapshots + staging dirs follow the session.
        if let Ok(rd) = std::fs::read_dir(&base) {
            for e in rd.flatten() {
                let n = e.file_name();
                let s = n.to_string_lossy();
                if s.starts_with("namespace-") && s.ends_with(".json") {
                    let _ = std::fs::rename(e.path(), dir.join(&*s));
                }
            }
        }
        let staging = base.join("staging");
        if staging.is_dir() {
            let _ = std::fs::rename(&staging, dir.join("staging"));
        }
        acc.accounts.push(AccountMeta {
            id: "default".into(),
            label: "Telegram".into(),
            drives: Vec::new(),
        });
        acc.active = Some("default".into());
        acc.save()?;
    }

    // Slots that never finished a login leave no session file — a crash
    // or quit mid-add would resurrect a ghost entry otherwise.
    let mut pruned = false;
    acc.accounts.retain(|a| {
        let keep = account_dir(&a.id)
            .map(|d| d.join("session.bin").exists())
            .unwrap_or(false);
        pruned |= !keep;
        keep
    });
    if pruned {
        acc.save()?;
    }

    // A fresh install still needs one slot — Connect resolves the
    // session under the active dir before any login has happened.
    if acc.accounts.is_empty() {
        acc.accounts.push(AccountMeta {
            id: "default".into(),
            label: "Telegram".into(),
            drives: Vec::new(),
        });
    }
    let active = acc
        .active
        .clone()
        .filter(|id| acc.meta(id).is_some())
        .or_else(|| acc.accounts.first().map(|a| a.id.clone()));
    if let Some(id) = active {
        acc.activate(&id)?;
    }
    Ok(acc)
}

/// Register a fresh empty slot and switch the runtime into it.
/// Label gets replaced by the signed-in user's name after login.
pub fn create_account() -> Result<AccountMeta> {
    let id = format!(
        "acct-{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let mut acc = Accounts::load()?;
    let meta = AccountMeta {
        id: id.clone(),
        label: "New account".into(),
        drives: Vec::new(),
    };
    acc.accounts.push(meta.clone());
    acc.activate(&id)?;
    Ok(meta)
}

/// Relabel the active slot after sign-in gives us the user's name.
pub fn label_active(label: &str) -> Result<()> {
    let mut acc = Accounts::load()?;
    if let Some(id) = acc.active.clone() {
        if let Some(m) = acc.accounts.iter_mut().find(|a| a.id == id) {
            m.label = label.to_string();
            acc.save()?;
        }
    }
    Ok(())
}

/// Remove `id`'s registry entry and (best-effort) its data dir.
pub fn remove_account(id: &str) -> Result<Accounts> {
    let mut acc = Accounts::load()?;
    acc.accounts.retain(|a| a.id != id);
    if acc.active.as_deref() == Some(id) {
        acc.active = acc.accounts.first().map(|a| a.id.clone());
    }
    acc.save()?;
    let _ = std::fs::remove_dir_all(accounts_dir()?.join(id));
    Ok(acc)
}

#[derive(Serialize, Deserialize, Default)]
pub struct AppConfig {
    pub upload_limit: usize,
    #[serde(default)]
    pub download_dir: Option<PathBuf>,
}

pub fn default_upload_limit() -> usize {
    3
}

pub const MAX_UPLOAD_LIMIT: usize = 16;

impl AppConfig {
    pub fn path() -> Result<PathBuf> {
        Ok(data_dir()?.join("config.json"))
    }
    pub fn load() -> Result<Option<Self>> {
        let path = Self::path()?;
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok(Some(serde_json::from_str(&s)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self) -> Result<()> {
        atomic_json_write(&Self::path()?, self)
    }
}

pub fn atomic_json_write<T: serde::Serialize>(path: &std::path::Path, v: &T) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_string(v)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn session_path() -> Result<PathBuf> {
    Ok(active_dir()?.join("session.bin"))
}

pub fn namespace_path(channel_id: i64) -> Result<PathBuf> {
    Ok(active_dir()?.join(format!("namespace-{channel_id}.json")))
}

/// Channels confirmed as drives, id → last-seen title. Titles let the
/// UI say what each login holds without a connection; they're
/// refreshed on every identification.
fn drives_path() -> Result<PathBuf> {
    Ok(active_dir()?.join("drives.json"))
}

fn load_drives_map(path: PathBuf) -> std::collections::HashMap<i64, String> {
    let Ok(s) = std::fs::read_to_string(path) else {
        return Default::default();
    };
    // Old registry was a bare id array — titles fill in on next listing.
    if let Ok(ids) = serde_json::from_str::<Vec<i64>>(&s) {
        return ids.into_iter().map(|id| (id, String::new())).collect();
    }
    serde_json::from_str(&s).unwrap_or_default()
}

pub fn known_drives() -> std::collections::HashMap<i64, String> {
    load_drives_map(drives_path().unwrap_or_default())
}

pub fn known_drives_for(account_id: &str) -> std::collections::HashMap<i64, String> {
    load_drives_map(
        account_dir(account_id)
            .unwrap_or_default()
            .join("drives.json"),
    )
}

pub fn mark_drive(channel_id: i64, title: &str) -> Result<()> {
    let mut map = known_drives();
    let dirty = match map.get(&channel_id) {
        Some(old) => !title.is_empty() && old != title,
        None => true,
    };
    if dirty {
        map.insert(channel_id, title.to_string());
        std::fs::write(drives_path()?, serde_json::to_string(&map)?)?;
    }
    Ok(())
}

/// Channels verified as NOT drives, keyed to the top message id seen
/// at check time. A channel's ops only ever arrive as new messages, so
/// an unchanged top id proves nothing new could be a drive op — the
/// remote re-check can be skipped entirely. The moment history moves,
/// the mark goes stale and the channel is checked again.
pub fn drive_check_marks() -> std::collections::HashMap<i64, i64> {
    active_dir()
        .map(|d| d.join("checked.json"))
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_drive_check_marks(marks: &std::collections::HashMap<i64, i64>) -> Result<()> {
    std::fs::write(
        active_dir()?.join("checked.json"),
        serde_json::to_string(marks)?,
    )?;
    Ok(())
}

/// A namespace snapshot only counts as drive evidence when it holds
/// entries — the old picker let users open any channel, which wrote
/// empty snapshots for plain groups and channels.
pub fn snapshot_has_entries(channel_id: i64) -> bool {
    namespace_path(channel_id)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<crate::project::NamespaceSnapshot>(&s).ok())
        .map(|s| !s.folders.is_empty() || !s.files.is_empty() || !s.trash.is_empty())
        .unwrap_or(false)
}

pub fn journal_path() -> Result<PathBuf> {
    Ok(active_dir()?.join("journal.json"))
}

pub fn staging_dir() -> Result<PathBuf> {
    let d = active_dir()?.join("staging");
    std::fs::create_dir_all(&d)?;
    Ok(d)
}
