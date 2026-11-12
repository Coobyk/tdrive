//! Namespace mutations — every user-visible write op TDrive emits,
//! folded through `send_and_project` so local state matches the wire.
use super::*;

impl Worker {
    pub(super) async fn mkdir(&mut self, parent: String, name: String) -> Result<()> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(anyhow!("folder name required"));
        }
        let op = ops::mkdir(parent, name);
        self.send_and_project(&op).await
    }

    pub(super) async fn mkdir_id(&mut self, id: String, parent: String, name: String) -> Result<()> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(anyhow!("folder name required"));
        }
        let op = ops::folder_commit_with_id(id, parent, name);
        self.send_and_project(&op).await
    }

    pub(super) async fn rename(&mut self, obj: String, name: String) -> Result<()> {
        let op = ops::rename(obj, name);
        self.send_and_project(&op).await
    }

    pub(super) async fn move_obj(&mut self, obj: String, parent: String) -> Result<()> {
        let op = ops::move_obj(obj, parent);
        self.send_and_project(&op).await
    }

    /// Deep-copy `obj` under `parent`. Folders mint fresh ids and
    /// recurse; files emit a manifest/fcommit that rebinds the existing
    /// body — copies never re-upload bytes, they share messages with the
    /// original (delete_body_msg_ids keeps shared bodies alive).
    pub(super) async fn copy_obj(&mut self, obj: String, parent: String) -> Result<()> {
        if self
            .namespace
            .dirent(&obj)
            .map(|d| d.tombstoned)
            .unwrap_or(true)
        {
            return Err(anyhow!("no such object"));
        }
        self.copy_subtree(&obj, &parent).await
    }

    fn copy_subtree<'a>(
        &'a mut self,
        obj: &'a str,
        parent: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(self.copy_subtree_inner(obj, parent))
    }

    async fn copy_subtree_inner(&mut self, obj: &str, parent: &str) -> Result<()> {
        if let Some(d) = self.namespace.folders.get(obj).cloned() {
            let new_id = wire::new_folder_id();
            let name = self
                .namespace
                .next_free_sibling_name(parent, &d.name, true);
            let op = ops::folder_commit_with_id(new_id.clone(), parent, name);
            self.send_and_project(&op).await?;
            let children: Vec<String> = self
                .namespace
                .list(obj)
                .iter()
                .map(|e| e.id().to_string())
                .collect();
            for child in children {
                self.copy_subtree(&child, &new_id).await?;
            }
            return Ok(());
        }
        let Some(f) = self.namespace.files.get(obj).cloned() else {
            return Err(anyhow!("no such object"));
        };
        let name = self
            .namespace
            .next_free_sibling_name(parent, &f.name, false);
        let op = if !f.upload_uuid.is_empty() {
            // Multipart: a new manifest binding the same part group.
            ops::file_manifest(
                f.upload_uuid,
                parent,
                name,
                f.part_count,
                f.size,
                f.upload_time,
                f.encrypted,
                f.plaintext_size,
            )
        } else {
            // Single body: fcommit re-binding the same body message —
            // either an explicit cmid or the upload's own message.
            let cmid = if f.content_msg_id > 0 {
                f.content_msg_id
            } else {
                f.msg_id
            };
            ops::file_commit(parent, name, f.size, f.encrypted, f.plaintext_size, cmid)
        };
        self.send_and_project(&op).await
    }

    /// Move a live object to the trash — same `trash` op TDrive emits, with
    /// the deterministic op id and 30-day retention window.
    pub(super) async fn delete(&mut self, obj: String) -> Result<()> {
        let Some(d) = self.namespace.dirent(&obj) else {
            return Err(anyhow!("no such object"));
        };
        let (revision, tombstoned) = (d.revision, d.tombstoned);
        if tombstoned {
            return Err(anyhow!("item is already in the trash"));
        }
        let op = ops::trash(obj, revision);
        self.send_and_project(&op).await
    }

    /// Permanently purge a trashed object: `harddel` marker plus physical
    /// deletion of the file's Telegram body messages.
    pub(super) async fn purge(&mut self, obj: String) -> Result<()> {
        let Some(d) = self.namespace.dirent(&obj) else {
            return Err(anyhow!("no such object"));
        };
        let (revision, tombstoned) = (d.revision, d.tombstoned);
        if !tombstoned {
            return Err(anyhow!("only trashed items can be purged"));
        }
        // Collect body messages before the marker lands — purging a
        // folder must delete descendant media too; tombstoning alone
        // only hides them.
        let bodies = self.namespace.delete_body_msg_ids(&obj);
        let op = ops::hard_delete(obj, revision);
        self.send_and_project(&op).await?;
        if let Err(e) = self.remote()?.delete_bodies(&bodies).await {
            self.emit(Event::Error {
                context: "purge",
                message: format!("body delete: {e:#}"),
            });
        }
        Ok(())
    }

    pub(super) async fn empty_trash(&mut self) -> Result<()> {
        let ids: Vec<String> = self.namespace.trash.keys().cloned().collect();
        for obj in ids {
            if let Err(e) = self.purge(obj.clone()).await {
                self.emit(Event::Error {
                    context: "empty trash",
                    message: format!("{obj}: {e:#}"),
                });
            }
        }
        Ok(())
    }

    /// Restore from the trash — TDrive's rule: land at the original parent
    /// (root if it no longer exists) under a conflict-free sibling name.
    pub(super) async fn restore(&mut self, obj: String) -> Result<()> {
        let entry = self
            .namespace
            .trash
            .get(&obj)
            .cloned()
            .context("item is not in the trash")?;
        let Some(d) = self.namespace.dirent(&obj) else {
            return Err(anyhow!("no such object"));
        };
        let (revision, tombstoned) = (d.revision, d.tombstoned);
        if !tombstoned {
            return Err(anyhow!("item is not in the trash"));
        }
        let parent = if !entry.original_parent.is_empty()
            && self
                .namespace
                .folders
                .get(&entry.original_parent)
                .map(|d| !d.tombstoned)
                .unwrap_or(false)
        {
            entry.original_parent.clone()
        } else {
            String::new()
        };
        let name =
            self.namespace
                .next_free_sibling_name(&parent, &entry.original_name, entry.is_folder);
        let op = ops::restore(obj, parent, name, revision);
        self.send_and_project(&op).await
    }

    /// Adopt a raw channel attachment into the namespace with a `meta` op —
    /// the same op TDrive emits when claiming an unop'd document.
    pub(super) async fn adopt(&mut self, msg_id: i64, parent: String) -> Result<()> {
        let e = self
            .namespace
            .unadopted
            .iter()
            .find(|e| e.msg_id == msg_id)
            .cloned()
            .context("no adoptable attachment with that id")?;
        let (name, size, sender, date) = (e.name, e.size, e.sender_id, e.date);
        // TDrive's rule: only the uploader can manage a raw attachment.
        if self.actor_id != 0 && sender != self.actor_id {
            return Err(anyhow!("only the uploader can manage this file"));
        }
        let op = ops::meta(msg_id, parent, name, size, date);
        self.send_and_project(&op).await
    }
}
