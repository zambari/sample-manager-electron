//! An Elektron Digitakt's +Drive, browsed over USB MIDI.
//!
//! One worker thread owns the MIDI connection and runs every request in
//! order: the device answers one API call at a time, so there is nothing to
//! gain from parallel requests and a lot to lose. The UI posts [`Cmd`]s and
//! gets results back as ordinary [`crate::jobs::Ev`]s.
//!
//! Device paths are plain absolute paths (`/samples/kicks`) stored in
//! `PathBuf`s; they never touch the local filesystem.

mod proto;
mod sample;

use crate::jobs::{Emitter, Ev};
use crate::ops::Policy;
use proto::{Args, DirEntry, Reader};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

/// Where elk-herd (and we) put deleted items, so a stray keypress is undoable.
pub const TRASH: &str = "/trash";

const TIMEOUT: Duration = Duration::from_secs(5);
const PROGRESS_EVERY: u64 = 64 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    /// Device to device, by rename. Instant.
    Move,
    /// Local file or folder to the device.
    Upload,
    /// Device to a local folder, as WAV.
    Download,
    /// Device to device, by reading and rewriting each file.
    Copy,
}

pub enum Task {
    Transfer {
        dir: Dir,
        /// `(source, destination folder)`.
        items: Vec<(PathBuf, PathBuf)>,
        policy: Policy,
        /// Fold uploads to mono this way.
        mono: Option<crate::audio::stats::Fold>,
        /// Best guess for the progress bar.
        total_bytes: u64,
    },
    Rename { from: PathBuf, to: PathBuf },
    Mkdir { path: PathBuf },
    Trash { paths: Vec<PathBuf> },
    /// Delete outright, folders with everything in them.
    Delete { paths: Vec<PathBuf> },
}

enum Cmd {
    List { path: PathBuf, generation: u64 },
    Job { job: u64, cancel: Arc<AtomicBool>, task: Task },
}

pub struct Device {
    tx: Sender<Cmd>,
}

impl Device {
    pub fn new(em: Emitter) -> Self {
        let (tx, rx) = channel();
        let _ = std::thread::Builder::new()
            .name("device".into())
            .spawn(move || Worker { conn: None, em, job: None }.run(rx));
        Self { tx }
    }

    pub fn list(&self, path: PathBuf, generation: u64) {
        let _ = self.tx.send(Cmd::List { path, generation });
    }

    pub fn run(&self, job: u64, cancel: Arc<AtomicBool>, task: Task) {
        let _ = self.tx.send(Cmd::Job { job, cancel, task });
    }
}

/// What can go wrong talking to the device. A link error drops the
/// connection so the next request reconnects.
enum Fail {
    Link(String),
    Refused(String),
}

impl std::fmt::Display for Fail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fail::Link(s) | Fail::Refused(s) => f.write_str(s),
        }
    }
}

type R<T> = Result<T, Fail>;

fn refused<T>(what: impl Into<String>) -> R<T> {
    Err(Fail::Refused(what.into()))
}

fn dev_str(p: &Path) -> String {
    let s = p.to_string_lossy().into_owned();
    if s.starts_with('/') { s } else { format!("/{s}") }
}

// ---- connection -------------------------------------------------------------

struct Conn {
    out: midir::MidiOutputConnection,
    _input: midir::MidiInputConnection<()>,
    rx: Receiver<Vec<u8>>,
    next: u16,
    stereo: bool,
    name: String,
}

fn find_port<T>(ports: Vec<T>, name: impl Fn(&T) -> Option<String>) -> Option<(T, String)> {
    ports.into_iter().find_map(|p| {
        let n = name(&p)?;
        let l = n.to_ascii_lowercase();
        (l.contains("elektron") || l.contains("digitakt")).then_some((p, n))
    })
}

impl Conn {
    fn open() -> Result<Self, String> {
        let mut input = midir::MidiInput::new("Sample Manager").map_err(|e| e.to_string())?;
        input.ignore(midir::Ignore::None);
        let output = midir::MidiOutput::new("Sample Manager").map_err(|e| e.to_string())?;

        let not_found = "No Elektron device found. Connect the Digitakt by USB \
                         (USB config: Overbridge or USB MIDI) and press Refresh.";
        let (in_port, in_name) =
            find_port(input.ports(), |p| input.port_name(p).ok()).ok_or(not_found)?;
        let (out_port, _) =
            find_port(output.ports(), |p| output.port_name(p).ok()).ok_or(not_found)?;

        let (tx, rx) = channel();
        let _input = input
            .connect(
                &in_port,
                "sample-manager-in",
                move |_, msg, _| {
                    if msg.first() == Some(&0xF0) {
                        let _ = tx.send(msg.to_vec());
                    }
                },
                (),
            )
            .map_err(|e| format!("MIDI input: {e}"))?;
        let out = output
            .connect(&out_port, "sample-manager-out")
            .map_err(|e| format!("MIDI output: {e}"))?;

        let mut c = Self { out, _input, rx, next: 0, stereo: false, name: in_name };
        let r = c.request(proto::DEVICE, Args::default()).map_err(|e| e.to_string())?;
        let mut rd = Reader::new(&r);
        let product = rd.u8().map_err(Fail::Link).map_err(|e| e.to_string())?;
        // Message ids the device supports, then its name.
        if let Ok(n) = rd.u8() {
            for _ in 0..n {
                let _ = rd.u8();
            }
        }
        if let Ok(name) = rd.str() {
            if !name.is_empty() {
                c.name = name;
            }
        }
        // Digitakt II (42) takes stereo; the original (12) is mono only.
        c.stereo = product == 42;
        Ok(c)
    }

