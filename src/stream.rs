//! Local media streaming — a tokenized HTTP server on 127.0.0.1 serving
//! plaintext byte ranges over a resolved logical file, like TDrive's media
//! service. Plain files map 1:1 onto stored bytes; encrypted files go
//! through TDE1 range decryption. URLs carry a random token — the
//! capability — and sessions stay open so players can issue many range
//! requests.

use crate::http::{self, Range};
use crate::remote::Remote;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

static SERVER: OnceLock<Server> = OnceLock::new();

struct Server {
    port: u16,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
}

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

/// Bind the streaming listener once. The shared master-key cell means
/// locking the drive revokes encrypted streams the way TDrive's session
/// teardown does.
pub async fn ensure_server(remote: Remote) -> Result<()> {
    if SERVER.get().is_some() {
        return Ok(());
    }
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let server = Server {
        port,
        sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    let sessions = server.sessions.clone();
    let _ = SERVER.set(server);
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                continue;
            };
            tokio::spawn(serve_conn(sock, sessions.clone(), remote.clone()));
        }
    });
    Ok(())
}

/// Register a session and return its stream URL.
pub fn open(session: Session) -> Result<String> {
    let server = SERVER.get().context("stream server not started")?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    let name = urlencoding::encode(&session.name).to_string();
    server
        .sessions
        .lock()
        .unwrap()
        .insert(token.clone(), session);
    Ok(format!(
        "http://127.0.0.1:{}/stream/{token}/{name}",
        server.port
    ))
}

async fn serve_conn(
    mut sock: tokio::net::TcpStream,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    remote: Remote,
) -> Result<()> {
    let (req, _rest) = match http::read_head(&mut sock).await {
        Ok(r) => r,
        Err(_) => return http::reply(&mut sock, 400).await,
    };
    if req.method != "GET" {
        return http::reply(&mut sock, 405).await;
    }
    let token = req.path.split('/').nth(2).unwrap_or("");
    let found = sessions.lock().unwrap().get(token).map(|s| {
        (
            s.parts.clone(),
            s.plaintext_size,
            s.encrypted,
            s.name.clone(),
        )
    });
    let Some((parts, pt_size, encrypted, name)) = found else {
        return http::respond(&mut sock, 404, &[], b"gone").await;
    };
    let total = pt_size as u64;
    let (start, end_excl, partial) = match http::range(req.header("range"), total) {
        Range::Span(s, e) => (s, e, true),
        Range::Unsatisfiable => {
            return http::respond(&mut sock, 416, &[], b"range").await;
        }
        Range::Full => (0, total, false),
    };
    if total == 0 {
        return http::respond(&mut sock, 416, &[], b"range").await;
    }
    let bytes = match remote.read_range(&parts, start, end_excl, encrypted).await {
        Ok(b) => b,
        Err(_) => return http::respond(&mut sock, 500, &[], b"read failed").await,
    };
    let mut headers = vec![
        ("Content-Type", mime_for(&name).to_string()),
        ("Accept-Ranges", "bytes".to_string()),
        ("Cache-Control", "no-store".to_string()),
    ];
    if partial {
        headers.push((
            "Content-Range",
            format!("bytes {start}-{}/{pt_size}", end_excl - 1),
        ));
    }
    http::respond(
        &mut sock,
        if partial { 206 } else { 200 },
        &headers
            .iter()
            .map(|(k, v)| (*k, v.as_str()))
            .collect::<Vec<_>>(),
        &bytes,
    )
    .await
}

fn mime_for(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "mp4" | "m4v" | "mov" => "video/mp4",
        "webm" => "video/webm",
        "mkv" | "mk3d" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "ts" => "video/mp2t",
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
