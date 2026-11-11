//! Media pipeline — thumbnail/preview fetch and rendition publish.
use super::*;

impl Worker {
    /// Best-effort rendition publish — TDrive generates a 512px "thumb" and
    /// 1600px "preview" JPEG for image uploads, sent as `rend` part ops.
    /// Failures never invalidate the stored original.
    pub(super) async fn publish_renditions(
        ctx: &TransferCtx,
        source: &PathBuf,
        name: &str,
        file_msg_id: i64,
        content_msg_id: i64,
        upload_uuid: &str,
        encrypted: bool,
    ) {
        let owned_source;
        let source = if is_image_name(name) {
            source
        } else if is_video_name(name) {
            // TDrive renders video thumbs through ffmpeg; extract a frame
            // locally and feed the same rendition pipeline.
            match extract_video_frame(source).await {
                Some(p) => {
                    owned_source = p;
                    &owned_source
                }
                None => return,
            }
        } else {
            return;
        };
        for (kind, edge) in [("thumbnail", 512u32), ("preview", 1600u32)] {
            if let Err(e) = Self::publish_rendition(
                ctx,
                source,
                file_msg_id,
                content_msg_id,
                upload_uuid,
                encrypted,
                kind,
                edge,
            )
            .await
            {
                ctx.emit(Event::Error {
                    context: "rendition",
                    message: format!("{e:#}"),
                });
            }
        }
    }

    pub(super) async fn publish_rendition(
        ctx: &TransferCtx,
        source: &PathBuf,
        file_msg_id: i64,
        content_msg_id: i64,
        upload_uuid: &str,
        encrypted: bool,
        kind: &str,
        edge: u32,
    ) -> Result<()> {
        let src = source.clone();
        let (jpeg, w, h) = tokio::task::spawn_blocking(move || {
            let img = image::open(&src).context("decode")?;
            let t = img.thumbnail(edge, edge);
            let mut cur = std::io::Cursor::new(Vec::new());
            t.write_to(&mut cur, image::ImageFormat::Jpeg)
                .context("jpeg encode")?;
            Ok::<_, anyhow::Error>((cur.into_inner(), t.width(), t.height()))
        })
        .await??;
        let pt_len = jpeg.len() as i64;
        let (body, stored) = if encrypted {
            let master = ctx.remote.master_key().context("drive locked")?;
            // TDrive wraps encrypted renditions in a TDR1 envelope binding
            // the descriptor (s=0) — Go's decryptRendition requires it.
            let desc = serde_json::json!({
                "c": ctx.peer.id.bare_id_unchecked(), "f": file_msg_id, "m": content_msg_id,
                "u": upload_uuid, "k": kind, "v": 1, "s": 0,
                "p": pt_len, "w": w, "h": h, "e": true,
            });
            let desc_bytes = serde_json::to_vec(&desc)?;
            let mut plain = Vec::with_capacity(8 + desc_bytes.len() + jpeg.len());
            plain.extend_from_slice(b"TDR1");
            plain.extend_from_slice(&(desc_bytes.len() as u32).to_be_bytes());
            plain.extend_from_slice(&desc_bytes);
            plain.extend_from_slice(&jpeg);
            let ct = tokio::task::spawn_blocking(move || crypto::encrypt_bytes(&master, &plain))
                .await??;
            let len = ct.len() as i64;
            (ct, len)
        } else {
            (jpeg, pt_len)
        };
        let rend = wire::Rendition {
            channel_id: ctx.peer.id.bare_id_unchecked(),
            file_msg_id,
            content_msg_id,
            upload_uuid: upload_uuid.to_string(),
            kind: kind.to_string(),
            version: 1,
            size: stored,
            plaintext_size: pt_len,
            width: w as i32,
            height: h as i32,
            encrypted,
        };
        let mut reader = std::io::Cursor::new(&body);
        let uploaded = ctx
            .remote
            .client
            .upload_stream(&mut reader, body.len(), "rendition.jpg".to_string())
            .await?;
        let caption = wire::format(&ops::rendition_part(stored, &rend));
        ctx.remote
            .client
            .send_message(
                ctx.peer,
                InputMessage::new().document(uploaded).text(caption),
            )
            .await?;
        Ok(())
    }

