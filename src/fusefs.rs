//! Read-only FUSE mount — TDrive mounts drives into the OS filesystem; this
//! implements the same surface for Linux via `fuser`. Directories come from
//! the live namespace snapshot, file bytes stream through the shared
//! TDE1-aware range reader in `stream.rs`.

use crate::project::{Entry, FileEntry, Namespace};
use crate::stream;
use anyhow::Result;
use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo,
    OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, ReplyOpen, Request,
};
use grammers_client::session::types::PeerRef;
use grammers_client::Client;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TTL: Duration = Duration::from_secs(10);
const ROOT_INO: u64 = 1;

/// Filesystem backed by the shared namespace snapshot.
pub struct TdFs {
    ns: Arc<Mutex<Arc<Namespace>>>,
    client: Client,
    peer: PeerRef,
    master: Arc<Mutex<Option<[u8; crate::crypto::MASTER_KEY_LEN]>>>,
    rt: tokio::runtime::Handle,
    inos: Mutex<InoMap>,
    uid: u32,
    gid: u32,
}

#[derive(Default)]
struct InoMap {
    next: u64,
    obj_to_ino: HashMap<String, u64>,
    ino_to_obj: HashMap<u64, String>,
}

impl InoMap {
    fn ino(&mut self, obj: &str) -> u64 {
        if let Some(ino) = self.obj_to_ino.get(obj) {
            return *ino;
        }
        self.next += 1;
        let ino = ROOT_INO + self.next;
        self.obj_to_ino.insert(obj.to_string(), ino);
        self.ino_to_obj.insert(ino, obj.to_string());
        ino
    }
    fn obj(&self, ino: u64) -> Option<&str> {
        self.ino_to_obj.get(&ino).map(String::as_str)
    }
}

impl TdFs {
    pub fn new(
        ns: Arc<Mutex<Arc<Namespace>>>,
        client: Client,
        peer: PeerRef,
        master: Arc<Mutex<Option<[u8; crate::crypto::MASTER_KEY_LEN]>>>,
        rt: tokio::runtime::Handle,
    ) -> Self {
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        Self {
            ns,
            client,
            peer,
            master,
            rt,
            inos: Mutex::new(InoMap::default()),
            uid,
            gid,
        }
    }

    fn snapshot(&self) -> Arc<Namespace> {
        self.ns.lock().unwrap().clone()
    }

    /// Map an inode to a namespace directory id (root ino -> "").
    fn dir_id(&self, ino: u64) -> Option<String> {
        if ino == ROOT_INO {
            return Some(String::new());
        }
        self.inos.lock().unwrap().obj(ino).map(str::to_string)
    }

