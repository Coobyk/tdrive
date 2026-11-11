//! WebDAV surface over `webdav://127.0.0.1:<port>/dav/` — file managers
//! can mount a drive without a kernel FS. PROPFIND lists the namespace,
//! GET/HEAD serve plaintext ranges via the shared TDE1-aware reader, and
//! MKCOL/DELETE/MOVE/PUT forward to the worker as `VfsOp`s — the same
//! writable-mount contract TDrive exposes.

use crate::http::{self, Range};
use crate::ops::{MAX_PUT_BYTES, VfsOp};
use crate::project::{Entry, EntryRef, FileEntry};
use crate::remote::Remote;
use crate::tg::Command;
use anyhow::Result;
use std::sync::OnceLock;
use tokio::net::TcpListener;

static PORT: OnceLock<u16> = OnceLock::new();

type CmdTx = tokio::sync::mpsc::UnboundedSender<Command>;

/// Spawn the DAV listener once. Later calls are no-ops — the server's
/// `Remote` cells already repoint it on drive switch.
pub async fn ensure_server(remote: Remote, cmd: CmdTx) {
    if PORT.get().is_some() {
        return;
    }
    let Ok(listener) = TcpListener::bind(("127.0.0.1", 0)).await else {
        return;
    };
    if let Ok(addr) = listener.local_addr() {
        let _ = PORT.set(addr.port());
    }
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                continue;
            };
            tokio::spawn(handle(sock, remote.clone(), cmd.clone()));
        }
    });
}

pub fn url() -> Option<String> {
    PORT.get().map(|p| format!("webdav://127.0.0.1:{p}/dav/"))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn decode(seg: &str) -> String {
    urlencoding::decode(seg).unwrap_or_default().to_string()
}

/// Split a DAV path into (dir, name) with decoding.
fn split_path(path: &str) -> (String, String) {
    let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
    (decode(dir), decode(name))
}

/// The decoded, "/" joined path under /dav — every resolver starts here.
fn decoded_path(path: &str) -> String {
    path.trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .map(decode)
        .collect::<Vec<_>>()
        .join("/")
}

/// Resolve a DAV path to a folder id ("" = root).
fn resolve_dir(ns: &crate::project::Namespace, path: &str) -> Result<String> {
    if path.trim_matches('/').is_empty() {
        return Ok(String::new());
    }
    let p = decoded_path(path);
    match ns.resolve_path(&p) {
        Some(EntryRef::Folder(d)) => Ok(d.id.clone()),
        None if p.is_empty() => Ok(String::new()),
        _ => anyhow::bail!("no such folder"),
    }
}

fn propfind(ns: &crate::project::Namespace, path: &str, depth: &str) -> Result<String> {
    let mut xml =
        String::from(r#"<?xml version="1.0" encoding="utf-8"?><D:multistatus xmlns:D="DAV:">"#);
    let mut emit = |href: &str, is_dir: bool, size: i64| {
        let rt = if is_dir { "<D:collection/>" } else { "" };
        let ct = if is_dir {
            "<D:getcontenttype>application/x-directory</D:getcontenttype>"
        } else {
            ""
        };
        xml.push_str(&format!(
            "<D:response><D:href>{href}</D:href><D:propstat><D:prop>\
             <D:resourcetype>{rt}</D:resourcetype>{ct}\
             <D:getcontentlength>{size}</D:getcontentlength>\
             </D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>"
        ));
    };
    let decoded_path = decoded_path(path);
    let base = format!("/dav/{decoded_path}");
    match ns.resolve_path(&decoded_path) {
        Some(EntryRef::File(f)) => {
            emit(&xml_escape(&base), false, f.logical_size());
        }
        Some(EntryRef::Folder(d)) => {
            emit(&xml_escape(&format!("{base}/")), true, 0);
            if depth.trim() == "1" {
                for e in ns.list(&d.id) {
                    match e {
                        Entry::Folder(d) => {
                            emit(&xml_escape(&format!("{base}/{}/", d.name)), true, 0)
                        }
                        Entry::File(f) => emit(
                            &xml_escape(&format!("{base}/{}", f.name)),
                            false,
                            f.logical_size(),
                        ),
                    }
                }
            }
        }
        None if decoded_path.is_empty() => {
            emit("/dav/", true, 0);
            if depth.trim() == "1" {
                for e in ns.list("") {
                    match e {
                        Entry::Folder(d) => {
                            emit(&xml_escape(&format!("/dav/{}/", d.name)), true, 0)
                        }
                        Entry::File(f) => emit(
                            &xml_escape(&format!("/dav/{}", f.name)),
                            false,
                            f.logical_size(),
                        ),
                    }
                }
            }
        }
        None => anyhow::bail!("not found"),
    }
    Ok(format!("{xml}</D:multistatus>"))
}

/// Resolve a path to an object id (file or folder).
fn resolve_obj(ns: &crate::project::Namespace, path: &str) -> Result<String> {
    match ns.resolve_path(&decoded_path(path)) {
        Some(EntryRef::Folder(d)) => Ok(d.id.clone()),
        Some(EntryRef::File(f)) => Ok(f.id.clone()),
        None => anyhow::bail!("not found"),
    }
}

/// Resolve a path to a file entry.
fn resolve_file<'a>(ns: &'a crate::project::Namespace, path: &str) -> Result<&'a FileEntry> {
    match ns.resolve_path(&decoded_path(path)) {
        Some(EntryRef::File(f)) => Ok(f),
        _ => anyhow::bail!("no such file"),
    }
}

