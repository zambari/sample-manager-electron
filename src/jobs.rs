//! Off-thread filesystem work.
//!
//! Nothing here touches egui except [`egui::Context::request_repaint`], which
//! is how a worker wakes the (otherwise idle) UI when a result is ready.
//!
//! The UI never blocks: it posts a request, keeps rendering from cached state,
//! and picks up [`Ev`] messages in `App::logic`.

use crate::fs_model::{self, Entry};

use crate::ops::{Kind, Op};
use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::Arc;

/// Results flowing worker -> UI.
pub enum Ev {
    Listed {
        /// Generation of the request, so a slow listing for a directory the
        /// user has already navigated away from can be discarded.
        generation: u64,
        path: PathBuf,
        result: Result<Vec<Entry>, String>,
    },
    DirSize {
        path: PathBuf,
        bytes: u64,
        files: u64,
    },
    Progress {
        job: u64,
        done_bytes: u64,
        total_bytes: u64,
        done_items: usize,
        total_items: usize,
        label: String,
    },
    Finished {
        job: u64,
        /// Directories that changed and should be re-listed.
        touched: Vec<PathBuf>,
        errors: Vec<String>,
        cancelled: bool,
    },
}

pub struct Jobs {
    tx: Sender<Ev>,
    rx: Receiver<Ev>,
    ctx: egui::Context,
    next_job: AtomicU64,
    cancels: HashMap<u64, Arc<AtomicBool>>,
}

impl Jobs {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            ctx,
            next_job: AtomicU64::new(1),
            cancels: HashMap::new(),
        }
    }

    /// Drain everything the workers have produced. Call once per `logic` pass.
    pub fn drain(&mut self) -> Vec<Ev> {
        self.rx.try_iter().collect()
    }

    pub fn cancel(&self, job: u64) {
        if let Some(flag) = self.cancels.get(&job) {
            flag.store(true, Ordering::Relaxed);
        }
    }

    pub fn forget(&mut self, job: u64) {
        self.cancels.remove(&job);
    }

    fn spawn(&self, name: &str, f: impl FnOnce(Emitter) + Send + 'static) {
        let em = Emitter {
            tx: self.tx.clone(),
            ctx: self.ctx.clone(),
        };
        // A detached thread per request. Listings and size walks are bounded by
        // user action, and a long copy must not block a directory listing --
        // which is exactly what a single shared worker thread would do.
        let _ = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || f(em));
    }

    pub fn list(&self, path: PathBuf, generation: u64, show_hidden: bool) {
        self.spawn("list", move |em| {
            let result = fs_model::list_dir(&path, show_hidden).map_err(|e| e.to_string());
            em.send(Ev::Listed { generation, path, result });
        });
    }

    /// Walk a directory to get its recursive size. This is the one genuinely
    /// expensive read, which is why it is explicit (⌘I) and cached.
    pub fn dir_size(&self, path: PathBuf) {
        self.spawn("dir_size", move |em| {
            let mut bytes = 0u64;
            let mut files = 0u64;
            for de in walkdir::WalkDir::new(&path).follow_links(false) {
                let Ok(de) = de else { continue };
                if de.file_type().is_file() {
                    if let Ok(md) = de.metadata() {
                        bytes += md.len();
                        files += 1;
                    }
                }
            }
            em.send(Ev::DirSize { path, bytes, files });
        });
    }

    /// Execute an already-conflict-resolved op list. Returns the job id so the
    /// UI can show progress and offer cancellation.
    pub fn execute(&mut self, kind: Kind, ops: Vec<Op>) -> u64 {
        let job = self.next_job.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels.insert(job, cancel.clone());
        self.spawn("execute", move |em| execute(em, job, kind, ops, cancel));
        job
    }

    pub fn trash(&mut self, paths: Vec<PathBuf>) -> u64 {
        let job = self.next_job.fetch_add(1, Ordering::Relaxed);
        self.cancels.insert(job, Arc::new(AtomicBool::new(false)));
        self.spawn("trash", move |em| {
            let touched: Vec<PathBuf> = paths
                .iter()
                .filter_map(|p| p.parent().map(Path::to_path_buf))
                .collect();
            // Move to the OS recycle bin, never unlink. Losing a one-off
            // recording to a stray keypress is not recoverable.
            let ctx = {
                #[allow(unused_mut)]
                let mut ctx = trash::TrashContext::default();
                // The default asks Finder over AppleScript, which can stall on
                // an automation-permission prompt and finish after we've
                // already re-listed the folder, leaving ghost rows. The
                // file-manager call is synchronous and needs no permission.
                #[cfg(target_os = "macos")]
                {
                    use trash::macos::{DeleteMethod, TrashContextExtMacos};
                    ctx.set_delete_method(DeleteMethod::NsFileManager);
                }
                ctx
            };
            let errors = match ctx.delete_all(&paths) {
                Ok(()) => Vec::new(),
                Err(e) => vec![e.to_string()],
            };
            em.send(Ev::Finished {
                job,
                touched,
                errors,
                cancelled: false,
            });
        });
        job
    }
}

