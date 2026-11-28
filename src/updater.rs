//! GitHub Releases auto-updater for TDrive (CLI & GPUI).

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use std::path::PathBuf;
use std::process::Command;

pub const REPO: &str = "Coobyk/tdrive";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

const LATEST_RELEASE_API: &str = "https://api.github.com/repos/Coobyk/tdrive/releases/latest";
const ALLOWED_DOWNLOAD_PREFIX: &str = "https://github.com/Coobyk/tdrive/releases/download/";

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub size: u64,
    pub browser_download_url: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub version: String,
    pub tag: String,
    pub html_url: String,
    pub notes: String,
    pub asset: Option<ReleaseAsset>,
}

/// Parses a `vX.Y.Z` or `X.Y.Z` version string into `(major, minor, patch)`.
pub fn parse_semver(raw: &str) -> Option<(u64, u64, u64)> {
    let clean = raw.trim().trim_start_matches('v');
    let base = clean.split(['-', '+']).next()?;
    let mut parts = base.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_semver(latest), parse_semver(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

pub fn is_trusted_download_url(url: &str) -> bool {
    url.starts_with(ALLOWED_DOWNLOAD_PREFIX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsInstallKind {
    Nsis,
    Msi,
    Portable,
}

#[cfg(target_os = "windows")]
pub fn detect_windows_install_kind() -> WindowsInstallKind {
    let Ok(exe) = std::env::current_exe() else {
        return WindowsInstallKind::Portable;
    };
    let Some(dir) = exe.parent() else {
        return WindowsInstallKind::Portable;
    };
    if dir.join("Uninstall.exe").exists() {
        return WindowsInstallKind::Nsis;
    }
    let lower = dir.to_string_lossy().to_ascii_lowercase();
    if lower.contains("\\program files\\tdrive") {
        return WindowsInstallKind::Msi;
    }
    WindowsInstallKind::Portable
}

pub fn select_asset_for(
    assets: &[ReleaseAsset],
    os: &str,
    arch: &str,
    win_kind: WindowsInstallKind,
) -> Option<ReleaseAsset> {
    let find = |pred: &dyn Fn(&str) -> bool| -> Option<ReleaseAsset> {
        assets.iter().find(|a| pred(&a.name)).cloned()
    };

    match os {
        "windows" => match win_kind {
            WindowsInstallKind::Nsis => find(&|n| {
                n.starts_with("tdrive-windows-x86_64-setup-")
                    && n.ends_with(".exe")
                    && !n.contains("winui")
            })
            .or_else(|| {
                find(&|n| {
                    n.starts_with("tdrive-windows-x86_64-")
                        && n.ends_with(".msi")
                        && !n.contains("winui")
                })
            }),
            WindowsInstallKind::Msi => find(&|n| {
                n.starts_with("tdrive-windows-x86_64-")
                    && n.ends_with(".msi")
                    && !n.contains("winui")
            })
            .or_else(|| {
                find(&|n| {
                    n.starts_with("tdrive-windows-x86_64-setup-")
                        && n.ends_with(".exe")
                        && !n.contains("winui")
                })
            }),
            WindowsInstallKind::Portable => find(&|n| {
                n.starts_with("tdrive-windows-x86_64-setup-")
                    && n.ends_with(".exe")
                    && !n.contains("winui")
            })
            .or_else(|| {
                find(&|n| {
                    n.starts_with("tdrive-windows-x86_64-")
                        && n.ends_with(".zip")
                        && !n.contains("winui")
                })
            }),
        },
        "macos" => {
            let arch_tag = if arch == "aarch64" { "aarch64" } else { "x86_64" };
            let prefix = format!("tdrive-macos-{arch_tag}-");
            find(&|n| n.starts_with(&prefix) && n.ends_with(".dmg"))
                .or_else(|| find(&|n| n.starts_with(&prefix) && n.ends_with(".tar.gz")))
        }
        "linux" => find(&|n| n.starts_with("tdrive-linux-x86_64-") && n.ends_with(".tar.gz")),
        _ => None,
    }
}

fn select_current_asset(assets: &[ReleaseAsset]) -> Option<ReleaseAsset> {
    #[cfg(target_os = "windows")]
    let win_kind = detect_windows_install_kind();
    #[cfg(not(target_os = "windows"))]
    let win_kind = WindowsInstallKind::Portable;

    select_asset_for(assets, std::env::consts::OS, std::env::consts::ARCH, win_kind)
}

/// Checks GitHub Releases for a newer version than `CURRENT_VERSION`.
/// Returns `Ok(Some(UpdateInfo))` when a newer release exists, `Ok(None)` if up to date.
pub fn check_for_update() -> Result<Option<UpdateInfo>> {
    let output = curl_command()
        .args([
            "-fsSL",
            "--max-time",
            "15",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            &format!("User-Agent: tdrive/{CURRENT_VERSION}"),
            LATEST_RELEASE_API,
        ])
        .output()
        .context("failed to run curl for update check")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("GitHub release check failed: {}", stderr.trim());
    }

    let release: GithubRelease =
        serde_json::from_slice(&output.stdout).context("invalid GitHub release JSON")?;
    let latest_ver = release.tag_name.trim_start_matches('v').to_string();
    if !is_newer(&latest_ver, CURRENT_VERSION) {
        return Ok(None);
    }

    let asset = select_current_asset(&release.assets);
    Ok(Some(UpdateInfo {
        version: latest_ver,
        tag: release.tag_name,
        html_url: release.html_url,
        notes: release.body,
        asset,
    }))
}

/// Downloads the release installer to a temp directory, verifies its origin and size,
/// and launches it. Returns the path of the downloaded file (or opens the release URL).
pub fn download_and_launch(info: &UpdateInfo) -> Result<Option<PathBuf>> {
    let Some(asset) = &info.asset else {
        open_url(&info.html_url)?;
        return Ok(None);
    };
    if !is_trusted_download_url(&asset.browser_download_url) {
        bail!("refusing untrusted update URL: {}", asset.browser_download_url);
    }

    let dir = std::env::temp_dir().join("tdrive-update");
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(&asset.name);

    let status = curl_command()
        .args([
            "-fsSL",
            "--max-time",
            "300",
            "-H",
            &format!("User-Agent: tdrive/{CURRENT_VERSION}"),
            "-o",
        ])
        .arg(&dest)
        .arg(&asset.browser_download_url)
        .status()
        .context("failed to download update")?;

    if !status.success() {
        let _ = std::fs::remove_file(&dest);
        bail!("update download failed with status {status}");
    }

    let actual_size = std::fs::metadata(&dest)?.len();
    if asset.size > 0 && actual_size != asset.size {
        let _ = std::fs::remove_file(&dest);
        bail!(
            "downloaded update size mismatch: expected {} bytes, got {}",
            asset.size,
            actual_size
        );
    }

    launch_installer(&dest)?;
    Ok(Some(dest))
}

fn curl_command() -> Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut cmd = Command::new("curl.exe");
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    }
    #[cfg(not(target_os = "windows"))]
    {
        Command::new("curl")
    }
}

fn launch_installer(path: &std::path::Path) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if ext == "msi" {
            Command::new("msiexec.exe")
                .arg("/i")
                .arg(path)
                .arg("/passive")
                .arg("/norestart")
                .spawn()
                .context("failed to launch msiexec")?;
        } else if ext == "exe" {
            Command::new(path)
                .spawn()
                .context("failed to launch installer")?;
        } else {
            open_url(&path.to_string_lossy())?;
        }
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(path)
            .spawn()
            .context("failed to open update package")?;
        return Ok(());
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        if let Some(parent) = path.parent() {
            let _ = Command::new("xdg-open").arg(parent).spawn();
        }
        Ok(())
    }
}

