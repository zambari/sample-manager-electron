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

mod conn;
mod elek;
pub use elek::{file, library, machines, patterns, presets, Cell, DumpMsg, ElekCmd, ElekEv, ElekProject, PatState, PoolEntry, ReadCtl, ReadFirst, SavedProject};
mod presence;
pub use presence::{Presence, connection_status, presence, start as watch_presence, switched_off};
mod proto;
pub(crate) mod sample;
mod transfer;
mod worker;

use crate::jobs::{Emitter, Ev};
use crate::ops::Policy;
use conn::{Conn, entries, port_present, rename_checked};
use proto::{Args, DirEntry, Reader};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};
use worker::{Worker, serve_lists};

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
        /// Downloads only: the local file name to write a source under, in
        /// place of its own (e.g. with a pool slot number in front).
        local_names: HashMap<PathBuf, String>,
    },
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    Mkdir {
        path: PathBuf,
    },
    Trash {
        paths: Vec<PathBuf>,
    },
    /// Delete outright, folders with everything in them.
    Delete {
        paths: Vec<PathBuf>,
    },
}

enum Cmd {
    List {
        path: PathBuf,
        generation: u64,
    },
    Job {
        job: u64,
        cancel: Arc<AtomicBool>,
        task: Task,
    },
    Elek(ElekCmd),
    /// Close the MIDI ports (the device was switched off).
    Release,
}

pub struct Device {
    tx: Sender<Cmd>,
}

impl Device {
    pub fn new(em: Emitter) -> Self {
        let (tx, rx) = channel();
        let _ = std::thread::Builder::new()
            .name("device".into())
            .spawn(move || {
                Worker {
                    conn: None,
                    em,
                    job: None,
                    rx,
                    later: VecDeque::new(),
                    counted: HashMap::new(),
                    counting: false,
                    busy: Vec::new(),
                    merged: (0, 0, 0),
                }
                .run()
            });
        Self { tx }
    }

    pub fn list(&self, path: PathBuf, generation: u64) {
        let _ = self.tx.send(Cmd::List { path, generation });
    }

    pub fn run(&self, job: u64, cancel: Arc<AtomicBool>, task: Task) {
        let _ = self.tx.send(Cmd::Job { job, cancel, task });
    }

    /// Project dump work (see [`elek`]): runs in turn with other jobs.
    pub fn elek(&self, cmd: ElekCmd) {
        let _ = self.tx.send(Cmd::Elek(cmd));
    }

    /// Switch the device off or on. Off closes the MIDI ports and refuses
    /// every request until it is switched on again.
    pub fn switch_off(&self, off: bool) {
        presence::set_off(off);
        if off {
            let _ = self.tx.send(Cmd::Release);
        }
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
/// Error suffix for a read of the device's factory content, which it
/// won't let anything copy off. The app spots it to explain once per batch.
pub const FACTORY_LOCKED: &str = "factory samples can't be copied off the device";

/// Under the device's `/factory` folder.
pub fn is_factory(p: &Path) -> bool {
    dev_str(p)
        .split('/')
        .find(|s| !s.is_empty())
        .is_some_and(|s| s.eq_ignore_ascii_case("factory"))
}

pub fn dev_str(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        s
    } else {
        format!("/{s}")
    }
}

/// Name for a file going to the device: the stem, since the device has no use
/// for `.wav`.
fn device_name(local: &Path) -> String {
    let name = local
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if crate::fs_model::is_audio_path(local) {
        local
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(name)
    } else {
        name
    }
}

/// `name 2`, `name 3`, ... not in `taken`.
fn unique_name(taken: &HashSet<String>, name: &str) -> String {
    (2..10_000)
        .map(|n| format!("{name} {n}"))
        .find(|c| !taken.contains(c))
        .unwrap_or_else(|| name.to_owned())
}

/// Where a download of `name` lands.
pub fn wav_name(name: &str) -> String {
    if name.to_ascii_lowercase().ends_with(".wav") {
        name.to_owned()
    } else {
        format!("{name}.wav")
    }
}

/// Where an upload of `local` lands in `dir`.
pub fn upload_target(local: &Path, dir: &Path) -> PathBuf {
    dir.join(device_name(local))
}
