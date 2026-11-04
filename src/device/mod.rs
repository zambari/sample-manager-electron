//! An Elektron machine's +Drive (Digitakt, Digitakt II, Analog Rytm,
//! Model:Samples), browsed over USB MIDI.
//!
//! One worker thread owns the MIDI connection and runs every request in
//! order: the device answers one API call at a time, so there is nothing to
//! gain from parallel requests and a lot to lose. The UI posts [`Cmd`]s and
//! gets results back as ordinary [`crate::jobs::Ev`]s. While a job runs, the
//! worker answers folder listings between its steps (down to single transfer
//! chunks) so browsing stays responsive; other jobs wait their turn.
//!
//! Device paths are plain absolute paths (`/samples/kicks`) stored in
//! `PathBuf`s; they never touch the local filesystem.

mod proto;
mod sample;

use crate::jobs::{Emitter, Ev};
use crate::ops::Policy;
use proto::{Args, DirEntry, Reader};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

/// Where elk-herd (and we) put deleted items, so a stray keypress is undoable.
pub const TRASH: &str = "/trash";

const TIMEOUT: Duration = Duration::from_secs(5);
/// Times one read chunk is asked for before the file counts as failed.
const CHUNK_TRIES: u32 = 4;
/// Times a whole file is read before it is reported as failed.
const FILE_TRIES: u32 = 3;
const PROGRESS_EVERY: u64 = 64 * 1024;
/// How often to check that the device is still plugged in.
const PROBE_EVERY: Duration = Duration::from_secs(1);

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
            .spawn(move || Worker { conn: None, em, job: None, rx, later: VecDeque::new(), counted: HashMap::new(), counting: false, busy: Vec::new(), merged: (0, 0, 0) }.run());
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

/// A device path as the device (and the user) expect it: rooted, with `/`
/// separators. On Windows, `Path::join` inserts `\`, which the device
/// would read as part of a file name.
pub fn dev_str(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
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
    /// The MIDI input port's own name, to notice when it disappears.
    port: String,
    /// The port was found gone: every request fails from now on.
    dead: bool,
    probed: Instant,
}

/// Is a MIDI input called `name` still there? A fresh client each time:
/// a long-lived one may not see ports come and go without a run loop.
fn port_present(name: &str) -> bool {
    let Ok(input) = midir::MidiInput::new("Sample Manager probe") else { return true };
    input.ports().iter().any(|p| input.port_name(p).ok().as_deref() == Some(name))
}

/// Could this MIDI port be an Elektron machine?
fn elektron_port(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    ["elektron", "digitakt", "digitone", "syntakt", "rytm", "model:samples", "model:cycles", "analog four", "analog keys", "analog heat"]
        .iter()
        .any(|k| l.contains(k))
}

/// Elektron product ids (the first byte of the device reply), as listed by
/// Elektroid: `(name, stereo samples)`. Whether a machine has a sample drive
/// is read from the commands it says it supports, not from here.
fn product(id: u8) -> Option<(&'static str, bool)> {
    Some(match id {
        4 => ("Analog Four", false),
        6 => ("Analog Keys", false),
        8 => ("Analog Rytm", false),
        10 => ("Analog Heat", false),
        12 => ("Digitakt", false),
        14 => ("Analog Four MKII", false),
        16 => ("Analog Rytm MKII", false),
        20 => ("Digitone", false),
        22 => ("Analog Heat MKII", false),
        25 => ("Model:Samples", false),
        27 => ("Model:Cycles", false),
        28 => ("Digitone Keys", false),
        30 => ("Syntakt", false),
        32 => ("Analog Heat +FX", false),
        42 => ("Digitakt II", true),
        43 => ("Digitone II", false),
        _ => return None,
    })
}

/// How long to wait for a port's first answer. Short, so trying a port that
/// isn't a usable device doesn't hold up the others.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(1500);

