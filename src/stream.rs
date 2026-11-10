//! Local media streaming — a tokenized HTTP server on 127.0.0.1 serving
//! plaintext byte ranges over a resolved logical file, like TDrive's media
//! service. Plain files map 1:1 onto stored bytes; encrypted files go through
//! TDE1 range decryption. URLs carry a random token — the capability — and
//! sessions stay open so players can issue many range requests.

use crate::crypto;
use anyhow::{Context, Result};
use grammers_client::session::types::PeerRef;
use grammers_client::Client;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

static REGISTRY: OnceLock<Registry> = OnceLock::new();
static PORT: OnceLock<u16> = OnceLock::new();
static STARTED: OnceLock<()> = OnceLock::new();

/// Everything needed to serve one logical file.
pub struct Session {
    /// Body parts in order: (msg_id, stored size).
    pub parts: Vec<(i64, i64)>,
    /// Logical plaintext size (== stored size when unencrypted).
    pub plaintext_size: i64,
    pub name: String,
    /// The stored stream is TDE1 ciphertext.
    pub encrypted: bool,
}

type Registry = Arc<Mutex<HashMap<String, Session>>>;

/// Spawn the streaming listener inside the worker's runtime — only once.
/// The master key is shared by reference so locking the drive revokes
/// encrypted streams the way TDrive's session teardown does.
pub fn ensure_server(
    client: Client,
    peer: PeerRef,
    master: Arc<Mutex<Option<[u8; crypto::MASTER_KEY_LEN]>>>,
) {
    if STARTED.set(()).is_err() {
        return;
    }
    let reg: Registry = Arc::new(Mutex::new(HashMap::new()));
    let _ = REGISTRY.set(reg.clone());
    tokio::spawn(async move {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let _ = PORT.set(listener.local_addr()?.port());
        loop {
            let (sock, _) = listener.accept().await?;
            tokio::spawn(serve_conn(
                sock,
                reg.clone(),
                client.clone(),
                peer.clone(),
                master.clone(),
            ));
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    });
}

/// Register a session and return its stream URL. Waits briefly for the
/// listener's port — `ensure_server` binds it inside a spawned task.
pub fn open(session: Session) -> Result<String> {
    let mut tries = 0;
    let port = loop {
        if let Some(p) = PORT.get() {
            break *p;
        }
        tries += 1;
        if tries > 100 {
            anyhow::bail!("stream server not ready");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let reg = REGISTRY.get().context("no registry")?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    let name = urlencoding::encode(&session.name).to_string();
    reg.lock().unwrap().insert(token.clone(), session);
    Ok(format!("http://127.0.0.1:{port}/stream/{token}/{name}"))
}

async fn serve_conn(
    mut sock: tokio::net::TcpStream,
    reg: Registry,
    client: Client,
    peer: PeerRef,
    master: Arc<Mutex<Option<[u8; crypto::MASTER_KEY_LEN]>>>,
) -> Result<()> {
    use tokio::io::AsyncReadExt;
    let mut buf = vec![0u8; 8192];
    let n = sock.read(&mut buf).await?;
    let req = String::from_utf8_lossy(&buf[..n]);
    let mut lines = req.lines();
    let req_line = lines.next().unwrap_or("");
    let mut range = None;
    for l in lines {
        if let Some(v) = l.strip_prefix("Range:").or_else(|| l.strip_prefix("range:")) {
            range = Some(v.trim().to_string());
        }
    }
    let path = req_line
        .strip_prefix("GET ")
        .and_then(|r| r.split_whitespace().next())
        .unwrap_or("");
    let token = path.split('/').nth(2).unwrap_or("");
    let found = {
        let g = reg.lock().unwrap();
        g.get(token).map(|s| {
            (
                s.parts.clone(),
                s.plaintext_size,
                s.encrypted,
                s.name.clone(),
            )
        })
    };
    let Some((parts, pt_size, encrypted, name)) = found else {
        return respond(&mut sock, 404, "text/plain", b"gone", None).await;
    };
    let (start, end_incl, partial) = parse_range(range.as_deref(), pt_size as u64);
    if pt_size <= 0 || start >= pt_size as u64 {
        return respond(&mut sock, 416, "text/plain", b"range", None).await;
    }
    let end_incl = end_incl.min(pt_size as u64 - 1);
    let bytes =
        match read_range(&client, &peer, &parts, start, end_incl + 1, encrypted, &master).await {
            Ok(b) => b,
            Err(_) => return respond(&mut sock, 404, "text/plain", b"read failed", None).await,
        };
    let mime = mime_for(&name);
    let crange = partial.then(|| format!("bytes {start}-{end_incl}/{pt_size}"));
    respond(
        &mut sock,
        if partial { 206 } else { 200 },
        &mime,
        &bytes,
        crange.as_deref(),
    )
    .await
}

fn parse_range(h: Option<&str>, total: u64) -> (u64, u64, bool) {
    let Some(h) = h.and_then(|h| h.strip_prefix("bytes=")) else {
        return (0, total.saturating_sub(1), false);
    };
    let mut it = h.split('-');
    let s: u64 = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let e: u64 = it
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(total.saturating_sub(1));
    (s, e, true)
}

fn mime_for(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
        "mp4" | "m4v" | "mov" => "video/mp4",
        "webm" => "video/webm",
        "mkv" | "mk3d" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "mp3" => "audio/mpeg",
        "m4a" | "aac" => "audio/mp4",
        "flac" => "audio/flac",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

async fn respond(
    sock: &mut tokio::net::TcpStream,
    code: u16,
    ctype: &str,
    body: &[u8],
    crange: Option<&str>,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let reason = match code {
        200 => "OK",
        206 => "Partial Content",
        404 => "Not Found",
        416 => "Range Not Satisfiable",
        _ => "Error",
    };
    let mut h = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\n",
        body.len()
    );
    if let Some(c) = crange {
        h.push_str(&format!("Content-Range: {c}\r\n"));
    }
    h.push_str("Connection: close\r\n\r\n");
    sock.write_all(h.as_bytes()).await?;
    sock.write_all(body).await?;
    Ok(())
}

/// Assemble plaintext `[start, end)` from the stored body parts.
pub(crate) async fn read_range(
    client: &Client,
    peer: &PeerRef,
    parts: &[(i64, i64)],
    start: u64,
    end: u64,
    encrypted: bool,
    master: &Arc<Mutex<Option<[u8; crypto::MASTER_KEY_LEN]>>>,
) -> Result<Vec<u8>> {
    if encrypted {
        let key = { *master.lock().unwrap() }.context("drive locked")?;
        // The TDE1 header sits at the start of the stored stream.
        let header = stored_read(client, peer, parts, 0, crypto::HEADER_LEN as u64).await?;
        let dec = crypto::RangeDecryptor::new(&key, &header)?;
        let (cs, ce) = dec.ct_range(start, end);
        let ct = stored_read(client, peer, parts, cs, ce).await?;
        return dec.decrypt_range(start, end, &ct);
    }
    stored_read(client, peer, parts, start, end).await
}

/// Concatenate `[start, end)` from the stored stream spanning body parts.
async fn stored_read(
    client: &Client,
    peer: &PeerRef,
    parts: &[(i64, i64)],
    start: u64,
    end: u64,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut base = 0u64;
    for (msg_id, size) in parts {
        let lo = base;
        let hi = base + *size as u64;
        base = hi;
        if start >= hi || end <= lo {
            continue;
        }
        let seg = part_read(
            client,
            peer,
            *msg_id,
            start.max(lo) - lo,
            end.min(hi) - lo,
        )
        .await?;
        out.extend_from_slice(&seg);
    }
    Ok(out)
}

/// Fetch `[start, end)` bytes of one stored message body via chunk-aligned
/// downloads (`skip_chunks` only skips whole chunks).
async fn part_read(
    client: &Client,
    peer: &PeerRef,
    msg_id: i64,
    start: u64,
    end: u64,
) -> Result<Vec<u8>> {
    const CHUNK: u64 = 512 * 1024;
    let msgs = client.get_messages_by_id(*peer, &[msg_id as i32]).await?;
    let media = msgs
        .into_iter()
        .flatten()
        .next()
        .and_then(|m| m.media())
        .context("body missing")?;
    let skip = (start / CHUNK) as i32;
    let mut it = client
        .iter_download(&media)
        .chunk_size(CHUNK as i32)
        .skip_chunks(skip);
    let mut buf = Vec::new();
    while let Some(b) = it.next().await? {
        buf.extend_from_slice(&b);
        if skip as u64 * CHUNK + buf.len() as u64 >= end {
            break;
        }
    }
    let rel = (start % CHUNK) as usize;
    let take = (end - start) as usize;
    // A body shorter than the declared size must not panic — the slice
    // would otherwise kill the FUSE dispatch thread.
    if rel >= buf.len() {
        return Ok(Vec::new());
    }
    Ok(buf[rel..(rel + take).min(buf.len())].to_vec())
}
