//! Namespace projection — folds an ordered stream of TDX1 ops into a
//! browsable file/folder tree. Ports TDrive's `backend/projection/apply.go`
//! semantics in memory: first-write-wins mkdir/parts, tombstoned=0 guards on
//! rename/move, move cycle rejection, file-meta upsert merging, versioned-op
//! dedup by op_id, expected-revision compare-and-swap, trash entries and
//! subtree tombstone/restore semantics.

use crate::wire::{self, Op, OpType, OpTypeOrUnknown};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderEntry {
    pub id: String,
    pub name: String,
    pub parent: String,
    pub tombstoned: bool,
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// Object id, `f:<msg_id>` of the uploading/manifest message.
    pub id: String,
    /// Message id carrying the op that made this file visible.
    pub msg_id: i64,
    pub name: String,
    /// Stored (on-Telegram) size in bytes.
    pub size: i64,
    pub parent: String,
    pub upload_time: i64,
    pub tombstoned: bool,
    pub encrypted: bool,
    pub plaintext_size: i64,
    /// Multipart upload grouping uuid, if the file was uploaded in parts.
    pub upload_uuid: String,
    pub part_count: i32,
    /// Single-body content message id (fcommit with cmid), else 0 meaning
    /// the body is `msg_id` itself or a multipart group.
    pub content_msg_id: i64,
    pub revision: i64,
}

/// Restorable-trash record created by a `trash` op.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrashEntry {
    pub object_id: String,
    pub is_folder: bool,
    pub original_parent: String,
    pub original_name: String,
    pub original_revision: i64,
    pub deleted_at: i64,
    pub purge_after: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PartGroup {
    /// pix -> (msg_id, size)
    parts: HashMap<i32, (i64, i64)>,
}

/// Persisted projection snapshot (excludes the encfg op — it re-parses from
/// history on the next incremental scan anyway).
#[derive(Debug, Serialize, Deserialize)]
pub struct NamespaceSnapshot {
    pub folders: HashMap<String, FolderEntry>,
    pub files: HashMap<String, FileEntry>,
    parts: HashMap<String, PartGroup>,
    pub trash: HashMap<String, TrashEntry>,
    seen_ops: HashSet<String>,
    pub last_msg_id: i64,
    pub encfg: Option<Op>,
    #[serde(default)]
    pub renditions: HashMap<String, HashMap<String, (i64, wire::Rendition)>>,
}

#[derive(Debug, Default, Clone)]
pub struct Namespace {
    pub folders: HashMap<String, FolderEntry>,
    pub files: HashMap<String, FileEntry>,
    parts: HashMap<String, PartGroup>,
    /// Restorable objects (trash view), keyed by object id.
    pub trash: HashMap<String, TrashEntry>,
    /// Applied/skipped versioned-op ids (replay dedup).
    seen_ops: HashSet<String>,
    /// Highest message id observed while scanning.
    pub last_msg_id: i64,
    /// Latest `encfg` op seen — the drive's encryption vault, if configured.
    pub encfg: Option<Op>,
    /// file id → kind ("thumb"|"preview") → (body msg id, descriptor)
    pub renditions: HashMap<String, HashMap<String, (i64, wire::Rendition)>>,
    /// Channel messages carrying a document but no TDX1 op — adoptable raw
    /// attachments in shared drives. (msg_id, name, size)
    pub unadopted: Vec<(i64, String, i64)>,
}

/// A visible listing row.
#[derive(Debug, Clone)]
pub enum Entry {
    Folder(FolderEntry),
    File(FileEntry),
}

impl Namespace {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one message. `msg_id` is the Telegram message id, `raw` is the
    /// full text/caption. Returns true when the message carried a valid op.
    pub fn apply_message(&mut self, msg_id: i64, raw: &str) -> bool {
        if msg_id > self.last_msg_id {
            self.last_msg_id = msg_id;
        }
        let op = match wire::parse(raw) {
            Ok(op) => op,
            Err(_) => return false,
        };
        self.apply(msg_id, &op);
        self.unadopted.retain(|(id, _, _)| *id != msg_id);
        true
    }

