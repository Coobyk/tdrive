//! Writable FUSE mount — TDrive mounts drives into the OS filesystem; this
//! implements the same surface for Linux via `fuser`. Directories come from
//! the live namespace snapshot, file bytes stream through the shared
//! TDE1-aware range reader in `stream.rs`, and writes go through the
//! durable mount-write ops (`fcommit`/`freplace`/`dcommit`/`relocate`/
//! `harddel`) so other TDrive clients see identical metadata.

use crate::ops::{MAX_PUT_BYTES, VfsOp};
use crate::project::{Entry, FileEntry, Namespace};
use crate::remote::Remote;
use crate::tg::Command;
use anyhow::Result;
use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, OpenFlags,
    ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite, Request,
};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TTL: Duration = Duration::from_secs(10);
const ROOT_INO: u64 = 1;

/// Filesystem backed by the shared namespace snapshot.
pub struct TdFs {
    remote: Remote,
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
    ino: u64,
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
    /// Synthetic inos handed out by mknod before the file exists.
    pending: HashMap<u64, (String, String)>,
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
    fn pending_create(&mut self, dir: String, name: String) -> u64 {
        self.next += 1;
        let ino = ROOT_INO + self.next;
        self.pending.insert(ino, (dir, name));
        ino
    }
    fn pending_take(&mut self, ino: u64) -> Option<(String, String)> {
        self.pending.remove(&ino)
    }
    /// Bind a committed object onto a synthetic ino.
    fn commit(&mut self, ino: u64, obj: &str) {
        self.pending.remove(&ino);
        self.obj_to_ino.insert(obj.to_string(), ino);
        self.ino_to_obj.insert(ino, obj.to_string());
    }
}

