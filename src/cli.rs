//! Headless CLI — drives the same worker as the GUI: commands in via
//! `Handle.tx`, events back on `Handle.rx`. Any argv → CLI mode, no
//! args → GPUI. The instance lock still applies: a CLI call while the
//! GUI holds the session fails cleanly instead of tripping
//! AUTH_KEY_DUPLICATED server-side.

use crate::config::{self, AppConfig};
use crate::project::Namespace;
use crate::tg::{ChannelInfo, Command, Event, TransferState, TransferUpdate};
use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashSet;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const USAGE: &str = "tdrive — terminal interface

  tdrive                 launch the GUI
  tdrive drives          list drives (channels)
  tdrive ls [path]       list remote entries (default: root)
  tdrive upload <paths>… [--to folder] [--wrap name]
  tdrive download <file> [--out path]
  tdrive rm <id|path>    move an object to trash (--hard deletes)
  tdrive restore <id|path>
  tdrive mkdir <name> [--in folder]
  tdrive sync            force a resync
  tdrive accounts        list accounts (--switch ID, --remove ID)
  tdrive login           interactive Telegram sign-in

Drive selection: --drive <id|title>, else the last-opened drive,
else the only drive when there's exactly one.";

pub fn run(argv: Vec<String>) -> i32 {
    match run_inner(argv) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("tdrive: {e:#}");
            1
        }
    }
}

fn run_inner(argv: Vec<String>) -> Result<()> {
    // Same lock the GUI takes — sharing a live session.db across two
    // processes mints a duplicate auth key server-side.
    let _lock = config::instance_lock().context(
        "another tdrive instance is running — close the GUI first",
    )?;

    let mut positional = Vec::new();
    let mut drive_sel: Option<String> = None;
    let mut to: Option<String> = None;
    let mut wrap: Option<String> = None;
    let mut out: Option<String> = None;
    let mut parent_in: Option<String> = None;
    let mut hard = false;
    let mut switch: Option<String> = None;
    let mut remove: Option<String> = None;

    let mut it = argv.iter().peekable();
    let sub = it.next().map(String::as_str).unwrap_or("--help");
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--drive" => drive_sel = Some(take(&mut it, "--drive")?),
            "--to" => to = Some(take(&mut it, "--to")?),
            "--wrap" => wrap = Some(take(&mut it, "--wrap")?),
            "--out" => out = Some(take(&mut it, "--out")?),
            "--in" => parent_in = Some(take(&mut it, "--in")?),
            "--switch" => switch = Some(take(&mut it, "--switch")?),
            "--remove" => remove = Some(take(&mut it, "--remove")?),
            "--hard" => hard = true,
            "--help" | "-h" => {
                print!("{USAGE}");
                return Ok(());
            }
            a => positional.push(a.to_string()),
        }
    }

    if sub == "help" || sub == "--help" {
        print!("{USAGE}");
        return Ok(());
    }

    let cfg = AppConfig::load()?.context(
        "no config — run the GUI once to enter api_id/api_hash, or tdrive login",
    )?;
    let handle = crate::tg::spawn();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let mut cli = Cli {
            tx: handle.tx.clone(),
            rx: handle.rx,
            ns: None,
        };
        match sub {
            "login" => cli.login(&cfg).await,
            "accounts" => cli.accounts(switch, remove).await,
            "drives" => cli.drives(&cfg).await,
            "ls" => cli.ls(&cfg, drive_sel, positional.first().cloned()).await,
            "upload" => cli.upload(&cfg, drive_sel, positional, to, wrap).await,
            "download" => cli.download(&cfg, drive_sel, positional, out).await,
            "rm" => cli.obj_op(&cfg, drive_sel, positional, ObjOp::Rm { hard }).await,
            "restore" => cli.obj_op(&cfg, drive_sel, positional, ObjOp::Restore).await,
            "mkdir" => cli.mkdir(&cfg, drive_sel, positional, parent_in).await,
            "sync" => cli.sync(&cfg, drive_sel).await,
            other => bail!("unknown command '{other}'\n\n{USAGE}"),
        }
    })
}