    /// Download a file's "thumb" rendition into the local cache and tell
    /// the UI where to read it. Encrypted renditions decrypt with TDE1.
    /// Thumbnails fetch off the command loop — each is a bounded parallel
    /// task so an image-heavy folder doesn't queue N×2 sequential RTTs.
    pub(super) fn spawn_thumbnail_fetch(&mut self, file_id: &str) -> Result<()> {
        let Some((body_id, r)) = self
            .namespace
            .renditions
            .get(file_id)
            .and_then(|m| m.get("thumbnail"))
            .cloned()
        else {
            return Ok(());
        };
        let dir = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("tdrive-rs/thumbs");
        std::fs::create_dir_all(&dir)?;
        let out = dir.join(format!("{body_id}.bin"));
        if out.exists() {
            self.emit(Event::ThumbnailReady {
                file_id: file_id.to_string(),
                path: out,
            });
            return Ok(());
        }
        let client = self.client()?.clone();
        let peer = self.channel()?;
        let master = self.master_key.clone();
        let sem = self.thumb_sem.clone();
        let tx = self.tx.clone();
        let file_id = file_id.to_string();
        tokio::spawn(async move {
            let _permit = sem.acquire().await;
            let key = *master.lock().unwrap();
            if let Ok(msgs) = client.get_messages_by_id(peer, &[body_id as i32]).await {
                if let Some(media) = msgs.into_iter().flatten().next().and_then(|m| m.media()) {
                    let ok = if r.encrypted {
                        match key {
                            Some(key) => {
                                let staging = dir.join(format!("{body_id}.enc"));
                                let dl = client.download_media(&media, &staging).await;
                                if dl.is_ok() {
                                    let out2 = out.clone();
                                    let staging2 = staging.clone();
                                    let dec = tokio::task::spawn_blocking(move || {
                                        crypto::decrypt_file(&key, &staging2, &out2)?;
                                        // Encrypted renditions are TDR1-
                                        // enveloped — strip to the JPEG.
                                        let data = std::fs::read(&out2)?;
                                        if data.len() > 8 && &data[..4] == b"TDR1" {
                                            let n =
                                                u32::from_be_bytes(data[4..8].try_into().unwrap())
                                                    as usize;
                                            if 8 + n <= data.len() {
                                                std::fs::write(&out2, &data[8 + n..])?;
                                            }
                                        }
                                        Ok::<_, anyhow::Error>(())
                                    })
                                    .await;
                                    let _ = std::fs::remove_file(&staging);
                                    dec.map(|r| r.is_ok()).unwrap_or(false)
                                } else {
                                    false
                                }
                            }
                            None => false,
                        }
                    } else {
                        client.download_media(&media, &out).await.is_ok()
                    };
                    let _ = tx
                        .send(if ok {
                            Event::ThumbnailReady { file_id, path: out }
                        } else {
                            Event::ThumbnailFailed { file_id }
                        })
                        .await;
                }
            }
        });
        Ok(())
    }

