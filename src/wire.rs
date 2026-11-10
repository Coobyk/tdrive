//! TDX1 wire format — compatible with TDrive's `backend/projection/wire.go`.
//!
//! Every operation is a `TDX1|t=<type>|key=value|...` header stored as the
//! first line of a Telegram message text or document caption.

use std::collections::HashMap;
use std::fmt::Write as _;
use thiserror::Error;

pub const WIRE_PREFIX: &str = "TDX1";
pub const FOLDER_ID_PREFIX: &str = "d:";
pub const FILE_ID_PREFIX: &str = "f:";
/// Empty string marks the drive root.
pub const ROOT_PARENT: &str = "";
const MAX_WIRE_UNIX_TS: i64 = 253_402_300_799;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WireError {
    #[error("wire: header missing")]
    MissingHeader,
    #[error("wire: unknown version")]
    BadVersion,
    #[error("wire: unknown op type")]
    BadOpType,
    #[error("wire: malformed header")]
    Malformed,
    #[error("wire: bad object id")]
    BadObject,
    #[error("wire: bad parent id")]
    BadParent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpType {
    FileUpload,
    Mkdir,
    Rename,
    Move,
    Rmdir,
    Tomb,
    Meta,
    EncConfig,
    FilePart,
    FileManifest,
    FileCommit,
    FileReplace,
    FolderCommit,
    Relocate,
    TrashTree,
    HardDeleteTree,
    RestoreTree,
}

impl OpType {
    fn as_str(self) -> &'static str {
        match self {
            OpType::FileUpload => "f",
            OpType::Mkdir => "mkdir",
            OpType::Rename => "rename",
            OpType::Move => "move",
            OpType::Rmdir => "rmdir",
            OpType::Tomb => "tomb",
            OpType::Meta => "meta",
            OpType::EncConfig => "encfg",
            OpType::FilePart => "part",
            OpType::FileManifest => "manifest",
            OpType::FileCommit => "fcommit",
            OpType::FileReplace => "freplace",
            OpType::FolderCommit => "dcommit",
            OpType::Relocate => "relocate",
            OpType::TrashTree => "trash",
            OpType::HardDeleteTree => "harddel",
            OpType::RestoreTree => "untrash",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "f" => OpType::FileUpload,
            "mkdir" => OpType::Mkdir,
            "rename" => OpType::Rename,
            "move" => OpType::Move,
            "rmdir" => OpType::Rmdir,
            "tomb" => OpType::Tomb,
            "meta" => OpType::Meta,
            "encfg" => OpType::EncConfig,
            "part" => OpType::FilePart,
            "manifest" => OpType::FileManifest,
            "fcommit" => OpType::FileCommit,
            "freplace" => OpType::FileReplace,
            "dcommit" => OpType::FolderCommit,
            "relocate" => OpType::Relocate,
            "trash" => OpType::TrashTree,
            "harddel" => OpType::HardDeleteTree,
            "untrash" => OpType::RestoreTree,
            _ => return None,
        })
    }
}

/// A parsed TDX1 operation. Fields mirror TDrive's `projection.Op`; the ones
/// the MVP never reads (renditions, encryption config bodies) are kept raw.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Op {
    pub op_type: OpTypeOrUnknown,
    pub protocol_version: i32,
    pub op_id: String,

    pub obj: String,
    pub parent: String,
    pub name: String,

    pub expected_revision: i64,
    pub expected_destination_revision: i64,

    pub content_msg_id: i64,
    pub content_hash: String,

    pub overwrite: bool,
    pub destination_obj: String,
    pub deleted_at: i64,
    pub purge_after: i64,
    pub retained_until: i64,

    pub file_size: i64,
    pub file_upload_time: i64,

    pub encrypted: bool,
    pub plaintext_size: i64,
    pub encryption_version: i32,

    pub upload_uuid: String,
    pub part_index: i32,
    pub part_count: i32,
}

/// Unknown op types parse as `Unknown` so a newer TDrive's messages are
/// skipped instead of breaking the scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpTypeOrUnknown {
    Known(OpType),
    #[default]
    Unknown,
}

/// First line of a message/caption — the only place a header can live.
pub fn extract_header_line(raw: &str) -> &str {
    raw.split('\n').next().unwrap_or("")
}

fn query_unescape(s: &str) -> Result<String, WireError> {
    // application/x-www-form-urlencoded decoding: '+' -> ' ', %XX -> byte.
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                if i + 2 >= bytes.len() {
                    return Err(WireError::Malformed);
                }
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                    .map_err(|_| WireError::Malformed)?;
                let v = u8::from_str_radix(hex, 16).map_err(|_| WireError::Malformed)?;
                out.push(v);
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8(out).map_err(|_| WireError::Malformed)
}

