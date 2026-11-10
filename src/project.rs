//! Namespace projection — folds an ordered stream of TDX1 ops into a
//! browsable file/folder tree. Simplified relative to TDrive's SQLite
//! projection but applies the same ops so listings stay compatible.

use crate::wire::{self, Op, OpType, OpTypeOrUnknown};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct FolderEntry {
    pub id: String,
    pub name: String,
    pub parent: String,
    pub tombstoned: bool,
}

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone, Default)]
struct PartGroup {
    /// pix -> (msg_id, size)
    parts: HashMap<i32, (i64, i64)>,
}

#[derive(Debug, Default, Clone)]
pub struct Namespace {
    pub folders: HashMap<String, FolderEntry>,
    pub files: HashMap<String, FileEntry>,
    parts: HashMap<String, PartGroup>,
    /// Highest message id observed while scanning.
    pub last_msg_id: i64,
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
        true
    }

    pub fn apply(&mut self, msg_id: i64, op: &Op) {
        let OpTypeOrUnknown::Known(t) = op.op_type else {
            return;
        };
        match t {
            OpType::FileUpload => {
                let id = wire::file_obj_id(msg_id);
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
                        upload_uuid: String::new(),
                        part_count: 0,
                        content_msg_id: 0,
                        revision: 1,
                    },
                );
            }
            OpType::FilePart => {
                self.parts
                    .entry(op.upload_uuid.clone())
                    .or_default()
                    .parts
                    .insert(op.part_index, (msg_id, op.file_size));
            }
            OpType::FileManifest => {
                let id = wire::file_obj_id(msg_id);
                let size = if op.file_size > 0 {
                    op.file_size
                } else {
                    self.parts
                        .get(&op.upload_uuid)
                        .map(|g| g.parts.values().map(|(_, s)| s).sum())
                        .unwrap_or(0)
                };
                self.files.insert(
                    id.clone(),
                    FileEntry {
                        id,
                        msg_id,
                        name: op.name.clone(),
                        size,
                        parent: op.parent.clone(),
                        upload_time: op.file_upload_time,
                        tombstoned: false,
                        encrypted: op.encrypted,
                        plaintext_size: op.plaintext_size,
                        upload_uuid: op.upload_uuid.clone(),
                        part_count: op.part_count,
                        content_msg_id: 0,
                        revision: 1,
                    },
                );
            }
            OpType::Meta => {
                if let Some(f) = self.files.get_mut(&op.obj) {
                    f.name = op.name.clone();
                    f.parent = op.parent.clone();
                    if op.file_size > 0 {
                        f.size = op.file_size;
                    }
                    if op.file_upload_time > 0 {
                        f.upload_time = op.file_upload_time;
                    }
                }
            }
            OpType::Mkdir | OpType::FolderCommit => {
                self.folders.insert(
                    op.obj.clone(),
                    FolderEntry {
                        id: op.obj.clone(),
                        name: op.name.clone(),
                        parent: op.parent.clone(),
                        tombstoned: false,
                    },
                );
            }
            OpType::Rename => self.rename(&op.obj, &op.name),
            OpType::Move => self.move_obj(&op.obj, &op.parent),
            OpType::Rmdir | OpType::TrashTree | OpType::HardDeleteTree => {
                self.tombstone(&op.obj)
            }
            OpType::Tomb => self.tombstone(&op.obj),
            OpType::FileCommit => {
                let id = wire::file_obj_id(msg_id);
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
                if let Some(f) = self.files.get_mut(&op.obj) {
                    f.content_msg_id = op.content_msg_id;
                    f.upload_uuid = op.upload_uuid.clone();
                    f.part_count = op.part_count;
                    if op.file_size > 0 {
                        f.size = op.file_size;
                    }
                    f.revision = op.expected_revision + 1;
                }
            }
            OpType::Relocate => {
                if !op.name.is_empty() {
                    self.rename(&op.obj, &op.name);
                }
                self.move_obj(&op.obj, &op.parent);
                if op.deleted_at > 0 {
                    self.tombstone(&op.obj);
                }
                if let Some(f) = self.files.get_mut(&op.obj) {
                    f.revision = op.expected_revision + 1;
                }
            }
            OpType::RestoreTree => {
                self.restore(&op.obj, &op.parent, &op.name);
            }
            OpType::EncConfig => {}
        }
    }

    fn rename(&mut self, obj: &str, name: &str) {
        if let Some(f) = self.files.get_mut(obj) {
            f.name = name.to_string();
        } else if let Some(d) = self.folders.get_mut(obj) {
            d.name = name.to_string();
        }
    }

    fn move_obj(&mut self, obj: &str, parent: &str) {
        if let Some(f) = self.files.get_mut(obj) {
            f.parent = parent.to_string();
        } else if let Some(d) = self.folders.get_mut(obj) {
            d.parent = parent.to_string();
        }
    }

    fn tombstone(&mut self, obj: &str) {
        if let Some(f) = self.files.get_mut(obj) {
            f.tombstoned = true;
        } else if let Some(d) = self.folders.get_mut(obj) {
            d.tombstoned = true;
            // Tombstone the subtree.
            let children: Vec<String> = self
                .folders
                .values()
                .filter(|c| c.parent == obj)
                .map(|c| c.id.clone())
                .collect();
            for c in children {
                self.tombstone(&c);
            }
            for f in self.files.values_mut() {
                if f.parent == obj {
                    f.tombstoned = true;
                }
            }
        }
    }

    fn restore(&mut self, obj: &str, parent: &str, name: &str) {
        if let Some(f) = self.files.get_mut(obj) {
            f.tombstoned = false;
            f.parent = parent.to_string();
            if !name.is_empty() {
                f.name = name.to_string();
            }
        } else if let Some(d) = self.folders.get_mut(obj) {
            d.tombstoned = false;
            d.parent = parent.to_string();
            if !name.is_empty() {
                d.name = name.to_string();
            }
        }
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
        assert_eq!(ns.list("d:1").len(), 0);
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
    }
}