    fn dir_attr(&self, ino: u64, ts: i64) -> FileAttr {
        FileAttr {
            ino: INodeNo(ino),
            size: 0,
            blocks: 0,
            atime: time(ts),
            mtime: time(ts),
            ctime: time(ts),
            crtime: time(ts),
            kind: FileType::Directory,
            perm: 0o555,
            nlink: 2,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    fn file_attr(&self, ino: u64, f: &FileEntry) -> FileAttr {
        let size = if f.encrypted { f.plaintext_size } else { f.size };
        FileAttr {
            ino: INodeNo(ino),
            size: size.max(0) as u64,
            blocks: (size.max(0) as u64 + 511) / 512,
            atime: time(f.upload_time),
            mtime: time(f.upload_time),
            ctime: time(f.upload_time),
            crtime: time(f.upload_time),
            kind: FileType::RegularFile,
            perm: 0o444,
            nlink: 1,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    /// Resolve (parent dir id, entry name) to (ino, attr).
    fn resolve_name(&self, parent: &str, name: &OsStr) -> Option<(u64, FileAttr)> {
        let name = name.to_str()?;
        let ns = self.snapshot();
        for e in ns.list(parent) {
            match e {
                Entry::Folder(d) if d.name == name => {
                    let ino = self.inos.lock().unwrap().ino(&d.id);
                    return Some((ino, self.dir_attr(ino, 0)));
                }
                Entry::File(f) if f.name == name => {
                    let ino = self.inos.lock().unwrap().ino(&f.id);
                    return Some((ino, self.file_attr(ino, &f)));
                }
                _ => {}
            }
        }
        None
    }

    fn file_for_ino(&self, ino: u64) -> Option<FileEntry> {
        let obj = self.inos.lock().unwrap().obj(ino)?.to_string();
        let ns = self.snapshot();
        ns.files.get(&obj).cloned()
    }
}

impl Filesystem for TdFs {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let Some(dir) = self.dir_id(parent.0) else {
            reply.error(Errno::ENOENT);
            return;
        };
        match self.resolve_name(&dir, name) {
            Some((_ino, attr)) => reply.entry(&TTL, &attr, Generation(0)),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        if ino.0 == ROOT_INO {
            reply.attr(&TTL, &self.dir_attr(ROOT_INO, 0));
            return;
        }
        let obj = match self.inos.lock().unwrap().obj(ino.0).map(str::to_string) {
            Some(o) => o,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        let ns = self.snapshot();
        if let Some(_d) = ns.folders.get(&obj) {
            reply.attr(&TTL, &self.dir_attr(ino.0, 0));
        } else if let Some(f) = ns.files.get(&obj) {
            reply.attr(&TTL, &self.file_attr(ino.0, f));
        } else {
            reply.error(Errno::ENOENT);
        }
    }

    fn open(&self, _req: &Request, _ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        reply.opened(FileHandle(0), FopenFlags::empty());
    }

    fn opendir(&self, _req: &Request, _ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        reply.opened(FileHandle(0), FopenFlags::empty());
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let Some(dir) = self.dir_id(ino.0) else {
            reply.error(Errno::ENOTDIR);
            return;
        };
        let ns = self.snapshot();
        let mut items: Vec<(u64, FileType, String)> = vec![
            (ino.0, FileType::Directory, ".".into()),
            (ino.0, FileType::Directory, "..".into()),
        ];
        for e in ns.list(&dir) {
            match e {
                Entry::Folder(d) => {
                    let i = self.inos.lock().unwrap().ino(&d.id);
                    items.push((i, FileType::Directory, d.name));
                }
                Entry::File(f) => {
                    let i = self.inos.lock().unwrap().ino(&f.id);
                    items.push((i, FileType::RegularFile, f.name));
                }
            }
        }
        for (i, (ino_val, kind, name)) in items.iter().enumerate().skip(offset as usize) {
            if reply.add(INodeNo(*ino_val), (i + 1) as u64, *kind, name) {
                break;
            }
        }
        reply.ok();
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyData,
    ) {
        let Some(file) = self.file_for_ino(ino.0) else {
            reply.error(Errno::EBADF);
            return;
        };
        let total = if file.encrypted {
            file.plaintext_size
        } else {
            file.size
        }
        .max(0) as u64;
        if offset >= total || size == 0 {
            reply.data(&[]);
            return;
        }
        let ns = self.snapshot();
        let parts: Vec<(i64, i64)> = if !file.upload_uuid.is_empty() {
            ns.part_sizes(&file.upload_uuid)
        } else {
            vec![(file.content_msg_id, file.size)]
        };
        let client = self.client.clone();
        let peer = self.peer.clone();
        let master = self.master.clone();
        let encrypted = file.encrypted;
        let end = (offset + size as u64).min(total);
        let bytes = self.rt.block_on(async move {
            stream::read_range(&client, &peer, &parts, offset, end, encrypted, &master).await
        });
        match bytes {
            Ok(b) => reply.data(&b),
            Err(_) => reply.error(Errno::EIO),
        }
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        _flush: bool,
        reply: fuser::ReplyEmpty,
    ) {
        reply.ok();
    }

    fn releasedir(&self, _req: &Request, _ino: INodeNo, _fh: FileHandle, _flags: OpenFlags, reply: fuser::ReplyEmpty) {
        reply.ok();
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: fuser::ReplyStatfs) {
        reply.statfs(0, 0, 0, 0, 0, 4096, 255, 0);
    }
}

fn time(ts: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(ts.max(0) as u64)
}

/// Mount the drive read-only at `dir` on the worker's runtime.
pub fn mount(
    fs: TdFs,
    dir: &std::path::Path,
) -> Result<fuser::BackgroundSession> {
    std::fs::create_dir_all(dir)?;
    let cfg = fuser::Config::default();
    let session = fuser::Session::new(fs, dir, &cfg)?;
    Ok(session.spawn()?)
}