    /// Record a channel document message carrying no TDX1 header — a raw
    /// attachment that can be adopted with a `meta` op.
    pub fn record_unadopted(&mut self, msg_id: i64, name: String, size: i64) {
        if msg_id > self.last_msg_id {
            self.last_msg_id = msg_id;
        }
        if self.unadopted.iter().all(|(id, _, _)| *id != msg_id) {
            self.unadopted.push((msg_id, name, size));
        }
    }

    pub fn apply(&mut self, msg_id: i64, op: &Op) {
        let OpTypeOrUnknown::Known(t) = op.op_type else {
            return;
        };
        // Versioned (v1-envelope) ops: validate envelope, dedup by op_id.
        if matches!(
            t,
            OpType::FileCommit
                | OpType::FileReplace
                | OpType::FolderCommit
                | OpType::Relocate
                | OpType::TrashTree
                | OpType::HardDeleteTree
                | OpType::RestoreTree
        ) {
            if op.protocol_version != 1
                || op.op_id.is_empty()
                || op.op_id.len() > 128
                || op.op_id.chars().any(|c| c.is_control() || c == '|')
            {
                return;
            }
            if !self.seen_ops.insert(op.op_id.clone()) {
                return;
            }
        }
        match t {
            OpType::FileUpload => self.apply_file_meta(msg_id, op, msg_id),
            OpType::Meta => {
                let Some(file_msg_id) = file_msg_id(&op.obj) else {
                    return;
                };
                self.apply_file_meta(file_msg_id, op, file_msg_id)
            }
            OpType::FilePart => {
                // Rendition envelope: binds this part message to a logical
                // file as a thumbnail/preview body.
                if !op.rendition.is_empty() {
                    if let Some(r) = wire::Rendition::decode(&op.rendition) {
                        self.renditions
                            .entry(wire::file_obj_id(r.file_msg_id))
                            .or_default()
                            .entry(r.kind.clone())
                            .or_insert((msg_id, r));
                    }
                    return;
                }
                if op.part_index < 0 || op.upload_uuid.trim().is_empty() {
                    return;
                }
                // First-write-wins, like applyFilePart.
                self.parts
                    .entry(op.upload_uuid.clone())
                    .or_default()
                    .parts
                    .entry(op.part_index)
                    .or_insert((msg_id, op.file_size));
            }
            OpType::FileManifest => {
                if op.upload_uuid.trim().is_empty()
                    || op.part_count <= 0
                    || op.name.trim().is_empty()
                    || !valid_parent(&op.parent)
                {
                    return;
                }
                let id = wire::file_obj_id(msg_id);
                let entry = self.files.entry(id.clone()).or_insert_with(|| FileEntry {
                    id: id.clone(),
                    msg_id,
                    name: String::new(),
                    size: 0,
                    parent: String::new(),
                    upload_time: 0,
                    tombstoned: false,
                    encrypted: false,
                    plaintext_size: 0,
                    upload_uuid: String::new(),
                    part_count: 0,
                    content_msg_id: 0,
                    revision: 1,
                });
                entry.name = op.name.clone();
                entry.parent = op.parent.clone();
                if op.file_size > 0 {
                    entry.size = op.file_size;
                }
                if op.file_upload_time > 0 {
                    entry.upload_time = op.file_upload_time;
                }
                if op.encrypted {
                    entry.encrypted = true;
                }
                if op.plaintext_size > 0 {
                    entry.plaintext_size = op.plaintext_size;
                }
                entry.upload_uuid = op.upload_uuid.clone();
                entry.part_count = op.part_count;
            }
            OpType::Mkdir => {
                if !wire::is_folder_id(&op.obj)
                    || op.name.trim().is_empty()
                    || !valid_parent(&op.parent)
                {
                    return;
                }
                // First-mkdir-wins.
                self.folders.entry(op.obj.clone()).or_insert_with(|| FolderEntry {
                    id: op.obj.clone(),
                    name: op.name.clone(),
                    parent: op.parent.clone(),
                    tombstoned: false,
                    revision: 1,
                });
            }
            OpType::Rename => {
                if op.name.trim().is_empty() {
                    return;
                }
                self.rename(&op.obj, &op.name);
            }
            OpType::Move => {
                if !valid_parent(&op.parent) {
                    return;
                }
                self.move_obj(&op.obj, &op.parent);
            }
            OpType::Rmdir => {
                if !wire::is_folder_id(&op.obj) {
                    return;
                }
                if let Some(d) = self.folders.get_mut(&op.obj) {
                    if !d.tombstoned {
                        d.tombstoned = true;
                        d.revision += 1;
                    }
                }
            }
            OpType::Tomb => {
                if !wire::is_file_id(&op.obj) {
                    return;
                }
                if let Some(f) = self.files.get_mut(&op.obj) {
                    if !f.tombstoned {
                        f.tombstoned = true;
                        f.revision += 1;
                    }
                }
            }
            OpType::FolderCommit => {
                if !wire::is_folder_id(&op.obj)
                    || op.name.trim().is_empty()
                    || !valid_parent(&op.parent)
                    || self.folders.contains_key(&op.obj)
                {
                    return;
                }
                self.folders.insert(
                    op.obj.clone(),
                    FolderEntry {
                        id: op.obj.clone(),
                        name: op.name.clone(),
                        parent: op.parent.clone(),
                        tombstoned: false,
                        revision: 1,
                    },
                );
            }
            OpType::FileCommit => {
                let id = wire::file_obj_id(msg_id);
                if op.name.trim().is_empty()
                    || !valid_parent(&op.parent)
                    || self.files.contains_key(&id)
                {
                    return;
                }
                self.files.insert(
                    id.clone(),
                    FileEntry {
                        id,
                        msg_id,
                        name: op.name.clone(),
                        size: op.file_size,
                        parent: op.parent.clone(),
                        upload_time: op.file_upload_time,
                        tombstoned: false,
                        encrypted: op.encrypted,
                        plaintext_size: op.plaintext_size,
                        upload_uuid: op.upload_uuid.clone(),
                        part_count: op.part_count,
                        content_msg_id: op.content_msg_id,
                        revision: 1,
                    },
                );
            }
            OpType::FileReplace => {
                let Some(f) = self.files.get_mut(&op.obj) else {
                    return;
                };
                if f.tombstoned || f.revision != op.expected_revision {
                    return;
                }
                f.content_msg_id = op.content_msg_id;
                f.upload_uuid = op.upload_uuid.clone();
                f.part_count = op.part_count;
                if op.file_size > 0 {
                    f.size = op.file_size;
                }
                if op.file_upload_time > 0 {
                    f.upload_time = op.file_upload_time;
                }
                if op.encrypted {
                    f.encrypted = true;
                }
                if op.plaintext_size > 0 {
                    f.plaintext_size = op.plaintext_size;
                }
                f.revision += 1;
            }
            OpType::Relocate => {
                if op.name.trim().is_empty() || !valid_parent(&op.parent) {
                    return;
                }
                if op.deleted_at > 0 {
                    // Relocate-into-trash (mount write path).
                    self.apply_trash(op, /*use_entry_parent*/ false);
                    return;
                }
                if !self.destination_obj_trash(&op.destination_obj, op.expected_destination_revision) {
                    return;
                }
                if let Some((_, live_rev)) = self.dirent(&op.obj) {
                    if live_rev != op.expected_revision {
                        return;
                    }
                } else {
                    return;
                }
                let obj = op.obj.clone();
                self.rename(&obj, &op.name);
                if self.folders.contains_key(&obj) && would_cycle(self, &obj, &op.parent) {
                    return;
                }
                self.move_obj(&obj, &op.parent);
            }
            OpType::TrashTree => {
                if op.expected_revision <= 0
                    || op.deleted_at <= 0
                    || op.purge_after <= op.deleted_at
                {
                    return;
                }
                self.apply_trash(op, true);
            }
            OpType::RestoreTree => {
                if op.expected_revision <= 0 {
                    return;
                }
                self.apply_restore(op);
            }
            OpType::HardDeleteTree => {
                if op.expected_revision <= 0 {
                    return;
                }
                self.apply_hard_delete(op);
            }
            OpType::EncConfig => {
                self.encfg = Some(op.clone());
            }
        }
    }