    fn request(&mut self, cmd: u8, args: Args) -> R<Vec<u8>> {
        self.next = self.next.wrapping_add(1).max(1);
        let id = self.next;
        while self.rx.try_recv().is_ok() {} // stale replies
        self.out
            .send(&proto::frame(id, cmd, &args.0))
            .map_err(|e| Fail::Link(format!("MIDI send failed: {e}")))?;
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let msg = self
                .rx
                .recv_timeout(left)
                .map_err(|_| {
                    crate::log::line(format!("device: cmd 0x{cmd:02x} id {id} timed out"));
                    Fail::Link("The device stopped answering (timed out)".into())
                })?;
            if let Some((c, a)) = proto::unframe(&msg, id) {
                if c == cmd | 0x80 {
                    return Ok(a);
                }
            }
        }
    }

    /// For calls answering a single "ok" byte.
    fn ok(&mut self, cmd: u8, args: Args, what: String) -> R<()> {
        let r = self.request(cmd, args)?;
        if Reader::new(&r).bool().unwrap_or(false) {
            Ok(())
        } else {
            crate::log::line(format!("device: cmd 0x{cmd:02x} refused ({what}), reply {r:02x?}"));
            refused(what)
        }
    }

    fn list(&mut self, p: &Path) -> R<Vec<DirEntry>> {
        let r = self.request(proto::DIR_LIST, Args::default().str(&dev_str(p)))?;
        proto::parse_listing(&r).map_err(Fail::Link)
    }

    fn mkdir(&mut self, p: &Path) -> R<()> {
        let s = dev_str(p);
        self.ok(proto::DIR_CREATE, Args::default().str(&s), format!("could not create folder {s}"))
    }

    /// Remove an empty folder.
    fn rmdir(&mut self, p: &Path) -> R<()> {
        let s = dev_str(p);
        self.ok(proto::DIR_DELETE, Args::default().str(&s), format!("could not remove folder {s}"))
    }

    fn delete_file(&mut self, p: &Path) -> R<()> {
        let s = dev_str(p);
        self.ok(proto::FILE_DELETE, Args::default().str(&s), format!("could not delete {s}"))
    }

    fn rename(&mut self, from: &Path, to: &Path) -> R<()> {
        let (a, b) = (dev_str(from), dev_str(to));
        self.ok(proto::ITEM_RENAME, Args::default().str(&a).str(&b), format!("could not move {a} to {b}"))
    }

    /// `on_bytes(chunk, file_total)` after each chunk.
    fn read(&mut self, p: &Path, cancel: &AtomicBool, on_bytes: &mut dyn FnMut(u64, u64)) -> R<Vec<u8>> {
        let s = dev_str(p);
        let r = self.request(proto::READ_OPEN, Args::default().str(&s))?;
        let mut rd = Reader::new(&r);
        let ok = rd.bool().map_err(Fail::Link)?;
        let fd = rd.u32().map_err(Fail::Link)?;
        let total = rd.u32().map_err(Fail::Link)? as usize;
        if !ok {
            return refused(format!("could not open {s}"));
        }
        let mut data = Vec::with_capacity(total);
        let res = (|| {
            while data.len() < total {
                if cancel.load(Ordering::Relaxed) {
                    return refused("cancelled");
                }
                let len = (total - data.len()).min(proto::CHUNK) as u32;
                let start = data.len() as u32;
                let r = self.request(proto::READ, Args::default().u32(fd).u32(len).u32(start))?;
                let mut rd = Reader::new(&r);
                let ok = rd.bool().map_err(Fail::Link)?;
                let _fd = rd.u32().map_err(Fail::Link)?;
                let _len = rd.u32().map_err(Fail::Link)?;
                let _start = rd.u32().map_err(Fail::Link)?;
                let _end = rd.u32().map_err(Fail::Link)?;
                let chunk = rd.rest();
                if !ok || chunk.is_empty() {
                    return refused(format!("read failed: {s}"));
                }
                data.extend_from_slice(chunk);
                on_bytes(chunk.len() as u64, total as u64);
            }
            Ok(())
        })();
        let close = self.request(proto::READ_CLOSE, Args::default().u32(fd));
        res?;
        close?;
        data.truncate(total);
        Ok(data)
    }

    /// `on_bytes(chunk, file_total)` after each chunk.
    fn write(&mut self, p: &Path, data: &[u8], cancel: &AtomicBool, on_bytes: &mut dyn FnMut(u64, u64)) -> R<()> {
        let s = dev_str(p);
        let total = data.len() as u32;
        let r = self.request(proto::WRITE_OPEN, Args::default().u32(total).str(&s))?;
        let mut rd = Reader::new(&r);
        let ok = rd.bool().map_err(Fail::Link)?;
        let fd = rd.u32().map_err(Fail::Link)?;
        if !ok {
            return refused(format!("could not create {s} (drive full, or bad name?)"));
        }
        let res = (|| {
            let mut off = 0usize;
            for chunk in data.chunks(proto::CHUNK) {
                if cancel.load(Ordering::Relaxed) {
                    return refused("cancelled");
                }
                let args = Args::default().u32(fd).u32(chunk.len() as u32).u32(off as u32).bytes(chunk);
                let r = self.request(proto::WRITE, args)?;
                if !Reader::new(&r).bool().unwrap_or(false) {
                    return refused(format!("write failed: {s}"));
                }
                off += chunk.len();
                on_bytes(chunk.len() as u64, total as u64);
            }
            Ok(())
        })();
        match res {
            Ok(()) => {
                let r = self.request(proto::WRITE_CLOSE, Args::default().u32(fd).u32(total))?;
                if Reader::new(&r).bool().unwrap_or(false) { Ok(()) } else { refused(format!("could not finish {s}")) }
            }
            Err(e) => {
                // Closing with length 0 abandons the file; then remove the stub.
                if self.request(proto::WRITE_CLOSE, Args::default().u32(fd).u32(0)).is_ok() {
                    let _ = self.delete_file(p);
                }
                Err(e)
            }
        }
    }
}