impl TdFs {
    pub fn new(
        remote: Remote,
        rt: tokio::runtime::Handle,
        cmd: tokio::sync::mpsc::UnboundedSender<Command>,
    ) -> Self {
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        Self {
            remote,
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
    fn write_op(&self, op: VfsOp) -> Result<(), Errno> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.cmd
            .send(Command::VfsWrite { op, reply: tx })
            .map_err(|_| Errno::EIO)?;
        self.rt
            .block_on(rx)
            .map_err(|_| Errno::EIO)?
            .map_err(|_| Errno::EIO)
    }

    /// Commit a pending write buffer through fcommit/freplace.
    fn commit_pending(&self, fh: u64) -> Result<(), Errno> {
        let p = match self.write_bufs.lock().unwrap().get(&fh) {
            Some(p) if !p.committed => PendingWrite {
                ino: p.ino,
                parent: p.parent.clone(),
                name: p.name.clone(),
                data: p.data.clone(),
                obj: p.obj.clone(),
                committed: true,
            },
            _ => return Ok(()),
        };
        let res = self.write_op(match &p.obj {
            Some(obj) => VfsOp::Replace {
                obj: obj.clone(),
                bytes: p.data.clone(),
            },
            None => VfsOp::Put {
                parent: p.parent.clone(),
                name: p.name.clone(),
                bytes: p.data.clone(),
            },
        });
        // Only mark committed after the op succeeded — otherwise later
        // flushes would skip retrying and the data would be lost.
        if res.is_ok() {
            // Rebind the synthetic ino onto the committed object, so
            // subsequent lookups see the real file, not a phantom.
            let obj_id =
                p.obj
                    .clone()
                    .or_else(|| match self.snapshot().find_child(&p.parent, &p.name) {
                        Some(crate::project::EntryRef::File(f)) => Some(f.id.clone()),
                        _ => None,
                    });
            if let Some(obj) = obj_id {
                self.inos.lock().unwrap().commit(p.ino, &obj);
            }
            if let Some(p) = self.write_bufs.lock().unwrap().get_mut(&fh) {
                p.committed = true;
            }
        }
        res
    }

    fn take_fh(&self) -> u64 {
        let mut g = self.next_fh.lock().unwrap();
        *g += 1;
        *g
    }

    fn snapshot(&self) -> Arc<Namespace> {
        self.remote.namespace()
    }

    /// Map an inode to a namespace directory id (root ino -> "").
    fn dir_id(&self, ino: u64) -> Option<String> {
        if ino == ROOT_INO {
            return Some(String::new());
        }
        self.inos.lock().unwrap().obj(ino).map(str::to_string)
    }

    fn attr(&self, ino: u64, kind: FileType, size: u64, ts: i64) -> FileAttr {
        let t = time(ts);
        FileAttr {
            ino: INodeNo(ino),
            size,
            blocks: (size + 511) / 512,
            atime: t,
            mtime: t,
            ctime: t,
            crtime: t,
            kind,
            perm: match kind {
                FileType::Directory => 0o555,
                _ => 0o444,
            },
            nlink: match kind {
                FileType::Directory => 2,
                _ => 1,
            },
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    fn dir_attr(&self, ino: u64, ts: i64) -> FileAttr {
        self.attr(ino, FileType::Directory, 0, ts)
    }

    fn file_attr(&self, ino: u64, f: &FileEntry) -> FileAttr {
        self.attr(
            ino,
            FileType::RegularFile,
            f.logical_size().max(0) as u64,
            f.upload_time,
        )
    }

    /// Attr for a node minted by mknod/create before its commit —
    /// reported as a fresh empty regular file.
    fn pending_attr(&self, ino: u64) -> FileAttr {
        let mut a = self.attr(ino, FileType::RegularFile, 0, 0);
        a.perm = 0o644;
        a.atime = SystemTime::now();
        a.mtime = SystemTime::now();
        a.ctime = SystemTime::now();
        a.crtime = SystemTime::now();
        a
    }

    /// Resolve (parent dir id, entry name) to (ino, attr).
    fn resolve_name(&self, parent: &str, name: &OsStr) -> Option<(u64, FileAttr)> {
        let name = name.to_str()?;
        let ns = self.snapshot();
        match ns.find_child(parent, name)? {
            crate::project::EntryRef::Folder(d) => {
                let ino = self.inos.lock().unwrap().ino(&d.id);
                Some((ino, self.dir_attr(ino, 0)))
            }
            crate::project::EntryRef::File(f) => {
                let ino = self.inos.lock().unwrap().ino(&f.id);
                Some((ino, self.file_attr(ino, f)))
            }
        }
    }

    fn file_for_ino(&self, ino: u64) -> Option<FileEntry> {
        let obj = self.inos.lock().unwrap().obj(ino)?.to_string();
        let ns = self.snapshot();
        ns.files.get(&obj).cloned()
    }
}

impl TdFs {
    fn remove_entry(&self, parent: INodeNo, name: &OsStr, dir_only: bool, reply: ReplyEmpty) {
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
        // unlink on a dir → EISDIR; rmdir on a file or non-empty dir →
        // ENOTDIR/ENOTEMPTY (never hard-delete a live subtree).
        let is_folder = crate::wire::is_folder_id(&obj);
        if !dir_only && is_folder {
            reply.error(Errno::EISDIR);
            return;
        }
        if dir_only {
            if !is_folder {
                reply.error(Errno::ENOTDIR);
                return;
            }
            let ns = self.remote.namespace();
            if !ns.list(&obj).is_empty() {
                reply.error(Errno::ENOTEMPTY);
                return;
            }
        }
        match self.write_op(VfsOp::Delete { obj }) {
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
                // Pending mknod/create inos aren't committed to the
                // namespace yet — report them as fresh empty files.
                if self.inos.lock().unwrap().pending.contains_key(&ino.0) {
                    reply.attr(&TTL, &self.pending_attr(ino.0));
                } else {
                    reply.error(Errno::ENOENT);
                }
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
                // Either an existing file (replace-write) or a node created
                // via mknod that hasn't been opened yet.
                let (ino, parent, name, obj) = match self.file_for_ino(ino.0) {
                    Some(f) => (ino.0, f.parent.clone(), f.name.clone(), Some(f.id.clone())),
                    None => match self.inos.lock().unwrap().pending_take(ino.0) {
                        Some((dir, name)) => (ino.0, dir, name, None),
                        None => {
                            reply.error(Errno::ENOENT);
                            return;
                        }
                    },
                };
                let fh = self.take_fh();
                self.write_bufs.lock().unwrap().insert(
                    fh,
                    PendingWrite {
                        ino,
                        parent,
                        name,
                        data: Vec::new(),
                        obj,
                        committed: false,
                    },
                );
                reply.opened(FileHandle(fh), FopenFlags::empty());
            }
            _ => reply.opened(FileHandle(0), FopenFlags::empty()),
        }
    }

    fn mknod(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _rdev: u32,
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
        if self.resolve_name(&dir, OsStr::new(&name)).is_some() {
            reply.error(Errno::EEXIST);
            return;
        }
        let ino = self.inos.lock().unwrap().pending_create(dir, name);
        reply.entry(&TTL, &self.pending_attr(ino), Generation(0));
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
        let ino = self
            .inos
            .lock()
            .unwrap()
            .pending_create(dir.clone(), name.clone());
        let fh = self.take_fh();
        self.write_bufs.lock().unwrap().insert(
            fh,
            PendingWrite {
                ino,
                parent: dir,
                name,
                data: Vec::new(),
                obj: None,
                committed: false,
            },
        );
        let attr = self.pending_attr(ino);
        reply.created(
            &TTL,
            &attr,
            Generation(0),
            FileHandle(fh),
            FopenFlags::empty(),
        );
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
        // Writes are buffered whole — a sparse write at a huge offset
        // would otherwise allocate unbounded memory.
        if end > MAX_PUT_BYTES {
            reply.error(Errno::EFBIG);
            return;
        }
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
        match self.write_op(VfsOp::Mkdir {
            parent: dir.clone(),
            name: name.clone(),
        }) {
            Ok(()) => {
                let ns = self.snapshot();
                if let Some(crate::project::EntryRef::Folder(d)) = ns.find_child(&dir, &name) {
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
        self.remove_entry(parent, name, false, reply);
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        self.remove_entry(parent, name, true, reply);
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
        match self.write_op(VfsOp::Move {
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
                    if sz > MAX_PUT_BYTES as u64 {
                        reply.error(Errno::EFBIG);
                        return;
                    }
                    p.data.resize(sz as usize, 0);
                    reply.attr(
                        &TTL,
                        &FileAttr {
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
                        },
                    );
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
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyData,
    ) {
        // Read-back on an open write handle serves the buffered data.
        {
            let bufs = self.write_bufs.lock().unwrap();
            if let Some(p) = bufs.get(&fh.0) {
                let off = offset as usize;
                if off >= p.data.len() {
                    reply.data(&[]);
                } else {
                    let end = (off + size as usize).min(p.data.len());
                    reply.data(&p.data[off..end]);
                }
                return;
            }
        }
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
        let parts = ns.body_parts(&file);
        let remote = self.remote.clone();
        let encrypted = file.encrypted;
        let end = (offset + size as u64).min(total);
        let bytes = self
            .rt
            .block_on(async move { remote.read_range(&parts, offset, end, encrypted).await });
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

    fn releasedir(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _flags: OpenFlags,
        reply: fuser::ReplyEmpty,
    ) {
        reply.ok();
    }

    fn forget(&self, _req: &Request, ino: INodeNo, _nlookup: u64) {
        // Bound the inode map — forget clears lookup references.
        let mut m = self.inos.lock().unwrap();
        if let Some(obj) = m.ino_to_obj.remove(&ino.0) {
            m.obj_to_ino.remove(&obj);
        }
        m.pending.remove(&ino.0);
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: fuser::ReplyStatfs) {
        reply.statfs(1 << 30, 1 << 30, 1 << 30, 1 << 20, 1 << 20, 4096, 255, 0);
    }
}

fn time(ts: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(ts.max(0) as u64)
}

/// Mount the drive read-write at `dir` on the worker's runtime.
pub fn mount(fs: TdFs, dir: &std::path::Path) -> Result<fuser::BackgroundSession> {
    std::fs::create_dir_all(dir)?;
    let cfg = fuser::Config::default();
    let session = fuser::Session::new(fs, dir, &cfg)?;
    Ok(session.spawn()?)
}
