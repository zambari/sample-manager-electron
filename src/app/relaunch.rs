//! Dev-only "new build ready" watcher.
//!
//! When the app runs from a cargo `target/` folder, it watches its own
//! binary. Once a newer build has landed (and stopped changing), the toolbar
//! offers to relaunch into it, so builds never have to kill a running session.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

const CHECK_EVERY: Duration = Duration::from_secs(2);
/// A build still being linked may touch the file more than once: wait until
/// the timestamp has held still this long.
const SETTLE: Duration = Duration::from_secs(2);

pub struct BuildWatch {
    /// `None` when not running from a cargo build folder: the watcher is off.
    exe: Option<PathBuf>,
    started: Option<SystemTime>,
    last_check: Instant,
    /// A newer timestamp seen on disk, and when it was first seen.
    candidate: Option<(SystemTime, Instant)>,
    ready: bool,
}

impl BuildWatch {
    pub fn new() -> Self {
        let exe = std::env::current_exe()
            .ok()
            .filter(|p| p.components().any(|c| c.as_os_str() == "target"));
        let started = exe.as_ref().and_then(|p| modified(p));
        Self {
            exe,
            started,
            last_check: Instant::now(),
            candidate: None,
            ready: false,
        }
    }

    /// A newer build is on disk and done being written.
    pub fn ready(&self) -> bool {
        self.ready
    }

    pub fn exe(&self) -> Option<&PathBuf> {
        self.exe.as_ref()
    }

    /// Call once a frame; schedules its own wake-ups.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let (Some(exe), Some(started)) = (&self.exe, self.started) else {
            return;
        };
        if self.ready {
            return;
        }
        ctx.request_repaint_after(CHECK_EVERY);
        if self.last_check.elapsed() < CHECK_EVERY {
            return;
        }
        self.last_check = Instant::now();
        let Some(now) = modified(exe) else {
            // Mid-replace: the file is briefly missing.
            self.candidate = None;
            return;
        };
        if now <= started {
            self.candidate = None;
            return;
        }
        match self.candidate {
            Some((t, since)) if t == now => {
                if since.elapsed() >= SETTLE {
                    self.ready = true;
                    ctx.request_repaint();
                }
            }
            _ => self.candidate = Some((now, Instant::now())),
        }
    }
}

fn modified(p: &PathBuf) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}