    /// Legacy `f`/`meta` upsert: name and parent always overwrite; size,
    /// upload time, encryption fields only replace when present (>0).
    fn apply_file_meta(&mut self, file_msg_id: i64, op: &Op, msg_id: i64) {
        if op.name.trim().is_empty() || !valid_parent(&op.parent) {
            return;
        }
        let id = wire::file_obj_id(file_msg_id);
        let entry = self.files.entry(id.clone()).or_insert_with(|| FileEntry {
            id,
            msg_id,
            name: String::new(),
            size: 0,
            parent: String::new(),
            upload_time: 0,
            tombstoned: false,
            encrypted: false,
            plaintext_size: 0,
            upload_uuid: String::new(),
            part_count: 0,
            content_msg_id: file_msg_id,
            revision: 1,
        });
        entry.name = op.name.clone();
        entry.parent = op.parent.clone();
        if op.file_size > 0 {
            entry.size = op.file_size;
        }
        if op.file_upload_time > 0 {
            entry.upload_time = op.file_upload_time;
        }
        if op.encrypted {
            entry.encrypted = true;
        }
        if op.plaintext_size > 0 {
            entry.plaintext_size = op.plaintext_size;
        }
    }

    /// Returns (is_folder, revision) for the object when present (live or
    /// tombstoned).
    fn dirent(&self, obj: &str) -> Option<(bool, i64)> {
        if let Some(f) = self.files.get(obj) {
            return Some((false, f.revision));
        }
        if let Some(d) = self.folders.get(obj) {
            return Some((true, d.revision));
        }
        None
    }