fn query_escape(s: &str) -> String {
    // Go's url.QueryEscape: unreserved [A-Za-z0-9-_.~] kept, space -> "+",
    // everything else percent-encoded (uppercase hex).
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}

fn set_obj(op: &mut Op, raw: &str, prefix: &str) -> Result<(), WireError> {
    if raw.is_empty() || !raw.starts_with(prefix) {
        return Err(WireError::BadObject);
    }
    op.obj = raw.to_string();
    Ok(())
}

fn set_obj_any(op: &mut Op, raw: &str) -> Result<(), WireError> {
    if raw.is_empty()
        || (!raw.starts_with(FOLDER_ID_PREFIX) && !raw.starts_with(FILE_ID_PREFIX))
    {
        return Err(WireError::BadObject);
    }
    op.obj = raw.to_string();
    Ok(())
}

fn set_parent(op: &mut Op, raw: &str) -> Result<(), WireError> {
    if raw.is_empty() {
        op.parent = ROOT_PARENT.to_string();
        return Ok(());
    }
    if !raw.starts_with(FOLDER_ID_PREFIX) {
        return Err(WireError::BadParent);
    }
    op.parent = raw.to_string();
    Ok(())
}

fn set_name(op: &mut Op, raw: &str) -> Result<(), WireError> {
    if raw.is_empty() {
        return Err(WireError::Malformed);
    }
    op.name = query_unescape(raw)?;
    Ok(())
}

fn set_positive_i64(dst: &mut i64, raw: &str) -> Result<(), WireError> {
    let n: i64 = raw.parse().map_err(|_| WireError::Malformed)?;
    if n <= 0 {
        return Err(WireError::Malformed);
    }
    *dst = n;
    Ok(())
}

fn set_writable_envelope(op: &mut Op, kv: &HashMap<String, String>) -> Result<(), WireError> {
    let version: i32 = kv
        .get("v")
        .and_then(|v| v.parse().ok())
        .ok_or(WireError::Malformed)?;
    if version != 1 {
        return Err(WireError::Malformed);
    }
    let raw_id = kv.get("oid").cloned().unwrap_or_default();
    let op_id = query_unescape(&raw_id)?;
    if op_id.is_empty() {
        return Err(WireError::Malformed);
    }
    op.protocol_version = version;
    op.op_id = op_id;
    Ok(())
}

fn set_content_reference(op: &mut Op, kv: &HashMap<String, String>) -> Result<(), WireError> {
    if let Some(raw) = kv.get("cmid") {
        if !raw.is_empty() {
            set_positive_i64(&mut op.content_msg_id, raw)?;
        }
    }
    if let Some(raw) = kv.get("u") {
        if raw.is_empty() {
            return Err(WireError::Malformed);
        }
        op.upload_uuid = raw.clone();
    }
    if let Some(raw) = kv.get("pc") {
        let n: i32 = raw.parse().map_err(|_| WireError::Malformed)?;
        if n <= 0 {
            return Err(WireError::Malformed);
        }
        op.part_count = n;
    }
    if let Some(raw) = kv.get("hash") {
        op.content_hash = query_unescape(raw)?;
    }
    if op.content_msg_id <= 0 && (op.upload_uuid.is_empty() || op.part_count <= 0) {
        return Err(WireError::Malformed);
    }
    if op.content_msg_id > 0 && (!op.upload_uuid.is_empty() || op.part_count != 0) {
        return Err(WireError::Malformed);
    }
    Ok(())
}

