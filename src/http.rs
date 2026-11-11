//! Minimal HTTP/1.1 core shared by the WebDAV and streaming servers —
//! one request reader (headers can't split mid-read), one header map,
//! one Range parser, one response writer.

use anyhow::{Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const MAX_HEAD: usize = 64 * 1024;

#[derive(Debug)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
}

impl Request {
    /// Header lookup, case-insensitive per RFC 7230.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn content_length(&self) -> Option<u64> {
        self.header("content-length").and_then(|v| v.parse().ok())
    }
}

/// Read a request head (through `\r\n\r\n`) and return it plus any body
/// bytes already read past the terminator.
pub async fn read_head(sock: &mut TcpStream) -> Result<(Request, Vec<u8>)> {
    let mut raw = Vec::with_capacity(8192);
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if raw.len() > MAX_HEAD {
            bail!("header too large");
        }
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            bail!("connection closed mid-headers");
        }
        raw.extend_from_slice(&chunk[..n]);
    };
    let rest = raw[header_end + 4..].to_vec();
    let text = String::from_utf8_lossy(&raw[..header_end]);
    let mut lines = text.lines();
    let mut it = lines.next().unwrap_or("").split_whitespace();
    let req = Request {
        method: it.next().unwrap_or("").to_string(),
        path: it.next().unwrap_or("/").to_string(),
        headers: lines
            .filter_map(|l| {
                l.split_once(':')
                    .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            })
            .collect(),
    };
    Ok((req, rest))
}

/// Read exactly `len` more body bytes (beyond what `read_head` buffered).
pub async fn read_body(sock: &mut TcpStream, buffered: Vec<u8>, len: u64) -> Result<Vec<u8>> {
    let mut body = buffered;
    while (body.len() as u64) < len {
        let mut more = vec![0u8; 65536];
        let n = sock.read(&mut more).await?;
        if n == 0 {
            bail!("short body");
        }
        body.extend_from_slice(&more[..n]);
    }
    body.truncate(len as usize);
    Ok(body)
}

/// HTTP Range resolved to absolute `[start, end)` against `total`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Range {
    /// No Range header.
    Full,
    /// `[start, end)` absolute byte span, clamped to `total`.
    Span(u64, u64),
    /// Syntactically fine but starts at/after EOF — answer 416.
    Unsatisfiable,
}

/// Parse a `bytes=…` range header per RFC 7233, suffix ranges included.
pub fn range(v: Option<&str>, total: u64) -> Range {
    let Some(h) = v.and_then(|h| h.strip_prefix("bytes=")) else {
        return Range::Full;
    };
    let Some((a, b)) = h.split_once('-') else {
        return Range::Full;
    };
    let span = if a.is_empty() {
        // Suffix form `bytes=-N`: the last N bytes.
        let Ok(n) = b.parse::<u64>() else {
            return Range::Unsatisfiable;
        };
        (total.saturating_sub(n), total)
    } else {
        let Ok(s) = a.parse::<u64>() else {
            return Range::Unsatisfiable;
        };
        let e = if b.is_empty() {
            total
        } else {
            match b.parse::<u64>() {
                Ok(e) => (e + 1).min(total),
                Err(_) => return Range::Unsatisfiable,
            }
        };
        (s, e)
    };
    if span.0 >= total && span.0 > 0 {
        Range::Unsatisfiable
    } else {
        Range::Span(span.0, span.1.max(span.0))
    }
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        206 => "Partial Content",
        207 => "Multi-Status",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        416 => "Range Not Satisfiable",
        431 => "Request Header Fields Too Large",
        _ => "Internal Server Error",
    }
}

/// One-shot response — these servers handle a single request per
/// connection, so `Connection: close` is always honest.
pub async fn respond(
    sock: &mut TcpStream,
    code: u16,
    extra_headers: &[(&str, &str)],
    body: &[u8],
) -> Result<()> {
    respond_len(sock, code, extra_headers, body, body.len() as u64).await
}

/// Same as `respond` but the declared Content-Length may exceed the
/// body sent — HEAD answers with the entity length and no entity.
pub async fn respond_len(
    sock: &mut TcpStream,
    code: u16,
    extra_headers: &[(&str, &str)],
    body: &[u8],
    content_length: u64,
) -> Result<()> {
    let mut h = format!(
        "HTTP/1.1 {code} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        reason(code),
        content_length
    );
    for (k, v) in extra_headers {
        h.push_str(&format!("{k}: {v}\r\n"));
    }
    h.push_str("\r\n");
    sock.write_all(h.as_bytes()).await?;
    if !body.is_empty() {
        sock.write_all(body).await?;
    }
    Ok(())
}

/// Reply with a bare status line (no body).
pub async fn reply(sock: &mut TcpStream, code: u16) -> Result<()> {
    respond(sock, code, &[], &[]).await
}