pub fn open_url(url: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map_err(|e| anyhow!("failed to open {url}: {e}"))?;
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(url)
            .spawn()
            .map_err(|e| anyhow!("failed to open {url}: {e}"))?;
        return Ok(());
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map_err(|e| anyhow!("failed to open {url}: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_comparison_handles_v_prefix_and_prerelease() {
        assert!(is_newer("v0.3.10", "0.3.9"));
        assert!(is_newer("0.4.0", "0.3.9"));
        assert!(is_newer("1.0.0", "0.9.99"));
        assert!(!is_newer("v0.3.9", "0.3.9"));
        assert!(!is_newer("0.3.8", "0.3.9"));
    }

    #[test]
    fn trusted_url_validation_rejects_foreign_hosts() {
        assert!(is_trusted_download_url(
            "https://github.com/Coobyk/tdrive/releases/download/v0.3.10/tdrive-windows-x86_64-0.3.10.msi"
        ));
        assert!(!is_trusted_download_url(
            "https://evil.example.com/tdrive-windows-x86_64-0.3.10.msi"
        ));
    }

    #[test]
    fn selects_correct_gpui_asset_ignoring_winui_assets() {
        let assets = vec![
            ReleaseAsset {
                name: "tdrive-windows-x86_64-winui-setup-0.3.10.exe".into(),
                size: 50_000_000,
                browser_download_url: "https://github.com/Coobyk/tdrive/releases/download/v0.3.10/tdrive-windows-x86_64-winui-setup-0.3.10.exe".into(),
            },
            ReleaseAsset {
                name: "tdrive-windows-x86_64-setup-0.3.10.exe".into(),
                size: 12_000_000,
                browser_download_url: "https://github.com/Coobyk/tdrive/releases/download/v0.3.10/tdrive-windows-x86_64-setup-0.3.10.exe".into(),
            },
            ReleaseAsset {
                name: "tdrive-windows-x86_64-0.3.10.msi".into(),
                size: 14_000_000,
                browser_download_url: "https://github.com/Coobyk/tdrive/releases/download/v0.3.10/tdrive-windows-x86_64-0.3.10.msi".into(),
            },
            ReleaseAsset {
                name: "tdrive-macos-aarch64-0.3.10.dmg".into(),
                size: 15_000_000,
                browser_download_url: "https://github.com/Coobyk/tdrive/releases/download/v0.3.10/tdrive-macos-aarch64-0.3.10.dmg".into(),
            },
        ];

        assert_eq!(
            select_asset_for(&assets, "windows", "x86_64", WindowsInstallKind::Msi)
                .unwrap()
                .name,
            "tdrive-windows-x86_64-0.3.10.msi"
        );
        assert_eq!(
            select_asset_for(&assets, "windows", "x86_64", WindowsInstallKind::Nsis)
                .unwrap()
                .name,
            "tdrive-windows-x86_64-setup-0.3.10.exe"
        );
        assert_eq!(
            select_asset_for(&assets, "macos", "aarch64", WindowsInstallKind::Portable)
                .unwrap()
                .name,
            "tdrive-macos-aarch64-0.3.10.dmg"
        );
    }
}
