//! App configuration and data-directory plumbing.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub api_id: i32,
    pub api_hash: String,
    /// Channel id of the last drive opened, reopened on next launch.
    #[serde(default)]
    pub last_channel_id: Option<i64>,
    /// Last folder id used as an upload/download destination.
    #[serde(default)]
    pub download_dir: Option<PathBuf>,
    /// Max concurrent upload tasks.
    #[serde(default = "default_upload_limit")]
    pub upload_limit: usize,
    /// Last file-browser view mode — false list, true grid.
    #[serde(default)]
    pub grid_view: bool,
}

/// TDrive's upload concurrency bounds — 3 concurrent, 16 max.
pub fn default_upload_limit() -> usize {
    3
}

pub const MAX_UPLOAD_LIMIT: usize = 16;

fn data_dir() -> Result<PathBuf> {
    let dir = dirs::data_dir()
        .or_else(dirs::config_dir)
        .context("no data directory")?
        .join("tdrive-rs");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

impl AppConfig {
    pub fn path() -> Result<PathBuf> {
        Ok(data_dir()?.join("config.json"))
    }

    pub fn load() -> Result<Option<Self>> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path)?;
        Ok(Some(serde_json::from_str(&raw)?))
    }

    pub fn save(&self) -> Result<()> {
        atomic_json_write(&Self::path()?, self)
    }
}

/// Write `v` as pretty JSON via a sibling `.tmp` + rename — a torn
/// write can never leave a half-file at `path` (POSIX rename is atomic).
pub fn atomic_json_write<T: serde::Serialize>(path: &std::path::Path, v: &T) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(v)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// SQLite-backed grammers session file.
pub fn session_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("session.db"))
}

/// Local folder cosmetics (color/badge) — kept out of the remote
/// namespace so styling never leaks into TDrive metadata.
pub fn styles_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("folder_styles.json"))
}

/// Held for the process lifetime — a second instance can't start.
/// Two apps sharing one session.db produced AUTH_KEY_DUPLICATED.
pub struct InstanceLock(std::fs::File);

pub fn instance_lock() -> Result<InstanceLock> {
    let path = data_dir()?.join("tdrive-rs.lock");
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(path)?;
    f.try_lock()
        .context("another tdrive-rs instance is already running")?;
    Ok(InstanceLock(f))
}

/// Per-channel projected namespace snapshot (durable projection).
pub fn namespace_path(channel_id: i64) -> Result<PathBuf> {
    let dir = data_dir()?.join("namespaces");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{channel_id}.json")))
}

/// Resumable-transfer journal file.
pub fn journal_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("journal.json"))
}

/// Whole-file ciphertext staging — data_dir, never tmpfs (a 40 GiB
/// staged ciphertext would live in RAM under /tmp).
pub fn staging_dir() -> Result<PathBuf> {
    let d = data_dir()?.join("staging");
    std::fs::create_dir_all(&d)?;
    Ok(d)
}