    /// Download a viewable rendition for the in-app viewer: prefer the
    /// 1600px "preview" rendition, fall back to "thumbnail", then to the
    /// raw body when the file is itself an image. Emits PreviewReady or
    /// PreviewFailed once the cache file is readable.
    pub(super) fn spawn_preview_fetch(&mut self, file_id: &str) -> Result<()> {
        let Some(file) = self.namespace.files.get(file_id).cloned() else {
            return Ok(());
        };
        // (body msg id, is_rendition, encrypted)
        let source = self
            .namespace
            .renditions
            .get(file_id)
            .and_then(|m| {
                m.get("preview")
                    .or_else(|| m.get("thumbnail"))
                    .map(|(id, r)| (*id, true, r.encrypted))
            })
            .or_else(|| {
                if is_image_name(&file.name) {
                    Some((
                        if file.content_msg_id > 0 {
                            file.content_msg_id
                        } else {
                            file.msg_id
                        },
                        false,
                        file.encrypted,
                    ))
                } else {
                    None
                }
            });
        let Some((body_id, is_rendition, encrypted)) = source else {
            return Err(anyhow!("no previewable body for {}", file.name));
        };
        let dir = dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("tdrive-rs/previews");
        std::fs::create_dir_all(&dir)?;
        let out = dir.join(format!("{body_id}.img"));
        if out.exists() {
            self.emit(Event::PreviewReady {
                file_id: file_id.to_string(),
                path: out,
            });
            return Ok(());
        }
        if encrypted && self.master_key.lock().unwrap().is_none() {
            return Err(anyhow!("unlock the drive to view encrypted files"));
        }
        let client = self.client()?.clone();
        let peer = self.channel()?;
        let master = self.master_key.clone();
        let sem = self.thumb_sem.clone();
        let tx = self.tx.clone();
        let file_id = file_id.to_string();
        tokio::spawn(async move {
            let _permit = sem.acquire().await;
            let key = *master.lock().unwrap();
            let ok = (async {
                let msgs = client
                    .get_messages_by_id(peer, &[body_id as i32])
                    .await
                    .ok()?;
                let media = msgs.into_iter().flatten().next()?.media()?;
                let staging = dir.join(format!("{body_id}.part"));
                if encrypted {
                    let key = key?;
                    client.download_media(&media, &staging).await.ok()?;
                    let st = staging.clone();
                    let o = out.clone();
                    tokio::task::spawn_blocking(move || -> Result<()> {
                        crypto::decrypt_file(&key, &st, &o)?;
                        if is_rendition {
                            // TDR1-wrapped rendition — strip to the JPEG.
                            let data = std::fs::read(&o)?;
                            if data.len() > 8 && &data[..4] == b"TDR1" {
                                let n = u32::from_be_bytes(data[4..8].try_into().unwrap()) as usize;
                                if 8 + n <= data.len() {
                                    std::fs::write(&o, &data[8 + n..])?;
                                }
                            }
                        }
                        Ok(())
                    })
                    .await
                    .ok()?
                    .ok()?;
                    let _ = std::fs::remove_file(&staging);
                } else {
                    client.download_media(&media, &out).await.ok()?;
                }
                Some(())
            })
            .await
            .is_some();
            let _ = tx
                .send(if ok {
                    Event::PreviewReady { file_id, path: out }
                } else {
                    Event::PreviewFailed { file_id }
                })
                .await;
        });
        Ok(())
    }
}

/// Extract a mid-video frame to a temp JPEG via ffmpeg (TDrive's video
/// thumbnail approach — best-effort, returns None without ffmpeg).
pub(super) async fn extract_video_frame(source: &PathBuf) -> Option<PathBuf> {
    let out = std::env::temp_dir().join(format!("tdrive-vf-{}.jpg", uuid::Uuid::new_v4()));
    let src = source.clone();
    let out2 = out.clone();
    let status = tokio::task::spawn_blocking(move || {
        std::process::Command::new("ffmpeg")
            .args(["-i"])
            .arg(&src)
            .args(["-vf", "thumbnail", "-frames:v", "1", "-q:v", "5", "-y"])
            .arg(&out2)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
    })
    .await
    .ok()?
    .ok()?;
    (status.success() && out.exists()).then_some(out)
}

/// Recover the real message id after `send_message` reports `Empty{id:0}` —
/// the op reached the channel, so the newest message carrying our exact
/// header is the one just sent. Returns 0 when recovery fails.
pub(crate) fn is_video_name(name: &str) -> bool {
    matches!(
        name.rsplit('.')
            .next()
            .unwrap_or("")
            .to_lowercase()
            .as_str(),
        "mp4" | "mkv" | "webm" | "mov" | "m4v" | "avi" | "ts"
    )
}

pub(crate) fn is_image_name(name: &str) -> bool {
    matches!(
        name.rsplit('.')
            .next()
            .unwrap_or("")
            .to_lowercase()
            .as_str(),
        "jpg" | "jpeg" | "png" | "webp" | "gif"
    )
}