// ---- worker -----------------------------------------------------------------

struct Worker {
    conn: Option<Conn>,
    em: Emitter,
    job: Option<Prog>,
}

/// Progress of the running transfer.
#[derive(Clone, Copy)]
struct Prog {
    id: u64,
    done: u64,
    /// `done` when progress was last sent.
    last: u64,
    /// Estimated bytes; 0 when unknown (the bar then counts items).
    total: u64,
    item: usize,
    items: usize,
    /// The file on the wire: `(phase, bytes done, file size)`.
    file: Option<(&'static str, u64, u64)>,
}

impl Prog {
    fn ev(&self, label: &str) -> Ev {
        Ev::Progress {
            job: self.id,
            done_bytes: self.done,
            total_bytes: self.total,
            done_items: self.item,
            total_items: self.items,
            label: label.to_owned(),
            file: self.file,
        }
    }
}

/// Name for a file going to the device: the stem, since the device has no use
/// for `.wav`.
fn device_name(local: &Path) -> String {
    let name = local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if crate::fs_model::is_audio_path(local) {
        local.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or(name)
    } else {
        name
    }
}

/// `name 2`, `name 3`, ... not in `taken`.
fn unique_name(taken: &HashSet<String>, name: &str) -> String {
    (2..10_000).map(|n| format!("{name} {n}")).find(|c| !taken.contains(c)).unwrap_or_else(|| name.to_owned())
}

/// Where a download of `name` lands.
pub fn wav_name(name: &str) -> String {
    if name.to_ascii_lowercase().ends_with(".wav") { name.to_owned() } else { format!("{name}.wav") }
}

/// Where an upload of `local` lands in `dir`.
pub fn upload_target(local: &Path, dir: &Path) -> PathBuf {
    dir.join(device_name(local))
}

/// Audio files under local `p` (non-hidden, as an upload picks them) and their bytes.
fn count_local(p: &Path) -> (usize, u64) {
    if !p.is_dir() {
        return (1, std::fs::metadata(p).map(|m| m.len()).unwrap_or(0));
    }
    let mut total = (0, 0);
    let walk = walkdir::WalkDir::new(p)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'));
    for de in walk.flatten() {
        if de.file_type().is_file() && crate::fs_model::is_audio_path(de.path()) {
            total.0 += 1;
            total.1 += de.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    total
}

impl Worker {
    fn run(mut self, rx: Receiver<Cmd>) {
        while let Ok(cmd) = rx.recv() {
            match cmd {
                Cmd::List { path, generation } => {
                    let result = self.with_conn(|c| c.list(&path)).map(|v| {
                        v.into_iter()
                            .map(|d| crate::fs_model::Entry {
                                path: path.join(&d.name),
                                name: d.name,
                                is_dir: d.is_dir,
                                len: if d.is_dir { 0 } else { d.size as u64 },
                                modified: None,
                                is_audio: false,
                            })
                            .collect()
                    });
                    self.em.send(Ev::Listed { device: true, generation, path, result });
                }
                Cmd::Job { job, cancel, task } => self.job(job, &cancel, task),
            }
        }
    }

    fn status(&self) {
        let s = match &self.conn {
            Some(c) => Ok(format!("{}{}", c.name, if c.stereo { "" } else { " (mono)" })),
            None => Err("not connected".to_owned()),
        };
        self.em.send(Ev::DeviceStatus(s));
    }

    /// Run `f` on the connection, opening it first if needed. A link failure
    /// closes it again so the next call starts fresh.
    fn with_conn<T>(&mut self, f: impl FnOnce(&mut Conn) -> R<T>) -> Result<T, String> {
        if self.conn.is_none() {
            match Conn::open() {
                Ok(c) => {
                    self.conn = Some(c);
                    self.status();
                }
                Err(e) => {
                    self.em.send(Ev::DeviceStatus(Err(e.clone())));
                    return Err(e);
                }
            }
        }
        let c = self.conn.as_mut().expect("opened above");
        match f(c) {
            Ok(v) => Ok(v),
            Err(Fail::Link(e)) => {
                self.conn = None;
                self.em.send(Ev::DeviceStatus(Err(e.clone())));
                Err(e)
            }
            Err(Fail::Refused(e)) => Err(e),
        }
    }

    fn progress(&mut self, label: &str, n: u64) {
        let Some(p) = self.job.as_mut() else { return };
        if !label.is_empty() {
            p.file = None;
        }
        p.done += n;
        if label.is_empty() && p.done - p.last < PROGRESS_EVERY {
            return;
        }
        p.last = p.done;
        let ev = p.ev(label);
        self.em.send(ev);
    }

    fn job(&mut self, job: u64, cancel: &AtomicBool, task: Task) {
        let mut errors = Vec::new();
        let mut touched_dev: Vec<PathBuf> = Vec::new();
        let mut touched_local: Vec<PathBuf> = Vec::new();
        let touch = |v: &mut Vec<PathBuf>, p: &Path| {
            if !v.iter().any(|t| t == p) {
                v.push(p.to_path_buf());
            }
        };
        // Tells the UI this job is now running rather than waiting in line.
        let items = match &task {
            Task::Transfer { items, .. } => items.len(),
            Task::Trash { paths } | Task::Delete { paths } => paths.len(),
            _ => 1,
        };
        let total = match &task {
            Task::Transfer { total_bytes, .. } => *total_bytes,
            _ => 0,
        };
        self.em.send(Prog { id: job, done: 0, last: 0, total, item: 0, items, file: None }.ev(""));

        match task {
            Task::Rename { from, to } => {
                let res = self.with_conn(|c| {
                    let parent = to.parent().unwrap_or(Path::new("/"));
                    let name = to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    if c.list(parent)?.iter().any(|e| e.name == name) {
                        return refused(format!("{} already exists", dev_str(&to)));
                    }
                    c.rename(&from, &to)
                });
                if let Err(e) = res {
                    errors.push(e);
                }
                touch(&mut touched_dev, to.parent().unwrap_or(Path::new("/")));
            }
            Task::Mkdir { path } => {
                if let Err(e) = self.with_conn(|c| c.mkdir(&path)) {
                    errors.push(e);
                }
                touch(&mut touched_dev, path.parent().unwrap_or(Path::new("/")));
            }
            Task::Trash { paths } => {
                let trash = PathBuf::from(TRASH);
                self.job = Some(Prog { id: job, done: 0, last: 0, total: 0, item: 0, items: paths.len(), file: None });
                let counts = self.count_files(&paths, false, cancel);
                self.set_totals(&counts, true);
                let mut before = (0, 0);
                for (i, p) in paths.into_iter().enumerate() {
                    self.catch_up(&mut before, counts.get(i.wrapping_sub(1)), true);
                    if p.starts_with(&trash) {
                        errors.push(format!("{} is already in {TRASH}", dev_str(&p)));
                        continue;
                    }
                    let target = self.with_conn(|c| {
                        if !c.list(Path::new("/"))?.iter().any(|e| e.is_dir && e.name == "trash") {
                            c.mkdir(&trash)?;
                        }
                        let taken: HashSet<String> = c.list(&trash)?.into_iter().map(|e| e.name).collect();
                        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        let name = if taken.contains(&name) { unique_name(&taken, &name) } else { name };
                        Ok(trash.join(name))
                    });
                    let res = target.and_then(|t| {
                        let e = self.stat(&p)?;
                        self.progress(&e.name, 0);
                        self.relocate(&p, &t, e.is_dir, e.size as u64, cancel)
                    });
                    match res {
                        Ok(_) => touch(&mut touched_dev, p.parent().unwrap_or(Path::new("/"))),
                        Err(e) => errors.push(e),
                    }
                }
                self.job = None;
                touch(&mut touched_dev, &trash);
                touch(&mut touched_dev, Path::new("/"));
            }
            Task::Delete { paths } => {
                self.job = Some(Prog { id: job, done: 0, last: 0, total: 0, item: 0, items: paths.len(), file: None });
                let counts = self.count_files(&paths, false, cancel);
                self.set_totals(&counts, false);
                let mut before = (0, 0);
                for (i, p) in paths.into_iter().enumerate() {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    self.catch_up(&mut before, counts.get(i.wrapping_sub(1)), false);
                    let res = self.stat(&p).and_then(|e| self.remove(&p, e.is_dir, cancel));
                    if let Err(e) = res {
                        errors.push(e);
                    }
                    touch(&mut touched_dev, p.parent().unwrap_or(Path::new("/")));
                }
                self.job = None;
            }
            Task::Transfer { dir, items, policy, mono, total_bytes } => {
                let n = items.len();
                crate::log::line(format!("job {job}: {dir:?} {n} item(s), policy {policy:?}"));
                for (src, dst) in &items {
                    crate::log::line(format!("  {} -> {}", src.display(), dst.display()));
                }
                self.job = Some(Prog { id: job, done: 0, last: 0, total: total_bytes, item: 0, items: n, file: None });
                let srcs: Vec<PathBuf> = items.iter().map(|(s, _)| s.clone()).collect();
                let counts = self.count_files(&srcs, dir == Dir::Upload, cancel);
                // Device sizes are exact; an upload's bytes on the wire are
                // not its file sizes, so keep the caller's guess there.
                let scan_bytes = dir != Dir::Upload || total_bytes == 0;
                self.set_totals(&counts, scan_bytes);
                let mut before = (0, 0);
                for (i, (src, dst_dir)) in items.into_iter().enumerate() {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    self.catch_up(&mut before, counts.get(i.wrapping_sub(1)), scan_bytes);
                    self.progress(&src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), 0);
                    let res = match dir {
                        Dir::Move => self.move_item(&src, &dst_dir, policy, cancel),
                        Dir::Upload => self.upload(&src, &dst_dir, policy, mono, cancel, &mut errors),
                        Dir::Download => self.download(&src, &dst_dir, policy, cancel, &mut errors),
                        Dir::Copy => self.copy(&src, &dst_dir, policy, cancel, &mut errors),
                    };
                    if let Err(e) = res {
                        errors.push(e);
                    }
                    match dir {
                        Dir::Download => touch(&mut touched_local, &dst_dir),
                        _ => touch(&mut touched_dev, &dst_dir),
                    }
                    if dir == Dir::Move {
                        touch(&mut touched_dev, src.parent().unwrap_or(Path::new("/")));
                    }
                }
                self.job = None;
            }
        }

        let cancelled = cancel.load(Ordering::Relaxed);
        if cancelled {
            errors.retain(|e| !e.ends_with("cancelled"));
        }
        self.em.send(Ev::Finished { job, touched: touched_local, device_touched: touched_dev, errors, cancelled });
    }

    /// Files under each of `paths` and their bytes, so the bar can count
    /// files rather than top-level items. `local`: paths on disk (an
    /// upload), filtered the way [`Self::upload`] picks files.
    fn count_files(&mut self, paths: &[PathBuf], local: bool, cancel: &AtomicBool) -> Vec<(usize, u64)> {
        self.progress("Counting files…", 0);
        let mut listings: HashMap<PathBuf, Vec<(String, bool, u64)>> = HashMap::new();
        let mut out = Vec::with_capacity(paths.len());
        for p in paths {
            if local {
                out.push(count_local(p));
                continue;
            }
            // One listing per parent folder, not one per item.
            let parent = p.parent().unwrap_or(Path::new("/")).to_path_buf();
            if !listings.contains_key(&parent) {
                let l = self.with_conn(|c| c.list(&parent)).unwrap_or_default();
                listings.insert(parent.clone(), l.into_iter().map(|e| (e.name, e.is_dir, e.size as u64)).collect());
            }
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let found = listings[&parent].iter().find(|e| e.0 == name).map(|e| (e.1, e.2));
            out.push(match found {
                None => (0, 0),
                Some((false, size)) => (1, size),
                Some((true, _)) => self.count_dir(p, cancel),
            });
        }
        out
    }

    fn count_dir(&mut self, p: &Path, cancel: &AtomicBool) -> (usize, u64) {
        let Ok(kids) = self.with_conn(|c| c.list(p)) else { return (0, 0) };
        let mut total = (0, 0);
        for k in kids {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let (n, b) = if k.is_dir { self.count_dir(&p.join(&k.name), cancel) } else { (1, k.size as u64) };
            total.0 += n;
            total.1 += b;
        }
        total
    }

    /// Put the counted files (and, with `bytes`, their size) on the bar.
    fn set_totals(&mut self, counts: &[(usize, u64)], bytes: bool) {
        let Some(p) = self.job.as_mut() else { return };
        p.items = counts.iter().map(|c| c.0).sum();
        if bytes {
            p.total = counts.iter().map(|c| c.1).sum();
        }
        let ev = p.ev("");
        self.em.send(ev);
    }

    /// Before the next top-level item: count the previous one (`prev`) as
    /// fully done, so skipped files or a folder renamed in one go still add up.
    fn catch_up(&mut self, before: &mut (usize, u64), prev: Option<&(usize, u64)>, bytes: bool) {
        let Some(&(n, b)) = prev else { return };
        before.0 += n;
        before.1 += b;
        let Some(p) = self.job.as_mut() else { return };
        p.item = p.item.max(before.0);
        if bytes {
            p.done = p.done.max(before.1);
        }
    }

    /// One file finished: tick the file count.
    fn file_done(&mut self) {
        let Some(p) = self.job.as_mut() else { return };
        p.item += 1;
        p.last = p.done;
        let ev = p.ev("");
        self.em.send(ev);
    }

    /// Look up one device item by listing its folder.
    fn stat(&mut self, p: &Path) -> Result<DirEntry, String> {
        let parent = p.parent().unwrap_or(Path::new("/")).to_path_buf();
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.with_conn(|c| c.list(&parent))?
            .into_iter()
            .find(|e| e.name == name)
            .ok_or_else(|| format!("{} no longer exists", dev_str(p)))
    }

    /// Settle where `name` goes in device folder `dir` under `policy`.
    /// `Some((path, exists))`, or `None` to skip. Replacing a file deletes it
    /// first; an existing folder is merged into.
    fn place_on_device(&mut self, dir: &Path, name: &str, is_dir: bool, policy: Policy) -> Result<Option<(PathBuf, bool)>, String> {
        let listing = self.with_conn(|c| c.list(dir))?;
        let Some(existing) = listing.iter().find(|e| e.name == name) else {
            return Ok(Some((dir.join(name), false)));
        };
        match policy {
            Policy::Skip => Ok(None),
            Policy::KeepBoth => {
                let taken: HashSet<String> = listing.iter().map(|e| e.name.clone()).collect();
                Ok(Some((dir.join(unique_name(&taken, name)), false)))
            }
            Policy::Replace if is_dir && existing.is_dir => Ok(Some((dir.join(name), true))),
            Policy::Replace if !is_dir && !existing.is_dir => {
                let p = dir.join(name);
                self.with_conn(|c| c.delete_file(&p))?;
                Ok(Some((p, false)))
            }
            Policy::Replace => Err(format!("{}: a {} of that name is in the way", dev_str(&dir.join(name)), if existing.is_dir { "folder" } else { "file" })),
        }
    }

    fn move_item(&mut self, src: &Path, dst_dir: &Path, policy: Policy, cancel: &AtomicBool) -> Result<(), String> {
        if dst_dir.starts_with(src) {
            return Err(format!("{}: cannot move a folder into itself", dev_str(src)));
        }
        if src.parent() == Some(dst_dir) {
            return Ok(());
        }
        let entry = self.stat(src)?;
        let name = entry.name.clone();
        self.progress(&name, 0);
        // A rename can't merge folders: an existing folder is in the way.
        let policy = if entry.is_dir && policy == Policy::Replace { Policy::KeepBoth } else { policy };
        let Some((target, _)) = self.place_on_device(dst_dir, &name, entry.is_dir, policy)? else { return Ok(()) };
        self.relocate(src, &target, entry.is_dir, entry.size as u64, cancel)
    }

    /// Delete `p` from the device; a folder is emptied first, since the
    /// device only removes empty ones.
    fn remove(&mut self, p: &Path, is_dir: bool, cancel: &AtomicBool) -> Result<(), String> {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        self.progress(&Self::short(p), 0);
        if is_dir {
            let kids = self.with_conn(|c| c.list(p))?;
            for k in kids {
                // Stop at the first failure, leaving the rest in place.
                self.remove(&p.join(&k.name), k.is_dir, cancel)?;
            }
            self.with_conn(|c| c.rmdir(p))
        } else {
            self.with_conn(|c| c.delete_file(p))?;
            self.file_done();
            Ok(())
        }
    }

    /// `parent/name` of a device path, for the status bar.
    fn short(p: &Path) -> String {
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match p.parent().and_then(|d| d.file_name()) {
            Some(d) => format!("{}/{name}", d.to_string_lossy()),
            None => name,
        }
    }

    /// Move `src` to `target` (a free path). The device refuses to rename a
    /// folder into another folder, so when a rename is refused, rebuild the
    /// item at `target` piece by piece and remove the source. `size` is the
    /// file's size (0 for folders), counted into progress once it has moved.
    fn relocate(&mut self, src: &Path, target: &Path, is_dir: bool, size: u64, cancel: &AtomicBool) -> Result<(), String> {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let renamed = self.with_conn(|c| match c.rename(src, target) {
            Ok(()) => Ok(true),
            Err(Fail::Refused(_)) => Ok(false),
            Err(e) => Err(e),
        })?;
        if renamed {
            self.progress("", size);
            if !is_dir {
                self.file_done();
            }
            return Ok(());
        }
        crate::log::line(format!("device: rename refused, moving {} piece by piece", dev_str(src)));
        if is_dir {
            self.progress(&format!("{}/…", Self::short(src)), 0);
            self.with_conn(|c| c.mkdir(target))?;
            let kids = self.with_conn(|c| c.list(src))?;
            for k in kids {
                // Each child gets its own line, so a big folder visibly moves.
                self.progress(&Self::short(&src.join(&k.name)), 0);
                // Stop at the first failure so the source keeps what didn't move.
                self.relocate(&src.join(&k.name), &target.join(&k.name), k.is_dir, k.size as u64, cancel)?;
            }
            self.with_conn(|c| c.rmdir(src))?;
        } else {
            self.progress(&Self::short(src), 0);
            // Read and write each count half, so the file adds up to its size once.
            let raw = self.read_raw(src, true, cancel)?;
            self.write_raw(target, &raw, true, cancel)?;
            self.with_conn(|c| c.delete_file(src))?;
            self.file_done();
        }
        Ok(())
    }

    fn upload(&mut self, src: &Path, dst_dir: &Path, policy: Policy, mono: Option<crate::audio::stats::Fold>, cancel: &AtomicBool, errors: &mut Vec<String>) -> Result<(), String> {
        if src.is_dir() {
            let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let Some((target, exists)) = self.place_on_device(dst_dir, &name, true, policy)? else { return Ok(()) };
            if !exists {
                self.with_conn(|c| c.mkdir(&target))?;
            }
            let mut kids: Vec<PathBuf> = std::fs::read_dir(src)
                .map_err(|e| format!("{}: {e}", src.display()))?
                .filter_map(|d| d.ok().map(|d| d.path()))
                .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
                .filter(|p| p.is_dir() || crate::fs_model::is_audio_path(p))
                .collect();
            kids.sort();
            for k in kids {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                if let Err(e) = self.upload(&k, &target, policy, mono, cancel, errors) {
                    errors.push(e);
                }
            }
            return Ok(());
        }

        let name = device_name(src);
        self.progress(&name, 0);
        let mut s = crate::audio::decode::decode(src).map_err(|e| format!("{}: {e}", src.display()))?;
        if let Some(fold) = mono.filter(|_| s.channels.len() > 1) {
            let one = (0..s.frames()).map(|i| fold.at(&s.channels, i)).collect();
            s.channels = vec![one];
        }
        let Some((target, _)) = self.place_on_device(dst_dir, &name, false, policy)? else { return Ok(()) };
        // Known only once connected, which placing the file made sure of.
        let stereo = mono.is_none() && self.conn.as_ref().is_some_and(|c| c.stereo);
        let raw = sample::to_device(&s, stereo);
        self.write_raw(&target, &raw, false, cancel)?;
        self.file_done();
        Ok(())
    }

    /// `half`: this write follows a read of the same bytes (a device-to-device
    /// copy), and each counts for half.
    fn write_raw(&mut self, target: &Path, raw: &[u8], half: bool, cancel: &AtomicBool) -> Result<(), String> {
        if let Some(p) = self.job.as_mut() {
            p.file = None;
        }
        // `progress` borrows self, so track bytes locally while the
        // connection is borrowed and fold them back in after.
        let mut prog = self.job;
        let em = &self.em;
        let mut on = |n: u64, file_total: u64| {
            if let Some(p) = prog.as_mut() {
                p.done += if half { n - n / 2 } else { n };
                let file_done = p.file.filter(|f| f.0 == "sending").map_or(0, |f| f.1) + n;
                p.file = Some(("sending", file_done, file_total));
                // Every chunk: they are slow enough that this is cheap, and
                // small samples would otherwise never show movement.
                p.last = p.done;
                em.send(p.ev(""));
            }
        };
        let Some(c) = self.conn.as_mut() else { return Err("not connected".into()) };
        let res = c.write(target, raw, cancel, &mut on);
        let res = match res {
            Ok(()) => Ok(()),
            Err(Fail::Refused(e)) => Err(e),
            Err(Fail::Link(e)) => {
                self.conn = None;
                self.em.send(Ev::DeviceStatus(Err(e.clone())));
                Err(e)
            }
        };
        if prog.is_some() {
            self.job = prog;
        }
        res
    }

    /// `half`: see [`Self::write_raw`].
    fn read_raw(&mut self, src: &Path, half: bool, cancel: &AtomicBool) -> Result<Vec<u8>, String> {
        if let Some(p) = self.job.as_mut() {
            p.file = None;
        }
        // `progress` borrows self, so track bytes locally while the
        // connection is borrowed and fold them back in after.
        let mut prog = self.job;
        let em = &self.em;
        let mut on = |n: u64, file_total: u64| {
            if let Some(p) = prog.as_mut() {
                p.done += if half { n / 2 } else { n };
                let file_done = p.file.filter(|f| f.0 == "reading").map_or(0, |f| f.1) + n;
                p.file = Some(("reading", file_done, file_total));
                // Every chunk: they are slow enough that this is cheap, and
                // small samples would otherwise never show movement.
                p.last = p.done;
                em.send(p.ev(""));
            }
        };
        let Some(c) = self.conn.as_mut() else { return Err("not connected".into()) };
        let res = c.read(src, cancel, &mut on);
        let res = match res {
            Ok(v) => Ok(v),
            Err(Fail::Refused(e)) => Err(e),
            Err(Fail::Link(e)) => {
                self.conn = None;
                self.em.send(Ev::DeviceStatus(Err(e.clone())));
                Err(e)
            }
        };
        if prog.is_some() {
            self.job = prog;
        }
        res
    }

    fn download(&mut self, src: &Path, dst_dir: &Path, policy: Policy, cancel: &AtomicBool, errors: &mut Vec<String>) -> Result<(), String> {
        let entry = self.stat(src)?;
        if entry.is_dir {
            let target = dst_dir.join(&entry.name);
            let target = match (target.exists(), policy) {
                (false, _) | (true, Policy::Replace) => target,
                (true, Policy::Skip) => return Ok(()),
                (true, Policy::KeepBoth) => crate::fs_model::unique_sibling(&target),
            };
            std::fs::create_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
            let kids = self.with_conn(|c| c.list(src))?;
            for k in kids {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                if let Err(e) = self.download(&src.join(&k.name), &target, policy, cancel, errors) {
                    errors.push(e);
                }
            }
            return Ok(());
        }

        let target = dst_dir.join(wav_name(&entry.name));
        let target = match (target.exists(), policy) {
            (false, _) | (true, Policy::Replace) => target,
            (true, Policy::Skip) => return Ok(()),
            (true, Policy::KeepBoth) => crate::fs_model::unique_sibling(&target),
        };
        self.progress(&entry.name, 0);
        let raw = self.read_raw(src, false, cancel)?;
        std::fs::create_dir_all(dst_dir).map_err(|e| format!("{}: {e}", dst_dir.display()))?;
        sample::write_wav(&raw, &target).map_err(|e| format!("{}: {e}", target.display()))?;
        self.file_done();
        Ok(())
    }

    fn copy(&mut self, src: &Path, dst_dir: &Path, policy: Policy, cancel: &AtomicBool, errors: &mut Vec<String>) -> Result<(), String> {
        if dst_dir.starts_with(src) {
            return Err(format!("{}: cannot copy a folder into itself", dev_str(src)));
        }
        let entry = self.stat(src)?;
        // Copying into the same folder always makes a second item.
        let policy = if src.parent() == Some(dst_dir) { Policy::KeepBoth } else { policy };
        let Some((target, exists)) = self.place_on_device(dst_dir, &entry.name, entry.is_dir, policy)? else { return Ok(()) };
        if entry.is_dir {
            if !exists {
                self.with_conn(|c| c.mkdir(&target))?;
            }
            let kids = self.with_conn(|c| c.list(src))?;
            for k in kids {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                if let Err(e) = self.copy(&src.join(&k.name), &target, policy, cancel, errors) {
                    errors.push(e);
                }
            }
            return Ok(());
        }
        self.progress(&entry.name, 0);
        let raw = self.read_raw(src, true, cancel)?;
        self.write_raw(&target, &raw, true, cancel)?;
        self.file_done();
        Ok(())
    }
}
