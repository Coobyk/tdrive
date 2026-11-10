//! Writable FUSE mount — TDrive mounts drives into the OS filesystem; this
//! implements the same surface for Linux via `fuser`. Directories come from
//! the live namespace snapshot, file bytes stream through the shared
//! TDE1-aware range reader in `stream.rs`, and writes go through the
//! durable mount-write ops (`fcommit`/`freplace`/`dcommit`/`relocate`/
//! `harddel`) so other TDrive clients see identical metadata.

use crate::project::{Entry, FileEntry, Namespace};
use crate::stream;
use crate::tg::{Command, DavOp};
use anyhow::Result;
use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo,
    OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen,
    ReplyWrite, Request,
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
    /// Write ops route through the worker's durable mount-write commands.
    cmd: tokio::sync::mpsc::UnboundedSender<Command>,
    /// Buffered writes: fh → (parent dir id, name, bytes so far).
    write_bufs: Mutex<HashMap<u64, PendingWrite>>,
    next_fh: Mutex<u64>,
    uid: u32,
    gid: u32,
}

struct PendingWrite {
    parent: String,
    name: String,
    data: Vec<u8>,
    /// `Some` when overwriting an existing file (freplace path); `None`
    /// for newly created files (fcommit path).
    obj: Option<String>,
    /// Set once the write has been committed — prevents double-commit on
    /// flush-then-release.
    committed: bool,
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
        cmd: tokio::sync::mpsc::UnboundedSender<Command>,
    ) -> Self {
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        Self {
            ns,
            client,
            peer,
            master,
            rt,
            cmd,
            write_bufs: Mutex::new(HashMap::new()),
            next_fh: Mutex::new(1),
            inos: Mutex::new(InoMap::default()),
            uid,
            gid,
        }
    }

    /// Run a durable write op through the worker and wait for the result.
    fn write_op(&self, op: DavOp) -> Result<(), Errno> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd
            .send(Command::DavWrite { op, reply: tx })
            .map_err(|_| Errno::EIO)?;
        self.rt
            .block_on(rx)
            .map_err(|_| Errno::EIO)?
            .map_err(|_| Errno::EIO)
    }

    /// Commit a pending write buffer through fcommit/freplace.
    fn commit_pending(&self, fh: u64) -> Result<(), Errno> {
        let p = match self.write_bufs.lock().unwrap().get_mut(&fh) {
            Some(p) if !p.committed => {
                p.committed = true;
                PendingWrite {
                    parent: p.parent.clone(),
                    name: p.name.clone(),
                    data: std::mem::take(&mut p.data),
                    obj: p.obj.clone(),
                    committed: true,
                }
            }
            _ => return Ok(()),
        };
        self.write_op(match p.obj {
            Some(obj) => DavOp::Replace {
                obj,
                bytes: p.data,
            },
            None => DavOp::Put {
                parent: p.parent,
                name: p.name,
                bytes: p.data,
            },
        })
    }

    fn take_fh(&self) -> u64 {
        let mut g = self.next_fh.lock().unwrap();
        *g += 1;
        *g
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

impl TdFs {
    fn remove_entry(&self, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let Some(dir) = self.dir_id(parent.0) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let Some((ino, _)) = self.resolve_name(&dir, name) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let obj = self
            .inos
            .lock()
            .unwrap()
            .obj(ino)
            .unwrap_or_default()
            .to_string();
        match self.write_op(DavOp::Delete { obj }) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
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

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        match flags.acc_mode() {
            fuser::OpenAccMode::O_WRONLY | fuser::OpenAccMode::O_RDWR => {
                let Some(f) = self.file_for_ino(ino.0) else {
                    reply.error(Errno::ENOENT);
                    return;
                };
                let fh = self.take_fh();
                self.write_bufs.lock().unwrap().insert(
                    fh,
                    PendingWrite {
                        parent: f.parent.clone(),
                        name: f.name.clone(),
                        data: Vec::new(),
                        obj: Some(f.id.clone()),
                        committed: false,
                    },
                );
                reply.opened(FileHandle(fh), FopenFlags::empty());
            }
            _ => reply.opened(FileHandle(0), FopenFlags::empty()),
        }
    }

    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: fuser::ReplyCreate,
    ) {
        let Some(dir) = self.dir_id(parent.0) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let Some(name) = name.to_str().map(str::to_string) else {
            reply.error(Errno::EINVAL);
            return;
        };
        if self.resolve_name(&dir, OsStr::new(&name)).is_some() {
            reply.error(Errno::EEXIST);
            return;
        }
        let fh = self.take_fh();
        self.write_bufs.lock().unwrap().insert(
            fh,
            PendingWrite {
                parent: dir,
                name,
                data: Vec::new(),
                obj: None,
                committed: false,
            },
        );
        // Synthetic attr — the real entry appears once the buffer commits.
        let attr = FileAttr {
            ino: INodeNo(1 << 60 | fh),
            size: 0,
            blocks: 0,
            atime: SystemTime::now(),
            mtime: SystemTime::now(),
            ctime: SystemTime::now(),
            crtime: SystemTime::now(),
            kind: FileType::RegularFile,
            perm: 0o644,
            nlink: 1,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        };
        reply.created(&TTL, &attr, Generation(0), FileHandle(fh), FopenFlags::empty());
    }

    fn write(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: fuser::WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyWrite,
    ) {
        let mut bufs = self.write_bufs.lock().unwrap();
        let Some(p) = bufs.get_mut(&fh.0) else {
            reply.error(Errno::EBADF);
            return;
        };
        let off = offset as usize;
        let end = off + data.len();
        if end > p.data.len() {
            p.data.resize(end, 0);
        }
        p.data[off..end].copy_from_slice(data);
        reply.written(data.len() as u32);
    }

    fn flush(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _lock_owner: fuser::LockOwner,
        reply: ReplyEmpty,
    ) {
        match self.commit_pending(fh.0) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn fsync(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        match self.commit_pending(fh.0) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn mkdir(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let Some(dir) = self.dir_id(parent.0) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let Some(name) = name.to_str().map(str::to_string) else {
            reply.error(Errno::EINVAL);
            return;
        };
        match self.write_op(DavOp::Mkdir {
            parent: dir.clone(),
            name: name.clone(),
        }) {
            Ok(()) => {
                let ns = self.snapshot();
                if let Some(d) = ns
                    .folders
                    .values()
                    .find(|d| !d.tombstoned && d.parent == dir && d.name == name)
                {
                    let ino = self.inos.lock().unwrap().ino(&d.id);
                    reply.entry(&TTL, &self.dir_attr(ino, 0), Generation(0));
                } else {
                    reply.error(Errno::EIO);
                }
            }
            Err(e) => reply.error(e),
        }
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        self.remove_entry(parent, name, reply);
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        self.remove_entry(parent, name, reply);
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        _flags: fuser::RenameFlags,
        reply: ReplyEmpty,
    ) {
        let Some(dir) = self.dir_id(parent.0) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let Some(dst) = self.dir_id(newparent.0) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let Some((ino, _)) = self.resolve_name(&dir, name) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let obj = self
            .inos
            .lock()
            .unwrap()
            .obj(ino)
            .unwrap_or_default()
            .to_string();
        let name = newname.to_str().unwrap_or_default().to_string();
        match self.write_op(DavOp::Move {
            obj,
            dst_parent: dst,
            name,
        }) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }

    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<fuser::TimeOrNow>,
        _mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<SystemTime>,
        fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        // Truncate-on-write: clearing a pending buffer, or starting a
        // replace-write on an existing file.
        if let Some(sz) = size {
            if let Some(h) = fh {
                if let Some(p) = self.write_bufs.lock().unwrap().get_mut(&h.0) {
                    p.data.resize(sz as usize, 0);
                    reply.attr(&TTL, &FileAttr {
                        ino,
                        size: sz,
                        blocks: 0,
                        atime: SystemTime::now(),
                        mtime: SystemTime::now(),
                        ctime: SystemTime::now(),
                        crtime: SystemTime::now(),
                        kind: FileType::RegularFile,
                        perm: 0o644,
                        nlink: 1,
                        uid: self.uid,
                        gid: self.gid,
                        rdev: 0,
                        blksize: 4096,
                        flags: 0,
                    });
                    return;
                }
            }
        }
        // Metadata-only setattr — report current attrs (content is durable).
        if ino.0 == ROOT_INO {
            reply.attr(&TTL, &self.dir_attr(ROOT_INO, 0));
        } else if let Some(f) = self.file_for_ino(ino.0) {
            reply.attr(&TTL, &self.file_attr(ino.0, &f));
        } else {
            reply.error(Errno::ENOENT);
        }
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
        fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        _flush: bool,
        reply: fuser::ReplyEmpty,
    ) {
        if let Err(e) = self.commit_pending(fh.0) {
            reply.error(e);
            return;
        }
        self.write_bufs.lock().unwrap().remove(&fh.0);
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
