//! External surfaces — WebDAV write dispatch, FUSE mount, stream URLs.
use super::*;

impl Worker {
    /// Apply a WebDAV write through the durable mount-write ops TDrive's
    /// mount emits (dcommit/relocate/harddel/fcommit).
    pub(super) async fn dav_write(&mut self, op: VfsOp) -> Result<()> {
        match op {
            VfsOp::Mkdir { parent, name } => {
                let op = ops::folder_commit(parent, name.trim());
                self.send_and_project(&op).await
            }
            VfsOp::Delete { obj } => {
                // Mounted deletes go through the durable hard-delete flow.
                let Some(d) = self.namespace.dirent(&obj) else {
                    return Err(anyhow!("no such object"));
                };
                if d.tombstoned {
                    return Err(anyhow!("already deleted"));
                }
                // Collect body msg ids BEFORE applying — the projection
                // drops the records so ids are empty afterwards.
                let bodies = self.namespace.delete_body_msg_ids(&obj);
                let op = ops::hard_delete(&obj, d.revision);
                self.send_and_project(&op).await?;
                self.remote()?.delete_bodies(&bodies).await?;
                Ok(())
            }
            VfsOp::Move {
                obj,
                dst_parent,
                name,
            } => {
                let Some(d) = self.namespace.dirent(&obj) else {
                    return Err(anyhow!("no such object"));
                };
                if d.tombstoned {
                    return Err(anyhow!("deleted"));
                }
                let op = ops::relocate(obj, dst_parent, name, d.revision);
                self.send_and_project(&op).await
            }
            VfsOp::Put {
                parent,
                name,
                bytes,
            } => {
                // Single-body commit: upload the doc, then fcommit binds it —
                // the same two-step TDrive's writable mount performs.
                let pt_len = bytes.len() as i64;
                let encrypted = self.master_key.lock().unwrap().is_some();
                let (body, stored) = if encrypted {
                    let key = (*self.master_key.lock().unwrap()).unwrap();
                    let ct =
                        tokio::task::spawn_blocking(move || crypto::encrypt_bytes(&key, &bytes))
                            .await??;
                    let len = ct.len() as i64;
                    (ct, len)
                } else {
                    (bytes, pt_len)
                };
                let mut reader = std::io::Cursor::new(&body);
                let uploaded = self
                    .client()?
                    .upload_stream(&mut reader, body.len(), name.clone())
                    .await?;
                let msg = self
                    .client()?
                    .send_message(self.channel()?, InputMessage::new().document(uploaded))
                    .await?;
                let op = ops::file_commit(parent, name, stored, encrypted, pt_len, msg.id() as i64);
                self.send_and_project(&op).await
            }
            VfsOp::Replace { obj, bytes } => {
                // freplace rebinds an existing file to fresh content —
                // TDrive's mount overwrite path.
                let Some(d) = self.namespace.dirent(&obj) else {
                    return Err(anyhow!("no such object"));
                };
                let (rev, tomb) = (d.revision, d.tombstoned);
                if tomb {
                    return Err(anyhow!("deleted"));
                }
                let pt_len = bytes.len() as i64;
                let encrypted = self.master_key.lock().unwrap().is_some();
                let (body, stored) = if encrypted {
                    let key = (*self.master_key.lock().unwrap()).unwrap();
                    let ct =
                        tokio::task::spawn_blocking(move || crypto::encrypt_bytes(&key, &bytes))
                            .await??;
                    let len = ct.len() as i64;
                    (ct, len)
                } else {
                    (bytes, pt_len)
                };
                let mut reader = std::io::Cursor::new(&body);
                let uploaded = self
                    .client()?
                    .upload_stream(&mut reader, body.len(), String::new())
                    .await?;
                let msg = self
                    .client()?
                    .send_message(self.channel()?, InputMessage::new().document(uploaded))
                    .await?;
                let op = ops::file_replace(obj, stored, encrypted, pt_len, msg.id() as i64, rev);
                self.send_and_project(&op).await
            }
        }
    }

    /// Mount the drive read-write at `dir` via FUSE — TDrive's mount surface
    /// for Linux file managers. Writes emit durable mount-write ops.
    pub(super) async fn mount_drive(&mut self, dir: PathBuf) -> Result<()> {
        let fs = fusefs::TdFs::new(
            self.remote()?,
            tokio::runtime::Handle::current(),
            self.cmd_tx.clone(),
        );
        let session = fusefs::mount(fs, &dir)?;
        self.fuse_session = Some(session);
        self.emit(Event::MountState { dir: Some(dir) });
        Ok(())
    }

    /// Open a tokenized stream session for a file and hand the URL to the
    /// UI — TDrive's media-service model: the URL is the capability.
    pub(super) async fn stream(&mut self, obj: &str) -> Result<()> {
        let Some(file) = self.namespace.files.get(obj).cloned() else {
            return Err(anyhow!("not a file"));
        };
        if file.encrypted && self.master_key.lock().unwrap().is_none() {
            return Err(anyhow!("unlock the drive to stream encrypted files"));
        }
        let parts: Vec<(i64, i64)> = if !file.upload_uuid.is_empty() {
            self.namespace.part_sizes(&file.upload_uuid)
        } else {
            vec![(file.content_msg_id, file.size)]
        };
        if parts.is_empty() {
            return Err(anyhow!("no body messages for this file"));
        }
        stream::ensure_server(self.remote()?).await?;
        let url = stream::open(stream::Session {
            parts,
            plaintext_size: if file.encrypted {
                file.plaintext_size
            } else {
                file.size
            },
            name: file.name,
            encrypted: file.encrypted,
        })?;
        self.emit(Event::StreamUrl { url });
        Ok(())
    }
}