    /// CAS a relocate destination tombstone: `dst` objects must be live with
    /// the expected revision, then are trashed in place.
    fn destination_obj_trash(&mut self, dst: &str, drev: i64) -> bool {
        if dst.is_empty() {
            return true;
        }
        let Some((is_folder, rev)) = self.dirent(dst) else {
            return true; // missing destination is not an obstacle
        };
        if drev > 0 && rev != drev {
            return false;
        }
        let id = dst.to_string();
        if is_folder {
            self.tombstone_subtree_folder(&id);
        } else if let Some(f) = self.files.get_mut(&id) {
            f.tombstoned = true;
            f.revision += 1;
        }
        true
    }

    /// `trash` apply: CAS on live dirent revision, record trash entry,
    /// tombstone object + subtree, each revision+1.
    fn apply_trash(&mut self, op: &Op, use_current_state: bool) {
        let Some((is_folder, live_parent, live_name, live_rev)) = (|| {
            if let Some(f) = self.files.get(&op.obj) {
                (!f.tombstoned).then(|| (false, f.parent.clone(), f.name.clone(), f.revision))
            } else {
                self.folders
                    .get(&op.obj)
                    .filter(|d| !d.tombstoned)
                    .map(|d| (true, d.parent.clone(), d.name.clone(), d.revision))
            }
        })() else {
            return;
        };
        if live_rev != op.expected_revision {
            return;
        }
        let (orig_parent, orig_name) = if use_current_state {
            (live_parent, live_name)
        } else {
            (live_parent, live_name)
        };
        self.trash.insert(
            op.obj.clone(),
            TrashEntry {
                object_id: op.obj.clone(),
                is_folder,
                original_parent: orig_parent,
                original_name: orig_name,
                original_revision: live_rev,
                deleted_at: op.deleted_at,
                purge_after: op.purge_after,
            },
        );
        let obj = op.obj.clone();
        if is_folder {
            self.tombstone_subtree_folder(&obj);
        } else if let Some(f) = self.files.get_mut(&obj) {
            f.tombstoned = true;
            f.revision += 1;
        }
    }

    /// `untrash` apply: CAS on tombstoned revision, restore subtree, land at
    /// the caller-resolved parent/name, drop the trash entry.
    fn apply_restore(&mut self, op: &Op) {
        let Some((is_folder, live_rev)) = (|| {
            if let Some(f) = self.files.get(&op.obj) {
                f.tombstoned.then_some((false, f.revision))
            } else {
                self.folders
                    .get(&op.obj)
                    .and_then(|d| d.tombstoned.then_some((true, d.revision)))
            }
        })() else {
            return;
        };
        if live_rev != op.expected_revision {
            return;
        }
        let obj = op.obj.clone();
        if is_folder {
            self.restore_subtree_folder(&obj);
        } else if let Some(f) = self.files.get_mut(&obj) {
            f.tombstoned = false;
            f.revision += 1;
        }
        // Land at the resolved destination.
        if let Some(d) = self.folders.get_mut(&obj) {
            d.parent = op.parent.clone();
            if !op.name.is_empty() {
                d.name = op.name.clone();
            }
        } else if let Some(f) = self.files.get_mut(&obj) {
            f.parent = op.parent.clone();
            if !op.name.is_empty() {
                f.name = op.name.clone();
            }
        }
        self.trash.remove(&obj);
    }

