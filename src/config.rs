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
}

fn default_upload_limit() -> usize {
    3
}

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
        let path = Self::path()?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}

/// SQLite-backed grammers session file.
pub fn session_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("session.db"))
}

/// Resumable-transfer journal file.
/// Per-channel projected namespace snapshot (durable projection).
pub fn namespace_path(channel_id: i64) -> Result<PathBuf> {
    let dir = data_dir()?.join("namespaces");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{channel_id}.json")))
}

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