fn take(it: &mut std::iter::Peekable<std::slice::Iter<String>>, flag: &str) -> Result<String> {
    it.next()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow!("{flag} needs a value"))
}

struct Cli {
    tx: tokio::sync::mpsc::UnboundedSender<Command>,
    rx: async_channel::Receiver<Event>,
    ns: Option<Arc<Namespace>>,
}

enum ObjOp {
    Rm { hard: bool },
    Restore,
}

const QUIET_SECS: u64 = 6;

impl Cli {
    fn send(&self, cmd: Command) -> Result<()> {
        self.tx.send(cmd).map_err(|_| anyhow!("worker gone"))
    }

    /// Drain events until `pred` accepts one; every event also updates
    /// the cached namespace and forwards transfer lines to stderr.
    async fn next_event(&mut self, timeout: Duration) -> Result<Event> {
        tokio::time::timeout(timeout, self.rx.recv())
            .await
            .map_err(|_| anyhow!("timed out waiting for the worker"))?
            .map_err(|_| anyhow!("worker gone"))
    }

    fn absorb(&mut self, ev: &Event) {
        if let Event::Namespace(ns) = ev {
            self.ns = Some(ns.clone());
        }
    }

    /// Connect + authorize — returns once the session is confirmed.
    async fn connect(&self, cfg: &AppConfig) -> Result<bool> {
        self.send(Command::Connect {
            api_id: cfg.api_id,
            api_hash: cfg.api_hash.clone(),
        })?;
        loop {
            match self.rx.recv().await.map_err(|_| anyhow!("worker gone"))? {
                Event::Connected { authorized } => return Ok(authorized),
                Event::Error { context, message } => {
                    eprintln!("{context}: {message}");
                }
                _ => {}
            }
        }
    }

    /// Connect, list channels, pick a drive, open it, and wait for the
    /// first namespace projection — shared preamble for every
    /// drive-scoped command.
    async fn open_drive(&mut self, cfg: &AppConfig, sel: Option<String>) -> Result<ChannelInfo> {
        if !self.connect(cfg).await? {
            bail!("not signed in — run `tdrive login` first");
        }
        self.send(Command::ListChannels)?;
        let channels = loop {
            let ev = self.next_event(Duration::from_secs(60)).await?;
            self.absorb(&ev);
            match ev {
                Event::Channels(list) => break list,
                Event::Error { context, message } => eprintln!("{context}: {message}"),
                _ => {}
            }
        };
        let info = pick_drive(&channels, sel, cfg)?;
        self.send(Command::OpenChannel {
            id: info.id,
            access_hash: info.access_hash,
            title: info.title.clone(),
            kind: info.kind,
        })?;
        // ChannelOpened and the first post-sync projection arrive in
        // either order; sync pushes a namespace event unconditionally.
        let mut opened = false;
        loop {
            let ev = self.next_event(Duration::from_secs(120)).await?;
            self.absorb(&ev);
            match ev {
                Event::ChannelOpened { .. } => opened = true,
                Event::Error { context, message } => eprintln!("{context}: {message}"),
                _ => {}
            }
            if opened && self.ns.is_some() {
                break;
            }
        }
        Ok(info)
    }

    async fn accounts(&mut self, switch: Option<String>, remove: Option<String>) -> Result<()> {
        if let Some(id) = switch {
            self.send(Command::SwitchAccount { id: id.clone() })?;
            loop {
                match self.next_event(Duration::from_secs(120)).await? {
                    Event::Connected { authorized } => {
                        println!("switched to {id} (authorized: {authorized})");
                        return Ok(());
                    }
                    Event::Error { context, message } => eprintln!("{context}: {message}"),
                    _ => {}
                }
            }
        }
        if let Some(id) = remove {
            self.send(Command::RemoveAccount { id })?;
        } else {
            self.send(Command::ListAccounts)?;
        }
        loop {
            match self.next_event(Duration::from_secs(60)).await? {
                Event::Accounts { accounts, active } => {
                    for a in accounts {
                        println!(
                            "{}{}\t{}",
                            a.id,
                            if active.as_deref() == Some(&*a.id) { " *" } else { "" },
                            a.label
                        );
                    }
                    return Ok(());
                }
                Event::Error { context, message } => eprintln!("{context}: {message}"),
                _ => {}
            }
        }
    }