    /// `harddel` apply: CAS on tombstoned revision, permanently drop the
    /// object (+ subtree) and its trash entry.
    fn apply_hard_delete(&mut self, op: &Op) {
        let tomb_rev = if let Some(f) = self.files.get(&op.obj) {
            f.tombstoned.then_some(f.revision)
        } else if let Some(d) = self.folders.get(&op.obj) {
            d.tombstoned.then_some(d.revision)
        } else {
            return; // only tombstoned objects are purgeable
        };
        if tomb_rev != Some(op.expected_revision) {
            return;
        }
        let obj = op.obj.clone();
        let upload_uuids: Vec<String> = self
            .subtree_files(&obj)
            .iter()
            .filter_map(|f| {
                if f.upload_uuid.is_empty() {
                    None
                } else {
                    Some(f.upload_uuid.clone())
                }
            })
            .collect();
        if self.folders.remove(&obj).is_some() {
            // Remove descendant folders/files recursively.
            let mut queue: Vec<String> = self
                .folders
                .values()
                .filter(|d| d.parent == obj)
                .map(|d| d.id.clone())
                .collect();
            while let Some(fid) = queue.pop() {
                self.folders.remove(&fid);
                queue.extend(
                    self.folders
                        .values()
                        .filter(|d| d.parent == fid)
                        .map(|d| d.id.clone()),
                );
            }
            self.files.retain(|_, f| {
                let mut anc = f.parent.clone();
                let mut guard = 0;
                while !anc.is_empty() && guard < 64 {
                    guard += 1;
                    if anc == obj {
                        return false;
                    }
                    anc = self
                        .folders
                        .get(&anc)
                        .map(|d| d.parent.clone())
                        .unwrap_or_default();
                }
                true
            });
        }
        self.files.remove(&obj);
        for u in upload_uuids {
            self.parts.remove(&u);
        }
        self.trash.remove(&obj);
    }

    fn subtree_files(&self, folder_id: &str) -> Vec<&FileEntry> {
        let mut ids = HashSet::new();
        ids.insert(folder_id.to_string());
        let mut changed = true;
        let mut guard = 0;
        while changed && guard < 64 {
            changed = false;
            guard += 1;
            for d in self.folders.values() {
                if ids.contains(&d.parent) && ids.insert(d.id.clone()) {
                    changed = true;
                }
            }
        }
        self.files
            .values()
            .filter(|f| ids.contains(&f.parent))
            .collect()
    }

    fn tombstone_subtree_folder(&mut self, root: &str) {
        if let Some(d) = self.folders.get_mut(root) {
            d.tombstoned = true;
            d.revision += 1;
        }
        let children: Vec<String> = self
            .folders
            .values()
            .filter(|c| c.parent == root && !c.tombstoned)
            .map(|c| c.id.clone())
            .collect();
        for c in children {
            self.tombstone_subtree_folder(&c);
        }
        for f in self.files.values_mut() {
            if f.parent == root && !f.tombstoned {
                f.tombstoned = true;
                f.revision += 1;
            }
        }
    }

    fn restore_subtree_folder(&mut self, root: &str) {
        if let Some(d) = self.folders.get_mut(root) {
            d.tombstoned = false;
            d.revision += 1;
        }
        let children: Vec<String> = self
            .folders
            .values()
            .filter(|c| c.parent == root && c.tombstoned)
            .map(|c| c.id.clone())
            .collect();
        for c in children {
            self.restore_subtree_folder(&c);
        }
        for f in self.files.values_mut() {
            if f.parent == root && f.tombstoned {
                f.tombstoned = false;
                f.revision += 1;
            }
        }
    }

    fn rename(&mut self, obj: &str, name: &str) {
        if let Some(f) = self.files.get_mut(obj) {
            if !f.tombstoned {
                f.name = name.to_string();
                f.revision += 1;
            }
        } else if let Some(d) = self.folders.get_mut(obj) {
            if !d.tombstoned {
                d.name = name.to_string();
                d.revision += 1;
            }
        }
    }