pub fn parse(raw: &str) -> Result<Op, WireError> {
    let header = extract_header_line(raw).trim();
    if header.is_empty() {
        return Err(WireError::MissingHeader);
    }
    let parts: Vec<&str> = header.split('|').collect();
    if parts.len() < 2 {
        return Err(WireError::Malformed);
    }
    if parts[0] != WIRE_PREFIX {
        return Err(WireError::BadVersion);
    }

    let mut kv: HashMap<String, String> = HashMap::with_capacity(parts.len() - 1);
    for p in &parts[1..] {
        let eq = p.find('=').ok_or(WireError::Malformed)?;
        if eq == 0 {
            return Err(WireError::Malformed);
        }
        kv.insert(p[..eq].to_string(), p[eq + 1..].to_string());
    }

    let t = kv.get("t").ok_or(WireError::Malformed)?;
    let mut op = Op::default();
    let op_type = OpType::from_str(t).ok_or(WireError::BadOpType)?;
    op.op_type = OpTypeOrUnknown::Known(op_type);

    let get = |k: &str| kv.get(k).map(String::as_str).unwrap_or("");

    match op_type {
        OpType::FileUpload => {
            set_parent(&mut op, get("p"))?;
            set_name(&mut op, get("n"))?;
        }
        OpType::Meta => {
            set_obj(&mut op, get("obj"), FILE_ID_PREFIX)?;
            set_parent(&mut op, get("p"))?;
            set_name(&mut op, get("n"))?;
        }
        OpType::Mkdir => {
            set_obj(&mut op, get("obj"), FOLDER_ID_PREFIX)?;
            set_parent(&mut op, get("p"))?;
            set_name(&mut op, get("n"))?;
        }
        OpType::Rename => {
            set_obj_any(&mut op, get("obj"))?;
            set_name(&mut op, get("n"))?;
        }
        OpType::Move => {
            set_obj_any(&mut op, get("obj"))?;
            set_parent(&mut op, get("p"))?;
        }
        OpType::Rmdir => set_obj(&mut op, get("obj"), FOLDER_ID_PREFIX)?,
        OpType::Tomb => set_obj(&mut op, get("obj"), FILE_ID_PREFIX)?,
        OpType::FilePart => {
            let u = get("u");
            if u.is_empty() {
                return Err(WireError::Malformed);
            }
            op.upload_uuid = u.to_string();
            let pix: i32 = get("pix").parse().map_err(|_| WireError::Malformed)?;
            if pix < 0 {
                return Err(WireError::Malformed);
            }
            op.part_index = pix;
        }
        OpType::FileManifest => {
            let u = get("u");
            if u.is_empty() {
                return Err(WireError::Malformed);
            }
            op.upload_uuid = u.to_string();
            set_parent(&mut op, get("p"))?;
            set_name(&mut op, get("n"))?;
            let pc: i32 = get("pc").parse().map_err(|_| WireError::Malformed)?;
            if pc <= 0 {
                return Err(WireError::Malformed);
            }
            op.part_count = pc;
        }
        OpType::EncConfig => {
            // Fields kept raw; the MVP does not decrypt.
            for key in ["salt", "kdf", "wrap", "check"] {
                if get(key).is_empty() {
                    return Err(WireError::Malformed);
                }
            }
            if let Some(hint) = kv.get("hint") {
                op.name = query_unescape(hint)?; // reuse name slot for hint
            }
            op.encryption_version = kv
                .get("cv")
                .and_then(|s| s.parse().ok())
                .unwrap_or(1);
        }
        OpType::FileCommit => {
            set_writable_envelope(&mut op, &kv)?;
            set_parent(&mut op, get("p"))?;
            set_name(&mut op, get("n"))?;
            set_content_reference(&mut op, &kv)?;
        }
        OpType::FolderCommit => {
            set_writable_envelope(&mut op, &kv)?;
            set_obj(&mut op, get("obj"), FOLDER_ID_PREFIX)?;
            set_parent(&mut op, get("p"))?;
            set_name(&mut op, get("n"))?;
        }
        OpType::FileReplace => {
            set_writable_envelope(&mut op, &kv)?;
            set_obj(&mut op, get("obj"), FILE_ID_PREFIX)?;
            set_positive_i64(&mut op.expected_revision, get("rev"))?;
            set_content_reference(&mut op, &kv)?;
            set_positive_i64(&mut op.retained_until, get("retain"))?;
        }
        OpType::Relocate => {
            set_writable_envelope(&mut op, &kv)?;
            set_obj_any(&mut op, get("obj"))?;
            set_parent(&mut op, get("p"))?;
            set_name(&mut op, get("n"))?;
            set_positive_i64(&mut op.expected_revision, get("rev"))?;
            op.overwrite = get("ow") == "1";
            if !get("dst").is_empty() {
                set_obj_any(&mut op, get("dst"))?;
                op.destination_obj = op.obj.clone();
            }
            if let Some(raw) = kv.get("drev") {
                if !raw.is_empty() {
                    set_positive_i64(&mut op.expected_destination_revision, raw)?;
                }
            }
            if let Some(raw) = kv.get("del") {
                if !raw.is_empty() {
                    set_positive_i64(&mut op.deleted_at, raw)?;
                }
            }
            if let Some(raw) = kv.get("purge") {
                if !raw.is_empty() {
                    set_positive_i64(&mut op.purge_after, raw)?;
                }
            }
        }
        OpType::TrashTree => {
            set_writable_envelope(&mut op, &kv)?;
            set_obj_any(&mut op, get("obj"))?;
            set_positive_i64(&mut op.expected_revision, get("rev"))?;
            set_positive_i64(&mut op.deleted_at, get("del"))?;
            set_positive_i64(&mut op.purge_after, get("purge"))?;
        }
        OpType::HardDeleteTree => {
            set_writable_envelope(&mut op, &kv)?;
            set_obj_any(&mut op, get("obj"))?;
            set_positive_i64(&mut op.expected_revision, get("rev"))?;
        }
        OpType::RestoreTree => {
            set_writable_envelope(&mut op, &kv)?;
            set_obj_any(&mut op, get("obj"))?;
            set_parent(&mut op, get("p"))?;
            set_name(&mut op, get("n"))?;
            set_positive_i64(&mut op.expected_revision, get("rev"))?;
        }
    }

    if let Some(s) = kv.get("sz") {
        let n: i64 = s.parse().map_err(|_| WireError::Malformed)?;
        if n < 0 {
            return Err(WireError::Malformed);
        }
        op.file_size = n;
    }
    if let Some(s) = kv.get("ts") {
        let n: i64 = s.parse().map_err(|_| WireError::Malformed)?;
        if !(0..=MAX_WIRE_UNIX_TS).contains(&n) {
            return Err(WireError::Malformed);
        }
        op.file_upload_time = n;
    }
    if let Some(s) = kv.get("enc") {
        if s == "1" {
            op.encrypted = true;
            op.encryption_version = 1;
        }
    }
    if let Some(s) = kv.get("ev") {
        op.encryption_version = s.parse().map_err(|_| WireError::Malformed)?;
    }
    if let Some(s) = kv.get("psz") {
        let n: i64 = s.parse().map_err(|_| WireError::Malformed)?;
        if n < 0 {
            return Err(WireError::Malformed);
        }
        op.plaintext_size = n;
    }
    // `rend=` (renditions on part ops) is ignored by the MVP parser.
    Ok(op)
}