/// Route a write method to the worker via VfsOp and translate the result.
async fn write_method(
    req: &http::Request,
    mut sock: tokio::net::TcpStream,
    rest: Vec<u8>,
    remote: &Remote,
    cmd: &CmdTx,
) -> Result<()> {
    let path = req.path.strip_prefix("/dav").unwrap_or("/");
    let ns = remote.namespace();
    let op = match req.method.as_str() {
        "MKCOL" => {
            if resolve_obj(&ns, path).is_ok() {
                return http::respond(&mut sock, 405, &[], b"exists").await;
            }
            let (dir, name) = split_path(path);
            match resolve_dir(&ns, &dir) {
                Ok(parent) => VfsOp::Mkdir { parent, name },
                Err(_) => return http::respond(&mut sock, 409, &[], b"bad parent").await,
            }
        }
        "DELETE" => match resolve_obj(&ns, path) {
            Ok(obj) => VfsOp::Delete { obj },
            Err(_) => return http::respond(&mut sock, 404, &[], b"not found").await,
        },
        "MOVE" => {
            let src = match resolve_obj(&ns, path) {
                Ok(o) => o,
                Err(_) => return http::respond(&mut sock, 404, &[], b"not found").await,
            };
            // Destination may be an absolute URL — take the path tail
            // after the /dav mountpoint.
            let dest = req
                .header("destination")
                .and_then(|u| u.split("/dav").nth(1))
                .unwrap_or("");
            let (dir, name) = split_path(dest);
            match resolve_dir(&ns, &dir) {
                Ok(dst_parent) => VfsOp::Move {
                    obj: src,
                    dst_parent,
                    name,
                },
                Err(_) => {
                    return http::respond(&mut sock, 409, &[], b"bad destination").await;
                }
            }
        }
        "PUT" => {
            let Some(clen) = req.content_length() else {
                return http::respond(&mut sock, 411, &[], b"length required").await;
            };
            if clen as usize > MAX_PUT_BYTES {
                return http::respond(&mut sock, 413, &[], b"too large").await;
            }
            let body = match http::read_body(&mut sock, rest, clen).await {
                Ok(b) => b,
                Err(_) => return http::respond(&mut sock, 400, &[], b"short body").await,
            };
            let (dir, name) = split_path(path);
            match resolve_dir(&ns, &dir) {
                Ok(parent) => match resolve_obj(&ns, path) {
                    // Overwrite → freplace (TDrive's mount PUT), else fcommit.
                    Ok(obj) if crate::wire::is_file_id(&obj) => VfsOp::Replace { obj, bytes: body },
                    Ok(_) => {
                        return http::respond(&mut sock, 405, &[], b"can't overwrite dir").await;
                    }
                    Err(_) => VfsOp::Put {
                        parent,
                        name,
                        bytes: body,
                    },
                },
                Err(_) => return http::respond(&mut sock, 409, &[], b"bad parent").await,
            }
        }
        _ => return http::respond(&mut sock, 405, &[], b"method").await,
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    if cmd.send(Command::VfsWrite { op, reply: tx }).is_err() {
        return http::respond(&mut sock, 500, &[], b"worker gone").await;
    }
    match rx.await {
        Ok(Ok(_)) => http::respond(&mut sock, 201, &[], &[]).await,
        Ok(Err(e)) => http::respond(&mut sock, 500, &[], e.as_bytes()).await,
        Err(_) => http::respond(&mut sock, 500, &[], b"no reply").await,
    }
}

async fn handle(mut sock: tokio::net::TcpStream, remote: Remote, cmd: CmdTx) -> Result<()> {
    let (req, rest) = match http::read_head(&mut sock).await {
        Ok(r) => r,
        Err(_) => {
            return http::reply(&mut sock, 431).await;
        }
    };
    let path = req.path.strip_prefix("/dav").unwrap_or("/").to_string();
    let ns = remote.namespace();
    match req.method.as_str() {
        "OPTIONS" => {
            http::respond(
                &mut sock,
                200,
                &[
                    ("DAV", "1"),
                    ("Allow", "OPTIONS,PROPFIND,GET,HEAD,MKCOL,DELETE,MOVE,PUT"),
                ],
                &[],
            )
            .await
        }
        "PROPFIND" => {
            let depth = req.header("depth").unwrap_or("0").to_string();
            match propfind(&ns, &path, &depth) {
                Ok(body) => {
                    http::respond(
                        &mut sock,
                        207,
                        &[("Content-Type", "application/xml")],
                        body.as_bytes(),
                    )
                    .await
                }
                Err(_) => http::reply(&mut sock, 404).await,
            }
        }
        "GET" | "HEAD" => {
            let file = match resolve_file(&ns, &path) {
                Ok(f) => f.clone(),
                Err(_) => return http::reply(&mut sock, 404).await,
            };
            let total = file.logical_size() as u64;
            let (start, end_excl, partial) = match http::range(req.header("range"), total) {
                Range::Span(s, e) => (s, e, true),
                Range::Unsatisfiable => {
                    return http::respond(
                        &mut sock,
                        416,
                        &[("Content-Range", &format!("bytes */{total}"))],
                        &[],
                    )
                    .await;
                }
                Range::Full => (0, total, false),
            };
            let parts = ns.body_parts(&file);
            let bytes = if req.method == "GET" {
                match remote
                    .read_range(&parts, start, end_excl, file.encrypted)
                    .await
                {
                    Ok(b) => b,
                    Err(e) => {
                        // Read failures (incl. locked drive) must not look
                        // like an empty file.
                        return http::respond(&mut sock, 500, &[], format!("{e:#}").as_bytes())
                            .await;
                    }
                }
            } else {
                Vec::new()
            };
            let mut headers = vec![("Accept-Ranges", "bytes".to_string())];
            if partial {
                headers.push((
                    "Content-Range",
                    format!("bytes {start}-{}/{total}", end_excl.saturating_sub(1)),
                ));
            }
            let code = if partial { 206 } else { 200 };
            let len = if req.method == "GET" {
                bytes
            } else {
                Vec::new()
            };
            // HEAD reports the range length without a body.
            if req.method == "HEAD" {
                headers.push(("X-Content-Length-Note", format!("{}", end_excl - start)));
                let _ = len;
                return http::respond(
                    &mut sock,
                    code,
                    &headers
                        .iter()
                        .map(|(k, v)| (*k, v.as_str()))
                        .collect::<Vec<_>>(),
                    &[],
                )
                .await;
            }
            http::respond(
                &mut sock,
                code,
                &headers
                    .iter()
                    .map(|(k, v)| (*k, v.as_str()))
                    .collect::<Vec<_>>(),
                &len,
            )
            .await
        }
        "MKCOL" | "DELETE" | "MOVE" | "PUT" => write_method(&req, sock, rest, &remote, &cmd).await,
        _ => http::respond(&mut sock, 405, &[], &[]).await,
    }
}