    fn move_obj(&mut self, obj: &str, parent: &str) {
        if let Some(f) = self.files.get_mut(obj) {
            if !f.tombstoned {
                f.parent = parent.to_string();
                f.revision += 1;
            }
        } else {
            let live = self
                .folders
                .get(obj)
                .map(|d| !d.tombstoned)
                .unwrap_or(false);
            if live && !would_cycle(self, obj, parent) {
                if let Some(d) = self.folders.get_mut(obj) {
                    d.parent = parent.to_string();
                    d.revision += 1;
                }
            }
        }
    }

    /// Tombstoned trash entries, most recently deleted first.
    pub fn list_trash(&self) -> Vec<TrashEntry> {
        let mut v: Vec<_> = self.trash.values().cloned().collect();
        v.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at).then(a.object_id.cmp(&b.object_id)));
        v
    }

    /// Live entries directly under `parent` ("" = root), folders first then
    /// files, each sorted by name.
    pub fn list(&self, parent: &str) -> Vec<Entry> {
        let mut folders: Vec<_> = self
            .folders
            .values()
            .filter(|f| !f.tombstoned && f.parent == parent)
            .cloned()
            .collect();
        folders.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        let mut files: Vec<_> = self
            .files
            .values()
            .filter(|f| !f.tombstoned && f.parent == parent)
            .cloned()
            .collect();
        files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        folders
            .into_iter()
            .map(Entry::Folder)
            .chain(files.into_iter().map(Entry::File))
            .collect()
    }

    /// Part message ids for a multipart file, ordered by part index.
    /// Returns None when the group is incomplete.
    pub fn part_msg_ids(&self, uuid: &str, count: i32) -> Option<Vec<i64>> {
        let group = self.parts.get(uuid)?;
        let mut ids = Vec::with_capacity(count as usize);
        for i in 0..count {
            ids.push(group.parts.get(&i)?.0);
        }
        Some(ids)
    }

    /// Message ids carrying a file's bytes, in read order.
    pub fn body_msg_ids(&self, file: &FileEntry) -> Vec<i64> {
        if file.content_msg_id > 0 {
            vec![file.content_msg_id]
        } else if !file.upload_uuid.is_empty() && file.part_count > 0 {
            self.part_msg_ids(&file.upload_uuid, file.part_count)
                .unwrap_or_default()
        } else {
            vec![file.msg_id]
        }
    }

    /// Live dirent (object id, kind flag, revision, tombstoned, parent, name).
    pub fn dirent_info(&self, obj: &str) -> Option<(bool, i64, bool, String, String)> {
        if let Some(f) = self.files.get(obj) {
            return Some((false, f.revision, f.tombstoned, f.parent.clone(), f.name.clone()));
        }
        self.folders
            .get(obj)
            .map(|d| (true, d.revision, d.tombstoned, d.parent.clone(), d.name.clone()))
    }

    /// Body message ids for every file under a folder subtree.
    pub fn subtree_body_msg_ids(&self, folder_id: &str) -> Vec<i64> {
        let mut out = Vec::new();
        for f in self.subtree_files(folder_id) {
            out.extend(self.body_msg_ids(f));
        }
        out
    }

    /// First free sibling name under `parent`, TDrive's restore rule: the
    /// name if free, else "name (k)" — counter before the extension for
    /// files ("photo (2).jpg").
    pub fn next_free_sibling_name(&self, parent: &str, name: &str, is_folder: bool) -> String {
        let (stem, ext) = if is_folder {
            (name.to_string(), String::new())
        } else {
            match name.rfind('.') {
                Some(i) if i > 0 => (name[..i].to_string(), name[i..].to_string()),
                _ => (name.to_string(), String::new()),
            }
        };
        for attempt in 1..=1000 {
            let candidate = if attempt == 1 {
                name.to_string()
            } else {
                format!("{stem} ({attempt}){ext}")
            };
            let key = candidate.to_lowercase();
            let taken = self
                .list(parent)
                .iter()
                .any(|e| match e {
                    Entry::Folder(d) => d.name.to_lowercase() == key,
                    Entry::File(f) => f.name.to_lowercase() == key,
                });
            if !taken {
                return candidate;
            }
        }
        name.to_string()
    }

    /// Snapshot for durable storage — mirrors TDrive's persisted projection.
    pub fn snapshot(&self) -> NamespaceSnapshot {
        NamespaceSnapshot {
            folders: self.folders.clone(),
            files: self.files.clone(),
            parts: self.parts.clone(),
            trash: self.trash.clone(),
            seen_ops: self.seen_ops.clone(),
            last_msg_id: self.last_msg_id,
            encfg: self.encfg.clone(),
            renditions: self.renditions.clone(),
        }
    }

    pub fn restore_snapshot(s: NamespaceSnapshot) -> Self {
        Self {
            folders: s.folders,
            files: s.files,
            parts: s.parts,
            trash: s.trash,
            seen_ops: s.seen_ops,
            last_msg_id: s.last_msg_id,
            encfg: s.encfg,
            renditions: s.renditions,
            unadopted: Vec::new(),
        }
    }

    /// Breadcrumb path from root to a folder id.
    pub fn folder_path<'a>(&'a self, mut id: &'a str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut guard = 0;
        while !id.is_empty() && guard < 64 {
            guard += 1;
            if let Some(d) = self.folders.get(id) {
                out.push((d.id.clone(), d.name.clone()));
                id = &d.parent;
            } else {
                break;
            }
        }
        out.reverse();
        out
    }
}

