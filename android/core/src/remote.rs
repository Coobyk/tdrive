//! The live drive connection shared by every surface — WebDAV, FUSE, the
//! streaming server and background transfer tasks all used to thread the
//! same (client, peer, master, namespace) tuple by hand. `Remote` is that
//! tuple as a type: the cells are updated on channel switch and vault
//! lock/unlock, so mounted surfaces always follow the current drive.
//!
//! Transfer tasks snapshot `peer` at spawn instead — an upload finishes
//! on the drive it started on.

use crate::crypto::{self, MASTER_KEY_LEN};
use crate::project::Namespace;
use anyhow::{Context, Result};
use grammers_client::Client;
use grammers_client::message::InputMessage;
use grammers_client::session::types::PeerRef;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Remote {
    pub client: Client,
    peer: Arc<Mutex<Option<PeerRef>>>,
    master: Arc<Mutex<Option<[u8; MASTER_KEY_LEN]>>>,
    ns: Arc<Mutex<Arc<Namespace>>>,
}

/// Telegram download granularity — resume offsets and range reads must
/// agree on the chunk size or skip math drifts.
pub const BODY_CHUNK: i32 = 512 * 1024;

impl Remote {
    pub fn new(
        client: Client,
        peer: Arc<Mutex<Option<PeerRef>>>,
        master: Arc<Mutex<Option<[u8; MASTER_KEY_LEN]>>>,
        ns: Arc<Mutex<Arc<Namespace>>>,
    ) -> Self {
        Self {
            client,
            peer,
            master,
            ns,
        }
    }

    /// Current drive peer.
    pub fn peer(&self) -> Result<PeerRef> {
        self.peer.lock().unwrap().context("no drive open")
    }

    pub fn master_key(&self) -> Option<[u8; MASTER_KEY_LEN]> {
        *self.master.lock().unwrap()
    }

    pub fn namespace(&self) -> Arc<Namespace> {
        self.ns.lock().unwrap().clone()
    }

    /// Send a TDX1 op to `peer` and return the message id, recovering it
    /// by scan when grammers reports `Empty{id:0}`.
    pub async fn send_op(&self, peer: PeerRef, header: &str) -> Result<i64> {
        let msg = self
            .client
            .send_message(peer, InputMessage::new().text(header))
            .await?;
        let id = msg.id() as i64;
        if id > 0 {
            return Ok(id);
        }
        Ok(recover_msg_id(&self.client, peer, header).await)
    }

    /// Send a file body (uploaded bytes + TDX1 caption) and return its
    /// message id, recovering it by caption scan when grammers reports
    /// `Empty{id:0}`. Single-body files and parts both bind their commit
    /// op to this id — a 0 here would corrupt the commit.
    pub async fn send_doc_op(
        &self,
        peer: PeerRef,
        uploaded: grammers_client::media::Uploaded,
        caption: &str,
    ) -> Result<i64> {
        let msg = self
            .client
            .send_message(
                peer,
                InputMessage::new()
                    .document(uploaded)
                    .text(caption.to_string()),
            )
            .await?;
        let id = msg.id() as i64;
        if id > 0 {
            return Ok(id);
        }
        Ok(recover_msg_id(&self.client, peer, caption).await)
    }

    /// Delete body messages, honoring Telegram's 100-ids-per-call cap.
    pub async fn delete_bodies(&self, ids: &[i64]) -> Result<()> {
        let peer = self.peer()?;
        for chunk in ids.chunks(100) {
            let ids: Vec<i32> = chunk.iter().map(|&m| m as i32).collect();
            self.client.delete_messages(peer, &ids).await?;
        }
        Ok(())
    }

    /// Assemble plaintext `[start, end)` from the stored body parts
    /// (`(msg_id, stored size)` pairs in read order).
    pub async fn read_range(
        &self,
        parts: &[(i64, i64)],
        start: u64,
        end: u64,
        encrypted: bool,
    ) -> Result<Vec<u8>> {
        let peer = self.peer()?;
        if encrypted {
            let key = self.master_key().context("drive locked")?;
            // The TDE1 header sits at the start of the stored stream.
            let header = self
                .stored_read(&peer, parts, 0, crypto::HEADER_LEN as u64)
                .await?;
            let dec = crypto::RangeDecryptor::new(&key, &header)?;
            let (cs, ce) = dec.ct_range(start, end);
            let ct = self.stored_read(&peer, parts, cs, ce).await?;
            return dec.decrypt_range(start, end, &ct);
        }
        self.stored_read(&peer, parts, start, end).await
    }