    async fn login(&mut self, cfg: &AppConfig) -> Result<()> {
        if self.connect(cfg).await? {
            println!("already signed in");
            return Ok(());
        }
        let phone = prompt("phone (with country code): ")?;
        self.send(Command::RequestCode { phone })?;
        loop {
            match self.rx.recv().await.map_err(|_| anyhow!("worker gone"))? {
                Event::CodeSent => {
                    let code = prompt("login code: ")?;
                    self.send(Command::SubmitCode { code })?;
                }
                Event::NeedPassword { hint } => {
                    let pw = prompt(&format!(
                        "2FA password{}: ",
                        if hint.is_empty() { "".into() } else { format!(" (hint: {hint})") }
                    ))?;
                    self.send(Command::SubmitPassword { password: pw })?;
                }
                Event::SignedIn { name } => {
                    println!("signed in as {name}");
                    return Ok(());
                }
                Event::Error { context, message } => {
                    bail!("{context}: {message}");
                }
                _ => {}
            }
        }
    }

    async fn drives(&mut self, cfg: &AppConfig) -> Result<()> {
        if !self.connect(cfg).await? {
            bail!("not signed in — run `tdrive login` first");
        }
        self.send(Command::ListChannels)?;
        loop {
            match self.next_event(Duration::from_secs(60)).await? {
                Event::Channels(list) => {
                    for c in list {
                        println!("{}\t{}\t{}", c.id, c.kind.as_str(), c.title);
                    }
                    return Ok(());
                }
                Event::Error { context, message } => eprintln!("{context}: {message}"),
                _ => {}
            }
        }
    }

    async fn ls(&mut self, cfg: &AppConfig, sel: Option<String>, path: Option<String>) -> Result<()> {
        // open_drive returns after open_channel's sync, so the cached
        // projection is already fresh.
        self.open_drive(cfg, sel).await?;
        let ns = self.ns.clone().context("no namespace snapshot")?;
        let parent = match path.as_deref() {
            None | Some("") | Some("/") => String::new(),
            Some(p) => resolve_dir(&ns, p)
                .ok_or_else(|| anyhow!("no such folder: {p}"))?,
        };
        // Folders first, then files — alphabetical inside each group.
        let mut rows: Vec<(u8, String, String)> = ns
            .folders
            .values()
            .filter(|f| f.parent == parent && !f.tombstoned)
            .map(|f| (0u8, f.name.clone(), format!("{}/", f.name)))
            .chain(
                ns.files
                    .values()
                    .filter(|f| f.parent == parent && !f.tombstoned)
                    .map(|f| {
                        (1u8, f.name.clone(), format!("{:>10}  {}", human(f.size), f.name))
                    }),
            )
            .collect();
        rows.sort();
        for (_, _, line) in rows {
            println!("{line}");
        }
        Ok(())
    }

    async fn upload(
        &mut self,
        cfg: &AppConfig,
        sel: Option<String>,
        items: Vec<String>,
        to: Option<String>,
        wrap: Option<String>,
    ) -> Result<()> {
        if items.is_empty() {
            bail!("upload needs at least one path");
        }
        let paths: Vec<PathBuf> = items.iter().map(PathBuf::from).collect();
        for p in &paths {
            if !p.exists() {
                bail!("no such path: {}", p.display());
            }
        }
        self.open_drive(cfg, sel).await?;
        let parent = match to.as_deref() {
            None => String::new(),
            Some(p) => {
                let ns = self.ns.clone().context("no namespace")?;
                resolve_dir(&ns, p).ok_or_else(|| anyhow!("no such folder: {p}"))?
            }
        };
        self.send(Command::UploadItems {
            items: paths,
            parent,
            wrap,
        })?;
        self.watch_transfers().await
    }