fn append_file_attrs(b: &mut String, op: &Op) {
    if op.file_size > 0 {
        let _ = write!(b, "|sz={}", op.file_size);
    }
    if op.file_upload_time > 0 {
        let _ = write!(b, "|ts={}", op.file_upload_time);
    }
    if op.encrypted {
        b.push_str("|enc=1");
        let v = if op.encryption_version == 0 {
            1
        } else {
            op.encryption_version
        };
        if v != 1 {
            let _ = write!(b, "|ev={v}");
        }
        if op.plaintext_size > 0 {
            let _ = write!(b, "|psz={}", op.plaintext_size);
        }
    }
}

/// Serialize an op to its TDX1 header. Only the op types this client emits
/// are supported — parse accepts more than format writes.
pub fn format(op: &Op) -> String {
    let op_type = match op.op_type {
        OpTypeOrUnknown::Known(t) => t,
        OpTypeOrUnknown::Unknown => panic!("cannot format unknown op"),
    };
    let mut b = String::with_capacity(64);
    let _ = write!(b, "{WIRE_PREFIX}|t={}", op_type.as_str());
    match op_type {
        OpType::FileUpload => {
            let _ = write!(b, "|p={}|n={}", op.parent, query_escape(&op.name));
            append_file_attrs(&mut b, op);
        }
        OpType::Meta => {
            let _ = write!(
                b,
                "|obj={}|p={}|n={}",
                op.obj,
                op.parent,
                query_escape(&op.name)
            );
            append_file_attrs(&mut b, op);
        }
        OpType::Mkdir | OpType::FolderCommit => {
            if op_type == OpType::FolderCommit {
                let _ = write!(b, "|v={}|oid={}", op.protocol_version, query_escape(&op.op_id));
            }
            let _ = write!(
                b,
                "|obj={}|p={}|n={}",
                op.obj,
                op.parent,
                query_escape(&op.name)
            );
        }
        OpType::Rename => {
            let _ = write!(b, "|obj={}|n={}", op.obj, query_escape(&op.name));
        }
        OpType::Move => {
            let _ = write!(b, "|obj={}|p={}", op.obj, op.parent);
        }
        OpType::Rmdir | OpType::Tomb => {
            let _ = write!(b, "|obj={}", op.obj);
        }
        OpType::FilePart => {
            let _ = write!(b, "|u={}|pix={}", op.upload_uuid, op.part_index);
            if op.file_size > 0 {
                let _ = write!(b, "|sz={}", op.file_size);
            }
        }
        OpType::FileManifest => {
            let _ = write!(
                b,
                "|u={}|p={}|n={}|pc={}",
                op.upload_uuid,
                op.parent,
                query_escape(&op.name),
                op.part_count
            );
            append_file_attrs(&mut b, op);
        }
        OpType::FileCommit => {
            let _ = write!(b, "|v={}|oid={}", op.protocol_version, query_escape(&op.op_id));
            let _ = write!(b, "|p={}|n={}", op.parent, query_escape(&op.name));
            if op.content_msg_id > 0 {
                let _ = write!(b, "|cmid={}", op.content_msg_id);
            } else {
                let _ = write!(b, "|u={}|pc={}", op.upload_uuid, op.part_count);
            }
            if !op.content_hash.is_empty() {
                let _ = write!(b, "|hash={}", query_escape(&op.content_hash));
            }
            append_file_attrs(&mut b, op);
        }
        _ => panic!("format: op type {op_type:?} not supported for writing"),
    }
    b
}