impl Conn {
    /// Try each Elektron-looking port in turn; the first that answers and
    /// has a sample drive wins.
    fn open() -> Result<Self, String> {
        let input = midir::MidiInput::new("Sample Manager").map_err(|e| e.to_string())?;
        let names: Vec<String> =
            input.ports().iter().filter_map(|p| input.port_name(p).ok()).filter(|n| elektron_port(n)).collect();
        drop(input);
        if names.is_empty() {
            return Err("No Elektron device found. Connect it by USB \
                        (USB config: Overbridge or USB MIDI) and press Refresh."
                .into());
        }
        let mut last = String::new();
        for name in names {
            match Self::open_port(&name) {
                Ok(c) => {
                    crate::log::line(format!("device: connected to {} on port {name}", c.name));
                    return Ok(c);
                }
                Err(e) => {
                    crate::log::line(format!("device: port {name}: {e}"));
                    last = e;
                }
            }
        }
        Err(last)
    }

    fn open_port(in_name: &str) -> Result<Self, String> {
        let mut input = midir::MidiInput::new("Sample Manager").map_err(|e| e.to_string())?;
        input.ignore(midir::Ignore::None);
        let output = midir::MidiOutput::new("Sample Manager").map_err(|e| e.to_string())?;

        let in_port = input
            .ports()
            .into_iter()
            .find(|p| input.port_name(p).ok().as_deref() == Some(in_name))
            .ok_or("port went away")?;
        // The output of the same device carries the same name; failing that,
        // the only Elektron output there is.
        let outs: Vec<_> = output.ports().into_iter().filter_map(|p| Some((output.port_name(&p).ok()?, p))).collect();
        let out_port = match outs.iter().find(|(n, _)| n == in_name) {
            Some((_, p)) => p.clone(),
            None => {
                let mut el = outs.iter().filter(|(n, _)| elektron_port(n));
                match (el.next(), el.next()) {
                    (Some((_, p)), None) => p.clone(),
                    _ => return Err("no matching MIDI output".into()),
                }
            }
        };

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

        let mut c = Self {
            out,
            _input,
            rx,
            next: 0,
            stereo: false,
            port: in_name.to_owned(),
            name: in_name.to_owned(),
            dead: false,
            probed: Instant::now(),
        };
        let r = c
            .request_within(proto::DEVICE, Args::default(), HANDSHAKE_TIMEOUT)
            .map_err(|_| "no answer (is USB config set to Overbridge or USB MIDI?)".to_owned())?;
        let mut rd = Reader::new(&r);
        let id = rd.u8().map_err(|e| format!("bad device reply: {e}"))?;
        // Message ids the device supports, then its name.
        let mut cmds = Vec::new();
        if let Ok(n) = rd.u8() {
            for _ in 0..n {
                cmds.extend(rd.u8());
            }
        }
        let known = product(id);
        c.name = match rd.str() {
            Ok(n) if !n.is_empty() => n,
            _ => known.map_or_else(|| format!("Elektron device {id}"), |k| format!("Elektron {}", k.0)),
        };
        crate::log::line(format!("device: {} (product {id}) on {in_name}, commands {cmds:02x?}", c.name));
        // An older firmware may not list its commands; then just try.
        let needed = [proto::DIR_LIST, proto::READ_OPEN, proto::WRITE_OPEN];
        if !cmds.is_empty() && !needed.iter().all(|n| cmds.contains(n)) {
            return Err(format!("{} has no sample drive to browse", c.name));
        }
        c.stereo = known.is_some_and(|k| k.1);
        Ok(c)
    }

    /// At most every [`PROBE_EVERY`], check the port is still there.
    /// Returns false once it is gone.
    fn probe(&mut self) -> bool {
        if !self.dead && self.probed.elapsed() >= PROBE_EVERY {
            self.probed = Instant::now();
            if !port_present(&self.port) {
                crate::log::line(format!("device: port {} disappeared", self.port));
                self.dead = true;
            }
        }
        !self.dead
    }

    fn request(&mut self, cmd: u8, args: Args) -> R<Vec<u8>> {
        self.request_within(cmd, args, TIMEOUT)
    }