    async fn download(
        &mut self,
        cfg: &AppConfig,
        sel: Option<String>,
        what: Vec<String>,
        out: Option<String>,
    ) -> Result<()> {
        let arg = what.first().context("download needs a file id or path")?;
        self.open_drive(cfg, sel).await?;
        let (file_id, name) = {
            let ns = self.ns.clone().context("no namespace")?;
            if crate::wire::is_file_id(arg) {
                let name = ns
                    .files
                    .get(arg)
                    .map(|f| f.name.clone())
                    .unwrap_or_else(|| arg.to_string());
                (arg.clone(), name)
            } else {
                let id = resolve_file(&ns, arg)
                    .ok_or_else(|| anyhow!("no such file: {arg}"))?;
                let name = ns.files.get(&id).map(|f| f.name.clone()).unwrap_or_default();
                (id, name)
            }
        };
        let dest = match out {
            Some(o) => {
                let p = PathBuf::from(o);
                if p.is_dir() { p.join(&name) } else { p }
            }
            None => std::env::current_dir()?.join(&name),
        };
        self.send(Command::Download {
            file_id,
            dest: dest.clone(),
        })?;
        self.watch_transfers().await?;
        println!("saved {}", dest.display());
        Ok(())
    }

    async fn obj_op(
        &mut self,
        cfg: &AppConfig,
        sel: Option<String>,
        what: Vec<String>,
        op: ObjOp,
    ) -> Result<()> {
        let arg = what.first().context("missing object")?;
        self.open_drive(cfg, sel).await?;
        let obj = {
            let ns = self.ns.clone().context("no namespace")?;
            resolve_obj(&ns, arg).ok_or_else(|| anyhow!("no such object: {arg}"))?
        };
        self.send(match op {
            ObjOp::Rm { hard: true } => Command::Purge { obj },
            ObjOp::Rm { hard: false } => Command::Delete { obj },
            ObjOp::Restore => Command::Restore { obj },
        })?;
        // The namespace event acknowledging the op is the completion.
        loop {
            let ev = self.next_event(Duration::from_secs(120)).await?;
            self.absorb(&ev);
            match ev {
                Event::Namespace(_) => {
                    println!("done");
                    return Ok(());
                }
                Event::Error { context, message } => eprintln!("{context}: {message}"),
                _ => {}
            }
        }
    }

    async fn mkdir(
        &mut self,
        cfg: &AppConfig,
        sel: Option<String>,
        name: Vec<String>,
        parent_in: Option<String>,
    ) -> Result<()> {
        let name = name.first().context("mkdir needs a name")?.clone();
        self.open_drive(cfg, sel).await?;
        let parent = match parent_in.as_deref() {
            None => String::new(),
            Some(p) => {
                let ns = self.ns.clone().context("no namespace")?;
                resolve_dir(&ns, p).ok_or_else(|| anyhow!("no such folder: {p}"))?
            }
        };
        self.send(Command::Mkdir { parent, name })?;
        loop {
            let ev = self.next_event(Duration::from_secs(120)).await?;
            self.absorb(&ev);
            match ev {
                Event::Namespace(_) => {
                    println!("done");
                    return Ok(());
                }
                Event::Error { context, message } => eprintln!("{context}: {message}"),
                _ => {}
            }
        }
    }

    async fn sync(&mut self, cfg: &AppConfig, sel: Option<String>) -> Result<()> {
        self.open_drive(cfg, sel).await?;
        self.send(Command::Sync)?;
        loop {
            let ev = self.next_event(Duration::from_secs(300)).await?;
            self.absorb(&ev);
            match ev {
                Event::SyncState { active: false } => {
                    println!("synced");
                    return Ok(());
                }
                Event::Error { context, message } => eprintln!("{context}: {message}"),
                _ => {}
            }
        }
    }