pub fn file_obj_id(msg_id: i64) -> String {
    format!("{FILE_ID_PREFIX}{msg_id}")
}

pub fn new_folder_id() -> String {
    format!("{FOLDER_ID_PREFIX}{}", uuid::Uuid::new_v4())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_file_upload() {
        let op = parse("TDX1|t=f|p=|n=hello+world.txt|sz=42|ts=1700000000").unwrap();
        assert_eq!(op.op_type, OpTypeOrUnknown::Known(OpType::FileUpload));
        assert_eq!(op.parent, "");
        assert_eq!(op.name, "hello world.txt");
        assert_eq!(op.file_size, 42);
        assert_eq!(op.file_upload_time, 1700000000);
    }

    #[test]
    fn parse_mkdir() {
        let op = parse("TDX1|t=mkdir|obj=d:abc-123|p=d:parent|n=My+Dir").unwrap();
        assert_eq!(op.obj, "d:abc-123");
        assert_eq!(op.parent, "d:parent");
        assert_eq!(op.name, "My Dir");
    }

    #[test]
    fn parse_manifest() {
        let op = parse("TDX1|t=manifest|u=uuid-1|p=|n=big.bin|pc=3|sz=6000").unwrap();
        assert_eq!(op.upload_uuid, "uuid-1");
        assert_eq!(op.part_count, 3);
        assert_eq!(op.file_size, 6000);
    }

    #[test]
    fn parse_part() {
        let op = parse("TDX1|t=part|u=uuid-1|pix=2|sz=1900").unwrap();
        assert_eq!(op.upload_uuid, "uuid-1");
        assert_eq!(op.part_index, 2);
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse("hello world"), Err(WireError::Malformed));
        assert_eq!(parse(""), Err(WireError::MissingHeader));
        assert_eq!(parse("TDX2|t=f|n=x"), Err(WireError::BadVersion));
        assert_eq!(parse("TDX1|t=nope|n=x"), Err(WireError::BadOpType));
    }

    #[test]
    fn format_roundtrip() {
        let op = Op {
            op_type: OpTypeOrUnknown::Known(OpType::FileUpload),
            parent: "d:abc".into(),
            name: "a file+.txt".into(),
            file_size: 10,
            file_upload_time: 42,
            ..Default::default()
        };
        let s = format(&op);
        assert_eq!(s, "TDX1|t=f|p=d:abc|n=a+file%2B.txt|sz=10|ts=42");
        let back = parse(&s).unwrap();
        assert_eq!(back.name, "a file+.txt");
        assert_eq!(back.parent, "d:abc");
    }

    #[test]
    fn format_percent20_decodes() {
        // Rust-side %20 (non-Go form) still decodes for us.
        let op = parse("TDX1|t=f|p=|n=a%20b").unwrap();
        assert_eq!(op.name, "a b");
    }

    #[test]
    fn parse_writable_ops() {
        let op = parse("TDX1|t=fcommit|v=1|oid=op-1|p=|n=x|cmid=55|sz=9").unwrap();
        assert_eq!(op.op_id, "op-1");
        assert_eq!(op.content_msg_id, 55);
        let op = parse("TDX1|t=trash|v=1|oid=o2|obj=f:9|rev=2|del=1|purge=2").unwrap();
        assert_eq!(op.expected_revision, 2);
        let op = parse("TDX1|t=relocate|v=1|oid=o3|obj=d:x|p=|n=y|rev=1|ow=1").unwrap();
        assert!(op.overwrite);
    }
}
