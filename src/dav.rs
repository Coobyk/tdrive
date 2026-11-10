//! Read-only WebDAV surface — TDrive mounts drives through the OS; we expose
//! the same browsing contract over `webdav://127.0.0.1:<port>/dav/` so file
//! managers can mount a drive without a kernel FS. PROPFIND lists the
//! namespace; GET/HEAD serve plaintext ranges via the shared TDE1-aware
//! reader in `stream.rs`. Write methods are not implemented — the mount is
//! read-only by design for now.

use crate::project::{Entry, Namespace};
use crate::stream;
use anyhow::{Context, Result};
use grammers_client::session::types::PeerRef;
use grammers_client::Client;
use std::sync::{Arc, Mutex, OnceLock};

static PORT: OnceLock<u16> = OnceLock::new();
static STARTED: OnceLock<()> = OnceLock::new();

/// Spawn the DAV listener once inside the worker's runtime.
pub fn ensure_server(
    client: Client,
    peer: PeerRef,
    ns: Arc<Mutex<Arc<Namespace>>>,
    master: Arc<Mutex<Option<[u8; crate::crypto::MASTER_KEY_LEN]>>>,
) {
    if STARTED.set(()).is_err() {
        return;
    }
    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let _ = PORT.set(listener.local_addr()?.port());
        loop {
            let (sock, _) = listener.accept().await?;
            tokio::spawn(handle(
                sock,
                client.clone(),
                peer.clone(),
                ns.clone(),
                master.clone(),
            ));
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    });
}

pub fn url() -> Option<String> {
    PORT.get()
        .map(|p| format!("webdav://127.0.0.1:{p}/dav/"))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Resolve a DAV path to a folder id (root = "") or error.
fn resolve_dir(ns: &Namespace, path: &str) -> Result<String> {
    let mut cur = String::new();
    for seg in path.trim_matches('/').split('/').filter(|s| !s.is_empty()) {
        let seg = urlencoding::decode(seg).unwrap_or_default().to_string();
        let Some(next) = ns.folders.values().find(|d| {
            !d.tombstoned && d.parent == cur && d.name == seg
        }) else {
            anyhow::bail!("no such folder");
        };
        cur = next.id.clone();
    }
    Ok(cur)
}

fn resolve_file<'a>(ns: &'a Namespace, path: &str) -> Result<&'a crate::project::FileEntry> {
    let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
    let parent = resolve_dir(ns, dir)?;
    let name = urlencoding::decode(name).unwrap_or_default().to_string();
    ns.files
        .values()
        .find(|f| !f.tombstoned && f.parent == parent && f.name == name)
        .context("no such file")
}

fn propfind(ns: &Namespace, path: &str, depth: &str) -> String {
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="utf-8"?><D:multistatus xmlns:D="DAV:">"#,
    );
    let mut emit = |href: &str, is_dir: bool, size: i64| {
        let ct = if is_dir {
            "<D:getcontenttype>application/x-directory</D:getcontenttype>"
        } else {
            ""
        };
        let rt = if is_dir { "<D:collection/>" } else { "" };
        xml.push_str(&format!(
            "<D:response><D:href>{href}</D:href><D:propstat><D:prop>\
             <D:resourcetype>{rt}</D:resourcetype>{ct}\
             <D:getcontentlength>{size}</D:getcontentlength>\
             </D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"
        ));
    };
    let base = format!("/dav{path}");
    if let Ok(file) = resolve_file(ns, path) {
        emit(&escape(&base), false, file.plaintext_size.max(file.size));
        return format!("{xml}</D:multistatus>");
    }
    let Ok(dir) = resolve_dir(ns, path) else {
        return format!("{xml}</D:multistatus>");
    };
    emit(&escape(&format!("{base}/")), true, 0);
    if depth == "1" {
        for e in ns.list(&dir) {
            match e {
                Entry::Folder(d) => emit(
                    &escape(&format!("{base}/{}/", d.name)),
                    true,
                    0,
                ),
                Entry::File(f) => emit(
                    &escape(&format!("{base}/{}", f.name)),
                    false,
                    f.plaintext_size.max(f.size),
                ),
            }
        }
    }
    format!("{xml}</D:multistatus>")
}

async fn handle(
    mut sock: tokio::net::TcpStream,
    client: Client,
    peer: PeerRef,
    ns: Arc<Mutex<Arc<Namespace>>>,
    master: Arc<Mutex<Option<[u8; crate::crypto::MASTER_KEY_LEN]>>>,
) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; 16384];
    let n = sock.read(&mut buf).await?;
    let req = String::from_utf8_lossy(&buf[..n]);
    let mut lines = req.lines();
    let first = lines.next().unwrap_or("");
    let mut it = first.split_whitespace();
    let method = it.next().unwrap_or("");
    let raw_path = it.next().unwrap_or("/");
    let path = raw_path.strip_prefix("/dav").unwrap_or("/").to_string();
    let mut depth = "0".to_string();
    let mut range = None;
    for l in lines {
        if let Some(v) = l.strip_prefix("Depth:") {
            depth = v.trim().to_string();
        }
        if let Some(v) = l.strip_prefix("Range:").or_else(|| l.strip_prefix("range:")) {
            range = Some(v.trim().to_string());
        }
    }
    let snapshot = ns.lock().unwrap().clone();
    match method {
        "OPTIONS" => {
            let h = "HTTP/1.1 200 OK\r\nDAV: 1\r\nAllow: OPTIONS,PROPFIND,GET,HEAD\r\nContent-Length: 0\r\n\r\n";
            sock.write_all(h.as_bytes()).await?;
        }
        "PROPFIND" => {
            let body = propfind(&snapshot, &path, &depth);
            let h = format!(
                "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            sock.write_all(h.as_bytes()).await?;
            sock.write_all(body.as_bytes()).await?;
        }
        "GET" | "HEAD" => {
            let file = match resolve_file(&snapshot, &path) {
                Ok(f) => f.clone(),
                Err(_) => {
                    sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                        .await?;
                    return Ok(());
                }
            };
            let total = if file.encrypted {
                file.plaintext_size
            } else {
                file.size
            } as u64;
            let (start, end_excl, partial) = match range.as_deref().and_then(|r| r.strip_prefix("bytes=")) {
                Some(r) => {
                    let mut it = r.split('-');
                    let s: u64 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                    let e: u64 = it
                        .next()
                        .and_then(|v| v.parse::<u64>().ok())
                        .map(|e| e + 1)
                        .unwrap_or(total);
                    (s, e.min(total), true)
                }
                None => (0, total, false),
            };
            let parts: Vec<(i64, i64)> = if !file.upload_uuid.is_empty() {
                snapshot.part_sizes(&file.upload_uuid)
            } else {
                vec![(file.content_msg_id, file.size)]
            };
            let bytes = if method == "GET" {
                stream::read_range(
                    &client, &peer, &parts, start, end_excl, file.encrypted, &master,
                )
                .await
                .unwrap_or_default()
            } else {
                Vec::new()
            };
            let code = if partial { 206 } else { 200 };
            let mut h = format!(
                "HTTP/1.1 {code} OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\n",
                if method == "GET" { bytes.len() } else { (end_excl - start) as usize }
            );
            if partial {
                h.push_str(&format!(
                    "Content-Range: bytes {start}-{}/{total}\r\n",
                    end_excl.saturating_sub(1)
                ));
            }
            h.push_str("\r\n");
            sock.write_all(h.as_bytes()).await?;
            if !bytes.is_empty() {
                sock.write_all(&bytes).await?;
            }
        }
        _ => {
            sock.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n")
                .await?;
        }
    }
    Ok(())
}
