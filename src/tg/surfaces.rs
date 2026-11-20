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
                let (msg_id, stored, pt_len, encrypted) = self.mount_put(&name, bytes).await?;
                let op = ops::file_commit(parent, name, stored, encrypted, pt_len, msg_id);
                self.send_and_project(&op).await
            }
            VfsOp::Replace { obj, bytes } => {
                // freplace rebinds an existing file to fresh content —
                // TDrive's mount overwrite path.
                let Some(d) = self.namespace.dirent(&obj) else {
                    return Err(anyhow!("no such object"));
                };
                if d.tombstoned {
                    return Err(anyhow!("deleted"));
                }
                let (msg_id, stored, pt_len, encrypted) = self.mount_put("", bytes).await?;
                let op = ops::file_replace(obj, stored, encrypted, pt_len, msg_id, d.revision);
                self.send_and_project(&op).await
            }
        }
    }

    /// Upload a mount-write body and return (msg_id, stored_size,
    /// plaintext_size, encrypted). TDE1-encrypts when the vault is
    /// unlocked; the message id recovers on `Empty{id:0}` like every
    /// other op send.
    async fn mount_put(&self, name: &str, bytes: Vec<u8>) -> Result<(i64, i64, i64, bool)> {
        let pt_len = bytes.len() as i64;
        let key = *self.master_key.lock().unwrap();
        let (body, stored) = match key {
            Some(k) => {
                let ct = tokio::task::spawn_blocking(move || crypto::encrypt_bytes(&k, &bytes))
                    .await??;
                let len = ct.len() as i64;
                (ct, len)
            }
            None => (bytes, pt_len),
        };
        let mut reader = std::io::Cursor::new(&body);
        let uploaded = self
            .client()?
            .upload_stream(&mut reader, body.len(), name.to_string())
            .await?;
        let msg_id = self
            .remote()?
            .send_doc_op(self.channel()?, uploaded, "")
            .await?;
        if msg_id <= 0 {
            anyhow::bail!("send outcome unknown");
        }
        Ok((msg_id, stored, pt_len, key.is_some()))
    }

    /// Mount the drive read-write at `dir` via FUSE — TDrive's mount surface
    /// for Linux file managers. Writes emit durable mount-write ops.
    #[cfg(unix)]
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

    #[cfg(not(unix))]
    pub(super) async fn mount_drive(&mut self, _dir: PathBuf) -> Result<()> {
        anyhow::bail!("mount requires FUSE (unix only)")
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
        let parts = self.namespace.body_parts(&file);
        if parts.is_empty() {
            return Err(anyhow!("no body messages for this file"));
        }
        stream::ensure_server(self.remote()?).await?;
        let url = stream::open(stream::Session {
            parts,
            plaintext_size: file.logical_size(),
            name: file.name,
            encrypted: file.encrypted,
        })?;
        self.emit(Event::StreamUrl { url });
        Ok(())
    }
}
