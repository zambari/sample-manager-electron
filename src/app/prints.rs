//! Keeping audio fingerprints ([`crate::prints`]) of what the editor has
//! open, and matching a local file to a pool slot with them.

use super::*;
use crate::prints::Ask;
use std::cell::Cell;
use std::collections::HashSet;
use std::path::Path;

type Key = (u32, u32);

/// The background fingerprint run.
#[derive(Default)]
pub(super) struct PrintRun {
    /// A worker thread is on it: one at a time.
    busy: bool,
    at: Option<std::time::Instant>,
    /// Local files that couldn't be read as audio: not asked again this run.
    failed: HashSet<PathBuf>,
    /// Downloaded copies already asked about: `(device sample, file, mtime)`.
    tried: HashSet<(Key, PathBuf, u64)>,
}

/// The pool slot a local file is on the device as.
pub(super) struct PoolMatch {
    /// Slot label, e.g. `A012`.
    pub slot: String,
    pub key: Key,
    /// Same audio (`true`), different (`false`), or not compared yet: a slot
    /// found by name whose sample was never fetched.
    pub same: Option<bool>,
    /// Found only by file name, not by link or fingerprint.
    pub by_name: bool,
}

impl App {
    /// Every few seconds: fingerprint the editor's files (the sample, the
    /// kit and the files its slices came from) when new or changed, and
    /// pool samples that have an unchanged downloaded copy. A stat per file
    /// on a worker; decoding only what changed.
    pub(super) fn print_tick(&mut self, ctx: &egui::Context) {
        const EVERY: std::time::Duration = std::time::Duration::from_secs(3);
        if self.print_run.busy || self.print_run.at.is_some_and(|t| t.elapsed() < EVERY) {
            return;
        }
        self.print_run.at = Some(std::time::Instant::now());
        ctx.request_repaint_after(EVERY);

        let mut want: Vec<PathBuf> = Vec::new();
        if let Some(p) = self.browser().path() {
            want.push(p.to_path_buf());
        }
        if self.has_kit() {
            let kit = self.kit();
            if let Some(p) = kit.path() {
                want.push(p.to_path_buf());
            }
            let (names, sources) = kit.slice_sources();
            want.extend(names.iter().map(PathBuf::from).filter(|p| p.is_absolute()));
            want.extend(sources.iter().cloned());
        }
        want.sort();
        want.dedup();
        let mut ask = Ask::default();
        for p in want {
            if self.print_run.failed.contains(&p) {
                continue;
            }
            let had = self.prints.local(&p).map(|r| (r.len, r.mtime));
            ask.local.push((p, had));
        }

        if let Some(proj) = &self.elek.project {
            for i in 1..proj.pool.len() {
                let e = self.elek.entry(i);
                if e.is_empty() || self.prints.device(e.key()).is_some() {
                    continue;
                }
                let k = e.key();
                if let Some(l) = self.links.downloads(k.0, k.1).next() {
                    if self.print_run.tried.insert((k, l.local.clone(), l.mtime)) {
                        ask.device.push((k, l.local.clone(), (l.len, l.mtime)));
                    }
                }
            }
        }
        if ask.is_empty() {
            return;
        }
        self.print_run.busy = true;
        let em = self.jobs.emitter();
        std::thread::spawn(move || em.send(crate::jobs::Ev::Printed(ask.run())));
    }

    pub(super) fn prints_landed(&mut self, found: crate::prints::Found) {
        self.print_run.busy = false;
        self.print_run.failed.extend(found.failed.iter().cloned());
        self.prints.absorb(found);
    }

    /// The pool slot local file `p` is on the device as, with no disk
    /// access. In order: a transfer link (the +Drive hash), the audio
    /// fingerprint, and last a slot whose +Drive file has the same name
    /// (then `same` says whether its audio still matches, when known).
    pub(super) fn pool_match(&self, p: &Path) -> Option<PoolMatch> {
        self.elek.project.as_ref()?;
        let print = self.prints.local(p);
        let found = Cell::new(None);
        let find = |m: &dyn Fn(Key) -> bool| {
            self.elek
                .slot_holding(|k| {
                    let hit = m(k);
                    if hit {
                        found.set(Some(k));
                    }
                    hit
                })
                .zip(found.get())
        };
        if let Some(l) = self.links.for_local(p) {
            if let Some((slot, key)) = find(&|k| l.is(k)) {
                // The print is re-made when the file changes: a stamp other
                // than the link's means it was edited since the transfer.
                let same = print.is_none_or(|r| (r.len, r.mtime) == (l.len, l.mtime));
                return Some(PoolMatch { slot, key, same: Some(same), by_name: false });
            }
        }
        if let Some(r) = print {
            let by_audio = |k: Key| self.prints.device(k).is_some_and(|d| r.keys.contains(&d));
            if let Some((slot, key)) = find(&by_audio) {
                return Some(PoolMatch { slot, key, same: Some(true), by_name: false });
            }
        }
        let stem = |s: &str| {
            let n = s.rsplit(['/', '\\']).next().unwrap_or(s);
            n.rsplit_once('.').map_or(n, |(a, _)| a).to_lowercase()
        };
        let mine = stem(&p.to_string_lossy());
        if mine.is_empty() {
            return None;
        }
        let names = &self.elek.names;
        let (slot, key) = find(&|k| names.get(&k).is_some_and(|n| stem(n) == mine))?;
        Some(PoolMatch { slot, key, same: self.prints.same(p, key), by_name: true })
    }
}