    fn request_within(&mut self, cmd: u8, args: Args, timeout: Duration) -> R<Vec<u8>> {
        if self.dead {
            return Err(Fail::Link("Device disconnected".into()));
        }
        self.next = self.next.wrapping_add(1).max(1);
        let id = self.next;
        while self.rx.try_recv().is_ok() {} // stale replies
        self.out
            .send(&proto::frame(id, cmd, &args.0))
            .map_err(|e| Fail::Link(format!("MIDI send failed: {e}")))?;
        let deadline = Instant::now() + timeout;
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

    /// `on_bytes(conn, chunk, file_total)` after each chunk.
    fn read(&mut self, p: &Path, cancel: &AtomicBool, on_bytes: &mut dyn FnMut(&mut Conn, u64, u64)) -> R<Vec<u8>> {
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
                // The drive sometimes answers a chunk with "not ok" or no
                // data; asking again for the same chunk usually works.
                let mut tries = 0;
                let chunk = loop {
                    let r = self.request(proto::READ, Args::default().u32(fd).u32(len).u32(start))?;
                    let mut rd = Reader::new(&r);
                    let ok = rd.bool().map_err(Fail::Link)?;
                    let _fd = rd.u32().map_err(Fail::Link)?;
                    let _len = rd.u32().map_err(Fail::Link)?;
                    let _start = rd.u32().map_err(Fail::Link)?;
                    let _end = rd.u32().map_err(Fail::Link)?;
                    let chunk = rd.rest();
                    if ok && !chunk.is_empty() {
                        break chunk.to_vec();
                    }
                    tries += 1;
                    crate::log::line(format!("device: read chunk at {start} of {s} failed (try {tries}, ok={ok}, {} bytes)", chunk.len()));
                    if tries >= CHUNK_TRIES || cancel.load(Ordering::Relaxed) {
                        return refused(format!("read failed: {s}"));
                    }
                    std::thread::sleep(Duration::from_millis(100 * tries as u64));
                };
                data.extend_from_slice(&chunk);
                on_bytes(&mut *self, chunk.len() as u64, total as u64);
            }
            Ok(())
        })();
        let close = self.request(proto::READ_CLOSE, Args::default().u32(fd));
        res?;
        close?;
        data.truncate(total);
        Ok(data)
    }

    /// `on_bytes(conn, chunk, file_total)` after each chunk.
    fn write(&mut self, p: &Path, data: &[u8], cancel: &AtomicBool, on_bytes: &mut dyn FnMut(&mut Conn, u64, u64)) -> R<()> {
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
                on_bytes(&mut *self, chunk.len() as u64, total as u64);
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
    rx: Receiver<Cmd>,
    /// Jobs that arrived while another one ran; they go before new commands.
    later: VecDeque<Cmd>,
    /// File counts of parked jobs, taken as they arrive so the bar's total
    /// covers the whole queue; the job reuses them when it starts.
    counted: HashMap<u64, Vec<(usize, u64)>>,
    /// Counting parked jobs right now (counting serves pending commands too).
    counting: bool,
    /// Device paths the running job reads or writes (see [`involved`]).
    busy: Vec<PathBuf>,
    /// The running transfer so far: files done (written or found already
    /// there), files skipped as already there, and same-name files of
    /// another size left alone. Reported at the end of a merge.
    merged: (usize, usize, usize),
}

/// Device paths `task` reads or writes: what a rename must not touch to run
/// ahead of it.
fn involved(task: &Task) -> Vec<PathBuf> {
    match task {
        Task::Transfer { dir, items, .. } => {
            let mut v = Vec::new();
            for (src, dst) in items {
                if *dir != Dir::Upload {
                    v.push(src.clone());
                }
                if *dir != Dir::Download {
                    v.push(match dir {
                        Dir::Upload => upload_target(src, dst),
                        _ => dst.join(src.file_name().unwrap_or_default()),
                    });
                }
            }
            v
        }
        Task::Rename { from, to } => vec![from.clone(), to.clone()],
        Task::Mkdir { path } => vec![path.clone()],
        Task::Trash { paths } | Task::Delete { paths } => paths.clone(),
    }
}

/// Whether `task` can run between the current job's steps: one or two quick
/// requests, like a listing, that don't touch anything the running job or a
/// waiting one is working on. Lets a new folder be made and named while a
/// transfer runs.
fn jumps_queue(task: &Task, busy: &[PathBuf], later: &VecDeque<Cmd>) -> bool {
    match task {
        Task::Mkdir { .. } => true,
        Task::Rename { from, to } => {
            let overlaps = |p: &PathBuf| [from, to].iter().any(|q| p.starts_with(q) || q.starts_with(p));
            let queued = later.iter().flat_map(|c| match c {
                Cmd::Job { task, .. } => involved(task),
                _ => Vec::new(),
            });
            !busy.iter().cloned().chain(queued).any(|p| overlaps(&p))
        }
        _ => false,
    }
}