fn valid_parent(p: &str) -> bool {
    p.is_empty() || wire::is_folder_id(p)
}

fn file_msg_id(obj: &str) -> Option<i64> {
    let raw = obj.strip_prefix("f:")?;
    if raw.is_empty() || !raw.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n: i64 = raw.parse().ok()?;
    (n > 0).then_some(n)
}

/// Move cycle check: walking ancestors of `parent` must not reach `obj`.
fn would_cycle(ns: &Namespace, obj: &str, parent: &str) -> bool {
    if obj == parent {
        return true;
    }
    let mut cur = parent.to_string();
    let mut guard = 0;
    while !cur.is_empty() && guard < 64 {
        guard += 1;
        if cur == obj {
            return true;
        }
        cur = ns
            .folders
            .get(&cur)
            .map(|d| d.parent.clone())
            .unwrap_or_default();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(s: &str) -> Op {
        wire::parse(s).unwrap()
    }

    #[test]
    fn single_file_listing() {
        let mut ns = Namespace::new();
        ns.apply(10, &op("TDX1|t=f|p=|n=a.txt|sz=5"));
        let items = ns.list("");
        assert_eq!(items.len(), 1);
        match &items[0] {
            Entry::File(f) => {
                assert_eq!(f.id, "f:10");
                assert_eq!(f.name, "a.txt");
                assert_eq!(ns.body_msg_ids(f), vec![10]);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn multipart_file() {
        let mut ns = Namespace::new();
        ns.apply(11, &op("TDX1|t=part|u=u1|pix=0|sz=100"));
        ns.apply(12, &op("TDX1|t=part|u=u1|pix=1|sz=50"));
        ns.apply(13, &op("TDX1|t=manifest|u=u1|p=|n=big|pc=2|sz=150"));
        let items = ns.list("");
        assert_eq!(items.len(), 1);
        match &items[0] {
            Entry::File(f) => {
                assert_eq!(f.size, 150);
                assert_eq!(ns.body_msg_ids(f), vec![11, 12]);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn folders_and_tombstone() {
        let mut ns = Namespace::new();
        ns.apply(1, &op("TDX1|t=mkdir|obj=d:1|p=|n=sub"));
        ns.apply(2, &op("TDX1|t=f|p=d:1|n=x|sz=1"));
        assert_eq!(ns.list("").len(), 1);
        assert_eq!(ns.list("d:1").len(), 1);
        ns.apply(3, &op("TDX1|t=rmdir|obj=d:1"));
        assert_eq!(ns.list("").len(), 0);
        // rmdir tombstones only the folder — children become unreachable
        // but are not individually trashed.
        assert_eq!(ns.files.get("f:2").unwrap().tombstoned, false);
        assert!(ns.trash.is_empty());
    }

    #[test]
    fn rename_and_move() {
        let mut ns = Namespace::new();
        ns.apply(1, &op("TDX1|t=mkdir|obj=d:1|p=|n=sub"));
        ns.apply(2, &op("TDX1|t=f|p=|n=x|sz=1"));
        ns.apply(3, &op("TDX1|t=rename|obj=f:2|n=y"));
        ns.apply(4, &op("TDX1|t=move|obj=f:2|p=d:1"));
        assert_eq!(ns.list("").len(), 1); // just the folder
        let Entry::File(f) = &ns.list("d:1")[0] else {
            panic!()
        };
        assert_eq!(f.name, "y");
        assert_eq!(f.revision, 3); // create=1, rename=2, move=3
    }

    #[test]
    fn trash_cas_and_restore() {
        let mut ns = Namespace::new();
        ns.apply(1, &op("TDX1|t=mkdir|obj=d:a|p=|n=dir"));
        ns.apply(2, &op("TDX1|t=f|p=d:a|n=x.txt|sz=5"));
        // Wrong revision → refused.
        ns.apply(3, &op("TDX1|t=trash|v=1|oid=bad|obj=d:a|rev=9|del=100|purge=200"));
        assert_eq!(ns.list("").len(), 1);
        // Correct revision: folder at rev 1 → CAS trashes whole subtree.
        ns.apply(4, &op("TDX1|t=trash|v=1|oid=t1|obj=d:a|rev=1|del=100|purge=200"));
        assert!(ns.files.get("f:2").unwrap().tombstoned);
        assert_eq!(ns.trash.get("d:a").unwrap().original_name, "dir");
        assert_eq!(ns.list_trash().len(), 1);
        // Replay of the same op_id is a no-op.
        ns.apply(5, &op("TDX1|t=trash|v=1|oid=t1|obj=d:a|rev=1|del=100|purge=200"));
        // Restore: folder is at revision 2 after trash.
        ns.apply(6, &op("TDX1|t=untrash|v=1|oid=r1|obj=d:a|p=|n=dir|rev=2"));
        assert!(!ns.folders.get("d:a").unwrap().tombstoned);
        assert!(!ns.files.get("f:2").unwrap().tombstoned);
        assert!(ns.trash.is_empty());
    }

    #[test]
    fn hard_delete_removes() {
        let mut ns = Namespace::new();
        ns.apply(1, &op("TDX1|t=f|p=|n=x|sz=1"));
        ns.apply(2, &op("TDX1|t=trash|v=1|oid=t|obj=f:1|rev=1|del=10|purge=40"));
        ns.apply(3, &op("TDX1|t=harddel|v=1|oid=p|obj=f:1|rev=2"));
        assert!(ns.files.get("f:1").is_none());
        assert!(ns.trash.is_empty());
        // harddel on a live object is rejected.
        ns.apply(4, &op("TDX1|t=f|p=|n=y|sz=1"));
        ns.apply(5, &op("TDX1|t=harddel|v=1|oid=p2|obj=f:4|rev=1"));
        assert!(ns.files.get("f:4").is_some());
    }

    #[test]
    fn move_cycle_rejected() {
        let mut ns = Namespace::new();
        ns.apply(1, &op("TDX1|t=mkdir|obj=d:a|p=|n=a"));
        ns.apply(2, &op("TDX1|t=mkdir|obj=d:b|p=d:a|n=b"));
        ns.apply(3, &op("TDX1|t=move|obj=d:a|p=d:b"));
        assert_eq!(ns.folders.get("d:a").unwrap().parent, "");
    }

    #[test]
    fn rename_on_tombstoned_ignored() {
        let mut ns = Namespace::new();
        ns.apply(1, &op("TDX1|t=f|p=|n=x|sz=1"));
        ns.apply(2, &op("TDX1|t=tomb|obj=f:1"));
        ns.apply(3, &op("TDX1|t=rename|obj=f:1|n=y"));
        assert_eq!(ns.files.get("f:1").unwrap().name, "x");
    }

    #[test]
    fn free_sibling_name() {
        let mut ns = Namespace::new();
        ns.apply(1, &op("TDX1|t=f|p=|n=photo.jpg|sz=1"));
        assert_eq!(ns.next_free_sibling_name("", "photo.jpg", false), "photo (2).jpg");
        assert_eq!(ns.next_free_sibling_name("", "other", false), "other");
    }
}