/// Worker-side handle for sending events and waking the UI.
pub struct Emitter {
    tx: Sender<Ev>,
    ctx: egui::Context,
}

impl Emitter {
    fn send(&self, ev: Ev) {
        // If the receiver is gone the app is shutting down; drop silently.
        if self.tx.send(ev).is_ok() {
            self.ctx.request_repaint();
        }
    }
}

const CHUNK: usize = 256 * 1024;
/// Repaint-worthy progress granularity. Sending an event per chunk would wake
/// the UI thousands of times a second for no visible benefit.
const PROGRESS_INTERVAL_BYTES: u64 = 4 * 1024 * 1024;

fn execute(em: Emitter, job: u64, kind: Kind, ops: Vec<Op>, cancel: Arc<AtomicBool>) {
    let total_bytes: u64 = ops.iter().map(|o| o.approx_bytes).sum();
    let total_items = ops.len();
    let mut done_bytes = 0u64;
    let mut last_reported = 0u64;
    let mut errors = Vec::new();
    let mut touched: Vec<PathBuf> = Vec::new();

    for (i, op) in ops.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }

        em.send(Ev::Progress {
            job,
            done_bytes,
            total_bytes,
            done_items: i,
            total_items,
            label: op
                .src
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        });

        let mut on_bytes = |n: u64| {
            done_bytes += n;
            if done_bytes - last_reported >= PROGRESS_INTERVAL_BYTES {
                last_reported = done_bytes;
                em.send(Ev::Progress {
                    job,
                    done_bytes,
                    total_bytes,
                    done_items: i,
                    total_items,
                    label: String::new(),
                });
            }
        };

        let res = run_one(op, kind, &cancel, &mut on_bytes);
        match res {
            Ok(()) => {
                if let Some(p) = op.dst.parent() {
                    if !touched.iter().any(|t| t == p) {
                        touched.push(p.to_path_buf());
                    }
                }
                if kind == Kind::Move {
                    if let Some(p) = op.src.parent() {
                        if !touched.iter().any(|t| t == p) {
                            touched.push(p.to_path_buf());
                        }
                    }
                }
            }
            Err(e) => errors.push(format!("{}: {e}", op.src.display())),
        }
    }

    em.send(Ev::Finished {
        job,
        touched,
        errors,
        cancelled: cancel.load(Ordering::Relaxed),
    });
}

fn run_one(
    op: &Op,
    kind: Kind,
    cancel: &AtomicBool,
    on_bytes: &mut impl FnMut(u64),
) -> std::io::Result<()> {
    if let Some(parent) = op.dst.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // `conflict: false` after `ops::resolve` means the destination is either
    // free or we were told to replace it.
    if op.dst.exists() {
        if op.dst.is_dir() {
            // TODO: merge instead of replace. Replacing a whole folder is the
            // conservative reading of "Replace", but a sample library commit
            // usually wants a merge. Needs per-file conflict resolution.
            std::fs::remove_dir_all(&op.dst)?;
        } else {
            std::fs::remove_file(&op.dst)?;
        }
    }

    if kind == Kind::Move {
        // Same-volume moves are a rename: instant, and no progress to report.
        match std::fs::rename(&op.src, &op.dst) {
            Ok(()) => {
                on_bytes(op.approx_bytes);
                return Ok(());
            }
            // Cross-device: fall through to copy-then-delete.
            Err(_) => {}
        }
    }

    if op.is_dir {
        copy_tree(&op.src, &op.dst, cancel, on_bytes)?;
    } else {
        copy_file(&op.src, &op.dst, cancel, on_bytes)?;
    }

    if kind == Kind::Move {
        if op.is_dir {
            std::fs::remove_dir_all(&op.src)?;
        } else {
            std::fs::remove_file(&op.src)?;
        }
    }
    Ok(())
}

fn copy_tree(
    src: &Path,
    dst: &Path,
    cancel: &AtomicBool,
    on_bytes: &mut impl FnMut(u64),
) -> std::io::Result<()> {
    for de in walkdir::WalkDir::new(src).follow_links(false) {
        if cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "cancelled",
            ));
        }
        let de = de.map_err(std::io::Error::other)?;
        let rel = de
            .path()
            .strip_prefix(src)
            .map_err(std::io::Error::other)?;
        let target = dst.join(rel);
        if de.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else if de.file_type().is_file() {
            copy_file(de.path(), &target, cancel, on_bytes)?;
        }
        // Symlinks are skipped for now; recreating them correctly is
        // platform-specific and not needed for sample folders.
    }
    Ok(())
}

fn copy_file(
    src: &Path,
    dst: &Path,
    cancel: &AtomicBool,
    on_bytes: &mut impl FnMut(u64),
) -> std::io::Result<()> {
    let mut r = std::io::BufReader::new(std::fs::File::open(src)?);
    let mut w = std::io::BufWriter::new(std::fs::File::create(dst)?);
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancel.load(Ordering::Relaxed) {
            // Don't leave a half-written file behind.
            drop(w);
            let _ = std::fs::remove_file(dst);
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "cancelled",
            ));
        }
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        w.write_all(&buf[..n])?;
        on_bytes(n as u64);
    }
    w.flush()?;
    Ok(())
}