    /// Watch transfer updates until every seen transfer is terminal and
    /// the stream goes quiet — discovery gaps between batches are real
    /// (the folder walk enqueues progressively) so silence is the only
    /// end signal we get.
    async fn watch_transfers(&mut self) -> Result<()> {
        let mut live: HashSet<String> = HashSet::new();
        let mut seen = false;
        let mut failed = 0usize;
        let mut last_line: std::collections::HashMap<String, i64> = Default::default();
        loop {
            let ev = match tokio::time::timeout(Duration::from_secs(QUIET_SECS), self.rx.recv()).await {
                Ok(Ok(ev)) => ev,
                Ok(Err(_)) => bail!("worker gone"),
                Err(_) => {
                    if live.is_empty() {
                        break;
                    }
                    // A live transfer can legitimately stall through a
                    // flood wait with no progress events — those emit
                    // Status lines, so true silence still means "stuck";
                    // give it a long leash, not a hard fail.
                    eprintln!("waiting on {} transfer(s)…", live.len());
                    continue;
                }
            };
            self.absorb(&ev);
            match ev {
                Event::Transfer(TransferUpdate { id, label, sent, total, state }) => {
                    seen = true;
                    let pct = if total > 0 { sent * 100 / total } else { 0 };
                    match state {
                        TransferState::Done => {
                            live.remove(&id);
                            println!("✓ {label}");
                            last_line.remove(&id);
                        }
                        TransferState::Failed => {
                            live.remove(&id);
                            failed += 1;
                            eprintln!("✗ {label} failed");
                            last_line.remove(&id);
                        }
                        TransferState::Active | TransferState::Paused => {
                            live.insert(id.clone());
                            if last_line.get(&id) != Some(&pct) {
                                last_line.insert(id, pct);
                                eprintln!("… {label} {pct}%");
                            }
                        }
                    }
                }
                Event::Status { message } | Event::Error { message, .. } => {
                    eprintln!("{message}");
                }
                _ => {}
            }
        }
        if !seen {
            println!("nothing to transfer");
        }
        if failed > 0 {
            bail!("{failed} transfer(s) failed");
        }
        Ok(())
    }
}

fn pick_drive(channels: &[ChannelInfo], sel: Option<String>, cfg: &AppConfig) -> Result<ChannelInfo> {
    if let Some(sel) = sel {
        return channels
            .iter()
            .find(|c| c.id.to_string() == sel || c.title.eq_ignore_ascii_case(&sel))
            .cloned()
            .ok_or_else(|| anyhow!("no drive matches '{sel}'"));
    }
    if let Some(last) = cfg.last_channel_id {
        if let Some(c) = channels.iter().find(|c| c.id == last) {
            return Ok(c.clone());
        }
    }
    match channels {
        [one] => Ok(one.clone()),
        [] => bail!("no drives on this account — create one in the GUI or picker"),
        many => bail!(
            "multiple drives — pick one with --drive:\n{}",
            many.iter()
                .map(|c| format!("  {}\t{}\t{}", c.id, c.kind.as_str(), c.title))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    }
}

/// Resolve `a/b/c` folder paths through the parent chain; "" → root.
fn resolve_dir(ns: &Namespace, path: &str) -> Option<String> {
    let mut cur = String::new();
    for seg in path.trim_matches('/').split('/') {
        if seg.is_empty() {
            continue;
        }
        cur = ns
            .folders
            .iter()
            .find(|(_, f)| f.parent == cur && f.name == seg && !f.tombstoned)
            .map(|(id, _)| id.clone())?;
    }
    Some(cur)
}

fn resolve_file(ns: &Namespace, path: &str) -> Option<String> {
    if crate::wire::is_file_id(path) {
        return Some(path.to_string());
    }
    let (dir, name) = path.rsplit_once('/').map(|(d, n)| (d, n)).unwrap_or(("", path));
    let parent = resolve_dir(ns, dir)?;
    ns.files
        .iter()
        .find(|(_, f)| f.parent == parent && f.name == name && !f.tombstoned)
        .map(|(id, _)| id.clone())
}

/// Any object by id, or a path resolved to file first then folder.
fn resolve_obj(ns: &Namespace, arg: &str) -> Option<String> {
    if crate::wire::is_file_id(arg) || ns.folders.contains_key(arg) {
        return Some(arg.to_string());
    }
    resolve_file(ns, arg).or_else(|| resolve_dir(ns, arg))
}

fn human(n: i64) -> String {
    const U: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n}B")
    } else {
        format!("{v:.1}{}", U[i])
    }
}

fn prompt(label: &str) -> Result<String> {
    eprint!("{label}");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}