    /// Concatenate `[start, end)` from the stored stream spanning parts.
    async fn stored_read(
        &self,
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
            let seg = self
                .part_read(peer, *msg_id, start.max(lo) - lo, end.min(hi) - lo)
                .await?;
            out.extend_from_slice(&seg);
        }
        Ok(out)
    }

    /// Fetch `[start, end)` bytes of one stored message body via
    /// chunk-aligned downloads (`skip_chunks` only skips whole chunks).
    async fn part_read(
        &self,
        peer: &PeerRef,
        msg_id: i64,
        start: u64,
        end: u64,
    ) -> Result<Vec<u8>> {
        const CHUNK: u64 = BODY_CHUNK as u64;
        let msgs = self
            .client
            .get_messages_by_id(*peer, &[msg_id as i32])
            .await?;
        let media = msgs
            .into_iter()
            .flatten()
            .next()
            .and_then(|m| m.media())
            .context("body missing")?;
        let skip = (start / CHUNK) as i32;
        let mut it = self
            .client
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
        // A body shorter than its declared size must not panic — the
        // slice would otherwise kill the FUSE dispatch thread.
        if rel >= buf.len() {
            return Ok(Vec::new());
        }
        Ok(buf[rel..(rel + take).min(buf.len())].to_vec())
    }
}

/// Recover the real message id after `send_message` reports `Empty{id:0}`
/// — the op reached the channel, so the newest message carrying our exact
/// text is the one just sent. Returns 0 when recovery fails.
async fn recover_msg_id(client: &Client, peer: PeerRef, header: &str) -> i64 {
    let mut iter = client.iter_messages(peer);
    for _ in 0..20 {
        let m = match iter.next().await {
            Ok(Some(m)) => m,
            _ => break,
        };
        // Empty caption can't be matched on text — the newest message is
        // the one just sent.
        if header.is_empty() || m.text() == header {
            return m.id() as i64;
        }
    }
    0
}

/// Parallel chunked upload — grammers' `upload_stream` only fans out
/// above its 10MB BIG_FILE threshold, so every smaller file pays one
/// 512KB SaveFilePart per RTT, which is a crawl on mobile links. This
/// runs `workers` parallel part uploads for any size (small files just
/// have few parts) and returns the same `Uploaded` `send_message`
/// accepts. Flood-wait retries ride on `client.invoke`'s RetryPolicy.
///
/// Chunks are read per-part via seek+take on one `tokio::fs::File` per
/// worker — the drive's resumable-part boundary (offset) is preserved
/// so TDX1 parts map 1:1 onto Telegram files as before.
pub async fn upload_parallel(
    client: &Client,
    path: &std::path::Path,
    offset: u64,
    size: u64,
    name: String,
    workers: usize,
    progress: impl Fn(i64) + Send + Sync + 'static,
) -> Result<grammers_client::media::Uploaded> {
    use grammers_client::tl;
    use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    const BIG: u64 = 10 * 1024 * 1024;
    let chunk = BODY_CHUNK as u64;
    let total_parts = ((size + chunk - 1) / chunk) as i32;
    let file_id: i64 = rand::random();
    let big = size > BIG;

    // Small files need an md5 in the final InputFile — compute it
    // upfront; a local read pass is free next to network RTTs.
    let md5_checksum = if big {
        String::new()
    } else {
        let mut f = tokio::fs::File::open(path).await?;
        f.seek(std::io::SeekFrom::Start(offset)).await?;
        let mut take = f.take(size);
        let mut ctx = md5::Context::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = take.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            ctx.consume(&buf[..n]);
        }
        format!("{:x}", ctx.finalize())
    };

    let next = Arc::new(AtomicU32::new(0));
    let sent = Arc::new(AtomicI64::new(0));
    let progress = Arc::new(progress);
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..workers.max(1).min(total_parts.max(1) as usize) {
        let client = client.clone();
        let path = path.to_path_buf();
        let (next, sent, progress) = (next.clone(), sent.clone(), progress.clone());
        set.spawn(async move {
            let mut file = tokio::fs::File::open(&path).await?;
            loop {
                let part = next.fetch_add(1, Ordering::Relaxed) as i32;
                if part >= total_parts {
                    return Ok(());
                }
                let pos = offset + part as u64 * chunk;
                let len = ((size - part as u64 * chunk).min(chunk)) as usize;
                file.seek(std::io::SeekFrom::Start(pos)).await?;
                let mut bytes = vec![0u8; len];
                file.read_exact(&mut bytes).await?;
                let ok: bool = if big {
                    client
                        .invoke(&tl::functions::upload::SaveBigFilePart {
                            file_id,
                            file_part: part,
                            file_total_parts: total_parts,
                            bytes,
                        })
                        .await?
                } else {
                    client
                        .invoke(&tl::functions::upload::SaveFilePart {
                            file_id,
                            file_part: part,
                            bytes,
                        })
                        .await?
                };
                if !ok {
                    anyhow::bail!("server failed to store uploaded part {part}");
                }
                progress(sent.fetch_add(len as i64, Ordering::Relaxed) + len as i64);
            }
        });
    }
    while let Some(res) = set.join_next().await {
        res??;
    }
    let raw: tl::enums::InputFile = if big {
        tl::types::InputFileBig {
            id: file_id,
            parts: total_parts,
            name,
        }
        .into()
    } else {
        tl::types::InputFile {
            id: file_id,
            parts: total_parts,
            name,
            md5_checksum,
        }
        .into()
    };
    Ok(grammers_client::media::Uploaded::from_raw(raw))
}