/// Rename `from` to `to`, refusing to replace anything already there.
fn rename_checked(c: &mut Conn, from: &Path, to: &Path) -> R<()> {
    let parent = to.parent().unwrap_or(Path::new("/"));
    let name = to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if c.list(parent)?.iter().any(|e| e.name == name) {
        return refused(format!("{} already exists", dev_str(to)));
    }
    c.rename(from, to)
}

fn entries(path: &Path, v: Vec<DirEntry>) -> Vec<crate::fs_model::Entry> {
    v.into_iter()
        .map(|d| crate::fs_model::Entry {
            path: path.join(&d.name),
            name: d.name,
            is_dir: d.is_dir,
            len: if d.is_dir { 0 } else { d.size as u64 },
            modified: None,
            is_audio: false,
            empty: false,
        })
        .collect()
}

/// Answer listings waiting in `rx` on `c`, mid-transfer; park anything else
/// in `later`. A link failure is only reported to the listing: the transfer's
/// next request hits it too and handles the reconnect.
fn serve_lists(c: &mut Conn, rx: &Receiver<Cmd>, later: &mut VecDeque<Cmd>, busy: &[PathBuf], em: &Emitter) {
    c.probe();
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            Cmd::List { path, generation } => {
                let result = c.list(&path).map(|v| entries(&path, v)).map_err(|e| e.to_string());
                em.send(Ev::Listed { device: true, generation, path, result });
            }
            Cmd::Job { job, task, .. } if jumps_queue(&task, busy, later) => {
                em.send(Prog { id: job, done: 0, last: 0, total: 0, item: 0, items: 1, file: None }.ev(""));
                let (res, at) = match &task {
                    Task::Mkdir { path } => (c.mkdir(path), path),
                    Task::Rename { from, to } => (rename_checked(c, from, to), to),
                    _ => unreachable!("jumps_queue only lets quick tasks through"),
                };
                let errors = res.err().map(|e| e.to_string()).into_iter().collect();
                let parent = at.parent().unwrap_or(Path::new("/")).to_path_buf();
                em.send(Ev::Finished { job, touched: Vec::new(), device_touched: vec![parent], errors, cancelled: false });
            }
            job => later.push_back(job),
        }
    }
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
    fn run(mut self) {
        loop {
            let cmd = match self.later.pop_front() {
                Some(c) => c,
                None if self.conn.is_some() => {
                    let probed = self.conn.as_ref().map_or_else(Instant::now, |c| c.probed);
                    match self.rx.recv_timeout(PROBE_EVERY.saturating_sub(probed.elapsed())) {
                        Ok(c) => c,
                        Err(RecvTimeoutError::Timeout) => {
                            self.check_alive();
                            continue;
                        }
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                None => match self.rx.recv() {
                    Ok(c) => c,
                    Err(_) => return,
                },
            };
            match cmd {
                Cmd::List { path, generation } => self.list_now(path, generation),
                Cmd::Job { job, cancel, task } => {
                    self.busy = involved(&task);
                    self.job(job, &cancel, task);
                    self.busy.clear();
                }
            }
        }
    }

    fn list_now(&mut self, path: PathBuf, generation: u64) {
        let result = self.with_conn(|c| c.list(&path)).map(|v| entries(&path, v));
        self.em.send(Ev::Listed { device: true, generation, path, result });
    }

    /// Drop the connection if the device's port is gone.
    fn check_alive(&mut self) {
        if self.conn.as_mut().is_some_and(|c| !c.probe()) {
            self.link_failed("Device disconnected".into());
        }
    }

    /// The link broke: drop the connection, and tell the UI whether the
    /// device is gone altogether (unplugged) or just stopped answering.
    fn link_failed(&mut self, e: String) -> String {
        let gone = self.conn.take().is_some_and(|c| c.dead || !port_present(&c.port));
        self.em.send(Ev::DeviceStatus(Err(e.clone())));
        if gone {
            self.em.send(Ev::DeviceLost);
        }
        e
    }

    /// Between a job's steps: answer waiting listings, park waiting jobs.
    fn serve_pending(&mut self) {
        self.check_alive();
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                Cmd::List { path, generation } => self.list_now(path, generation),
                // Quick enough to run between the current job's steps.
                Cmd::Job { job, cancel, task } if jumps_queue(&task, &self.busy, &self.later) => self.job(job, &cancel, task),
                job => self.later.push_back(job),
            }
        }
        self.count_parked();
    }

    /// What a job's files are counted from: its top-level paths, whether they
    /// are local (an upload), and whether the counted bytes replace the
    /// caller's guess.
    fn count_sources(task: &Task) -> Option<(Vec<PathBuf>, bool, bool)> {
        match task {
            Task::Transfer { dir, items, .. } => {
                // An upload counts its local file sizes, which is what its
                // progress adds up to (see `write_raw`'s `worth`).
                Some((items.iter().map(|(s, _)| s.clone()).collect(), *dir == Dir::Upload, true))
            }
            // Trash moves bytes; a delete just counts files.
            Task::Trash { paths } => Some((paths.clone(), false, true)),
            Task::Delete { paths } => Some((paths.clone(), false, false)),
            _ => None,
        }
    }

    /// Count the files of jobs waiting in `later` and put them on the bar now,
    /// rather than when each job's turn comes.
    fn count_parked(&mut self) {
        if self.counting {
            return;
        }
        let todo: Vec<(u64, Arc<AtomicBool>, Vec<PathBuf>, bool, bool)> = self
            .later
            .iter()
            .filter_map(|c| match c {
                Cmd::Job { job, cancel, task } if !self.counted.contains_key(job) => {
                    let (paths, local, bytes) = Self::count_sources(task)?;
                    Some((*job, cancel.clone(), paths, local, bytes))
                }
                _ => None,
            })
            .collect();
        if todo.is_empty() {
            return;
        }
        self.counting = true;
        for (job, cancel, paths, local, bytes) in todo {
            let counts = self.count_files(&paths, local, &cancel);
            self.em.send(Ev::Counted {
                job,
                items: counts.iter().map(|c| c.0).sum(),
                bytes: bytes.then(|| counts.iter().map(|c| c.1).sum()),
            });
            self.counted.insert(job, counts);
        }
        self.counting = false;
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
            Err(Fail::Link(e)) => Err(self.link_failed(e)),
            Err(Fail::Refused(e)) => Err(e),
        }
    }

    fn progress(&mut self, label: &str, n: u64) {
        self.serve_pending();
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
        // Counted while it waited: start from those totals so the bar
        // doesn't shrink back to top-level items for a moment.
        let mut counted = self.counted.remove(&job);
        let scan_bytes = Self::count_sources(&task).is_some_and(|s| s.2);
        let items = match (&counted, &task) {
            (Some(c), _) => c.iter().map(|c| c.0).sum(),
            (None, Task::Transfer { items, .. }) => items.len(),
            (None, Task::Trash { paths } | Task::Delete { paths }) => paths.len(),
            _ => 1,
        };
        let total = match (&counted, &task) {
            (Some(c), _) if scan_bytes => c.iter().map(|c| c.1).sum(),
            (_, Task::Transfer { total_bytes, .. }) => *total_bytes,
            _ => 0,
        };
        self.em.send(Prog { id: job, done: 0, last: 0, total, item: 0, items, file: None }.ev(""));
        let mut counts_for = |w: &mut Self, paths: &[PathBuf], local: bool| match counted.take() {
            Some(c) => c,
            None => {
                w.progress("Counting files…", 0);
                w.count_files(paths, local, cancel)
            }
        };

        match task {
            Task::Rename { from, to } => {
                let res = self.with_conn(|c| rename_checked(c, &from, &to));
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
                let counts = counts_for(self, &paths, false);
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
                let counts = counts_for(self, &paths, false);
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
                self.merged = (0, 0, 0);
                let srcs: Vec<PathBuf> = items.iter().map(|(s, _)| s.clone()).collect();
                let counts = counts_for(self, &srcs, dir == Dir::Upload);
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
                if policy == Policy::Merge {
                    let (done, dupes, clashes) = self.merged;
                    let added = done.saturating_sub(dupes);
                    let mut text = format!("Merged: {added} added");
                    if dupes > 0 {
                        text += &format!(", {dupes} already there skipped");
                    }
                    if clashes > 0 {
                        text += &format!(", {clashes} with the same name but a different size not copied");
                    }
                    self.em.send(Ev::Note { job, text });
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
            self.serve_pending();
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
        self.merged.0 += 1;
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
    /// `Some((path, existing))`, or `None` to skip. Replacing a file deletes
    /// it first; an existing folder is merged into. On a merge, an existing
    /// file comes back as `existing` for the caller to compare sizes.
    fn place_on_device(&mut self, dir: &Path, name: &str, is_dir: bool, policy: Policy) -> Result<Option<(PathBuf, Option<DirEntry>)>, String> {
        let listing = self.with_conn(|c| c.list(dir))?;
        let Some(existing) = listing.iter().find(|e| e.name == name) else {
            return Ok(Some((dir.join(name), None)));
        };
        match policy {
            Policy::Skip => Ok(None),
            Policy::Merge if is_dir == existing.is_dir => Ok(Some((dir.join(name), Some(existing.clone())))),
            Policy::Merge => {
                self.left_alone(&dir.join(name));
                Ok(None)
            }
            Policy::KeepBoth => {
                let taken: HashSet<String> = listing.iter().map(|e| e.name.clone()).collect();
                Ok(Some((dir.join(unique_name(&taken, name)), None)))
            }
            Policy::Replace if is_dir && existing.is_dir => Ok(Some((dir.join(name), Some(existing.clone())))),
            Policy::Replace if !is_dir && !existing.is_dir => {
                let p = dir.join(name);
                self.with_conn(|c| c.delete_file(&p))?;
                Ok(Some((p, None)))
            }
            Policy::Replace => Err(format!("{}: a {} of that name is in the way", dev_str(&dir.join(name)), if existing.is_dir { "folder" } else { "file" })),
        }
    }

    /// Merge: a file already there at the expected size. Counted as done.
    fn already_there(&mut self, bytes: u64) {
        self.merged.1 += 1;
        if let Some(p) = self.job.as_mut() {
            p.done += bytes;
        }
        self.file_done();
    }

    /// Merge: same name, different size (or file vs folder): left alone.
    fn left_alone(&mut self, at: &Path) {
        self.merged.2 += 1;
        crate::log::line(format!("merge: left in place (same name, different size): {}", dev_str(at)));
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
            self.write_raw(target, &raw, true, None, cancel)?;
            self.with_conn(|c| c.delete_file(src))?;
            self.file_done();
        }
        Ok(())
    }

    fn upload(&mut self, src: &Path, dst_dir: &Path, policy: Policy, mono: Option<crate::audio::stats::Fold>, cancel: &AtomicBool, errors: &mut Vec<String>) -> Result<(), String> {
        if src.is_dir() {
            let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let Some((target, existing)) = self.place_on_device(dst_dir, &name, true, policy)? else { return Ok(()) };
            if existing.is_none() {
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
        let Some((target, existing)) = self.place_on_device(dst_dir, &name, false, policy)? else { return Ok(()) };
        // Known only once connected, which placing the file made sure of.
        let stereo = mono.is_none() && self.conn.as_ref().is_some_and(|c| c.stereo);
        let raw = sample::to_device(&s, stereo);
        // What goes over the wire (converted, maybe folded) isn't the file's
        // size, which is what the bar's total counts: scale it to that, so
        // the bar doesn't jump when the next file starts.
        let worth = std::fs::metadata(src).map(|m| m.len()).unwrap_or(raw.len() as u64);
        if let Some(e) = existing {
            // Merging onto a file of that name: compare with what we'd send,
            // with or without the sample header.
            if [raw.len(), raw.len() - sample::HEADER_LEN].contains(&(e.size as usize)) {
                self.already_there(worth);
            } else {
                self.left_alone(&target);
            }
            return Ok(());
        }
        self.write_raw(&target, &raw, false, Some(worth), cancel)?;
        self.file_done();
        Ok(())
    }

    /// `half`: this write follows a read of the same bytes (a device-to-device
    /// copy), and each counts for half. `worth`: the bar bytes the whole
    /// write counts for, when not its own length.
    fn write_raw(&mut self, target: &Path, raw: &[u8], half: bool, worth: Option<u64>, cancel: &AtomicBool) -> Result<(), String> {
        let len = raw.len().max(1) as u64;
        let mut sent = 0u64;
        if let Some(p) = self.job.as_mut() {
            p.file = None;
        }
        // `progress` borrows self, so track bytes locally while the
        // connection is borrowed and fold them back in after.
        let mut prog = self.job;
        let (em, rx, later, busy) = (&self.em, &self.rx, &mut self.later, &self.busy);
        let mut on = |c: &mut Conn, n: u64, file_total: u64| {
            serve_lists(c, rx, later, busy, em);
            if let Some(p) = prog.as_mut() {
                p.done += match worth {
                    Some(w) => w * (sent + n) / len - w * sent / len,
                    None if half => n - n / 2,
                    None => n,
                };
                sent += n;
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
            Err(Fail::Link(e)) => Err(self.link_failed(e)),
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
        // A file that still fails after its chunk retries is read again from
        // the start (reopened), with its progress rolled back first.
        let start = self.job;
        let mut attempt = 1;
        loop {
            let res = self.read_once(src, half, cancel);
            match res {
                Err(e) if attempt < FILE_TRIES && self.conn.is_some() && !cancel.load(Ordering::Relaxed) && e.starts_with("read failed") => {
                    crate::log::line(format!("device: {e} (attempt {attempt}), reading it again"));
                    attempt += 1;
                    if start.is_some() {
                        self.job = start;
                        if let Some(p) = self.job.as_mut() {
                            p.file = None;
                            self.em.send(p.ev(""));
                        }
                    }
                    std::thread::sleep(Duration::from_millis(300));
                }
                res => {
                    if res.is_ok() && attempt > 1 {
                        crate::log::line(format!("device: {} read fine on attempt {attempt}", dev_str(src)));
                    }
                    return res;
                }
            }
        }
    }

    fn read_once(&mut self, src: &Path, half: bool, cancel: &AtomicBool) -> Result<Vec<u8>, String> {
        // `progress` borrows self, so track bytes locally while the
        // connection is borrowed and fold them back in after.
        let mut prog = self.job;
        let (em, rx, later, busy) = (&self.em, &self.rx, &mut self.later, &self.busy);
        let mut on = |c: &mut Conn, n: u64, file_total: u64| {
            serve_lists(c, rx, later, busy, em);
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
            Err(Fail::Link(e)) => Err(self.link_failed(e)),
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
                (false, _) | (true, Policy::Replace | Policy::Merge) => target,
                (true, Policy::Skip) => return Ok(()),
                (true, Policy::KeepBoth) => crate::fs_model::unique_sibling(&target),
            };
            if policy == Policy::Merge && target.exists() && !target.is_dir() {
                self.left_alone(&target);
                return Ok(());
            }
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
            (true, Policy::Merge) => {
                // The WAV we'd write: a 44-byte header plus the sample data,
                // whether or not the device's size counts its own header.
                let have = std::fs::metadata(&target).map_or(0, |m| m.len());
                let data = entry.size as u64;
                if [data + 44, (data + 44).saturating_sub(sample::HEADER_LEN as u64)].contains(&have) {
                    self.already_there(data);
                } else {
                    self.left_alone(&target);
                }
                return Ok(());
            }
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
        let Some((target, existing)) = self.place_on_device(dst_dir, &entry.name, entry.is_dir, policy)? else { return Ok(()) };
        if entry.is_dir {
            if existing.is_none() {
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
        if let Some(e) = existing {
            if e.size == entry.size {
                self.already_there(entry.size as u64);
            } else {
                self.left_alone(&target);
            }
            return Ok(());
        }
        self.progress(&entry.name, 0);
        let raw = self.read_raw(src, true, cancel)?;
        self.write_raw(&target, &raw, true, None, cancel)?;
        self.file_done();
        Ok(())
    }
}
