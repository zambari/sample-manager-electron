//! Undo (⌘Z in a file pane) for file operations on the computer: moves,
//! copies, renames, new folders and moves to the Trash. Device operations
//! aren't recorded.

use super::*;

/// One undoable file operation. `App::undo` holds them, newest last.
pub(super) enum FileUndo {
    /// Items now at `.1` that came from `.0`: a move or a rename.
    Moved { pairs: Vec<(PathBuf, PathBuf)>, what: &'static str, replaced: bool },
    /// New copies; undoing sends them to the Trash.
    Copied { paths: Vec<PathBuf>, replaced: bool },
    /// Items sent to the Trash: `(original, where it landed)`.
    Trashed(Vec<(PathBuf, PathBuf)>),
    NewFolder(PathBuf),
}

/// What a running job is, so its `Ev::Applied` becomes the right undo step.
#[derive(Clone, Copy)]
pub(super) enum UndoJob {
    Copy,
    Move,
    Trash,
}

const UNDO_LIMIT: usize = 100;

impl App {
    pub(super) fn push_undo(&mut self, step: FileUndo) {
        self.undo.push(step);
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
    }

    /// A job reported what it did: keep it for undo if it's one we track.
    pub(super) fn applied(&mut self, job: u64, done: Vec<(PathBuf, PathBuf)>, replaced: bool) {
        let Some(kind) = self.undo_jobs.remove(&job) else { return };
        if done.is_empty() {
            return;
        }
        let step = match kind {
            UndoJob::Move => FileUndo::Moved { pairs: done, what: "move", replaced },
            UndoJob::Copy => FileUndo::Copied { paths: done.into_iter().map(|(_, d)| d).collect(), replaced },
            UndoJob::Trash => FileUndo::Trashed(done),
        };
        self.push_undo(step);
    }

    /// Trash right away, no confirmation: local items go to the system
    /// Trash (undoable), device items to the Digitakt's /trash.
    pub(super) fn trash_now(&mut self, on_dev: bool, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        if on_dev {
            let n = paths.len();
            self.run_device(crate::device::Task::Trash { paths }, "Moving to /trash", n, 0);
        } else {
            self.trash_local(paths);
        }
    }

    /// Send local items to the Trash, undoably.
    pub(super) fn trash_local(&mut self, paths: Vec<PathBuf>) {
        let id = self.jobs.trash(paths);
        self.undo_jobs.insert(id, UndoJob::Trash);
    }

    /// Start a local copy / move job with a progress bar.
    pub(super) fn run_local(&mut self, kind: Kind, ops: Vec<ops::Op>) -> u64 {
        self.run_local_then(kind, ops, None)
    }

    /// `run_local`, with a merge-move's clean-up afterwards.
    pub(super) fn run_local_then(&mut self, kind: Kind, ops: Vec<ops::Op>, tidy: Option<crate::jobs::Tidy>) -> u64 {
        let total_items = ops.len();
        let total_bytes = ops.iter().map(|o| o.approx_bytes).sum();
        let id = self.jobs.execute_then(kind, ops, tidy);
        self.active_jobs.push(ActiveJob {
            id,
            label: String::new(),
            done_bytes: 0,
            total_bytes,
            done_items: 0,
            total_items,
            started: true,
            file: None,
            file_bar: false,
        });
        id
    }

    pub(super) fn undo_file(&mut self) {
        if !self.active_jobs.is_empty() {
            self.status = "Undo waits until the running job finishes".to_owned();
            return;
        }
        let Some(step) = self.undo.pop() else {
            self.status = "Nothing to undo".to_owned();
            return;
        };
        let lost = " — what it replaced can't be brought back";
        match step {
            FileUndo::Moved { pairs, what, replaced } => {
                let n = self.move_back(&pairs);
                self.status = format!("Undid {what}: {n} of {} item(s) back{}", pairs.len(), if replaced { lost } else { "" });
            }
            FileUndo::Copied { paths, replaced } => {
                let there: Vec<PathBuf> = paths.into_iter().filter(|p| std::fs::symlink_metadata(p).is_ok()).collect();
                if there.is_empty() {
                    self.status = "Nothing to undo: the copies are already gone".to_owned();
                    return;
                }
                self.status = format!("Undid copy: {} item(s) moved to the Trash{}", there.len(), if replaced { lost } else { "" });
                self.jobs.trash(there);
            }
            FileUndo::Trashed(pairs) => {
                #[cfg(target_os = "macos")]
                let n = self.move_back(&pairs);
                #[cfg(not(target_os = "macos"))]
                let n = self.restore_trashed(&pairs);
                self.status = format!("Undid move to Trash: {n} of {} item(s) put back", pairs.len());
            }
            FileUndo::NewFolder(path) => {
                match std::fs::remove_dir(&path) {
                    Ok(()) => self.status = format!("Undid new folder {}", path.display()),
                    Err(_) if path.is_dir() => {
                        self.errors.push(format!("{} isn't empty any more, left it in place", path.display()))
                    }
                    Err(e) => self.errors.push(format!("{}: {e}", path.display())),
                }
                if let Some(p) = path.parent() {
                    self.reload_showing(p, false);
                }
            }
        }
    }

    /// Put each item at `.1` back at `.0`. Returns how many went back (or
    /// started going back, for moves across disks, which run as a job).
    fn move_back(&mut self, pairs: &[(PathBuf, PathBuf)]) -> usize {
        let nows: std::collections::HashSet<&Path> = pairs.iter().map(|(_, n)| n.as_path()).collect();
        // A rename that only changed case finds its old name "taken" by itself
        // on a case-insensitive disk.
        let case_only = |a: &Path, b: &Path| a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase();
        let mut todo: Vec<(PathBuf, PathBuf)> = Vec::new();
        for (old, now) in pairs {
            if std::fs::symlink_metadata(now).is_err() {
                self.errors.push(format!("{} is gone, can't move it back", now.display()));
            } else if old.exists() && !nows.contains(old.as_path()) && !case_only(old, now) {
                self.errors.push(format!("{} exists again, left {} where it is", old.display(), now.display()));
            } else {
                todo.push((old.clone(), now.clone()));
            }
        }
        // Renames that traded names (a→b, b→a) step aside first.
        let swaps = todo.iter().any(|(o, n)| nows.contains(o.as_path()) && o != n);
        if swaps {
            for (_, now) in todo.iter_mut() {
                let name = now.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let aside = fs_model::unique_sibling(&now.with_file_name(format!("{name}.undo")));
                match std::fs::rename(&*now, &aside) {
                    Ok(()) => *now = aside,
                    Err(e) => self.errors.push(format!("{}: {e}", now.display())),
                }
            }
        }

        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut slow = Vec::new();
        let mut moved = 0;
        for (old, now) in todo {
            for d in [old.parent(), now.parent()].into_iter().flatten() {
                if !dirs.iter().any(|x| x == d) {
                    dirs.push(d.to_path_buf());
                }
            }
            if let Some(p) = old.parent() {
                let _ = std::fs::create_dir_all(p);
            }
            match std::fs::rename(&now, &old) {
                Ok(()) => {
                    moved += 1;
                    self.follow_rename(&now, &old);
                }
                Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
                    let md = std::fs::symlink_metadata(&now).ok();
                    let is_dir = md.as_ref().is_some_and(|m| m.is_dir());
                    let approx_bytes = if is_dir { self.sizes.bytes_of(&now).unwrap_or(0) } else { md.map_or(0, |m| m.len()) };
                    slow.push(ops::Op { src: now, dst: old, approx_bytes, is_dir, conflict: false });
                    moved += 1;
                }
                Err(e) => self.errors.push(format!("{}: {e}", now.display())),
            }
        }
        if !slow.is_empty() {
            self.run_local(Kind::Move, slow);
        }
        for d in dirs {
            self.sizes.invalidate_subtree(&d);
            self.reload_showing(&d, false);
        }
        moved
    }

    /// Paths the app remembers (opened, committed) follow a file that moved.
    fn follow_rename(&mut self, from: &Path, to: &Path) {
        if self.opened.remove(from) {
            self.opened.insert(to.to_path_buf());
            self.opened_dirty = true;
        }
        if self.committed.remove(from) {
            self.committed.insert(to.to_path_buf());
            self.committed_dirty = true;
        }
    }

    /// Windows / Linux: the recycle bin restores by original path. Picks the
    /// most recently deleted item for each path.
    #[cfg(not(target_os = "macos"))]
    fn restore_trashed(&mut self, pairs: &[(PathBuf, PathBuf)]) -> usize {
        let want: std::collections::HashSet<&Path> = pairs.iter().map(|(o, _)| o.as_path()).collect();
        let items = match trash::os_limited::list() {
            Ok(items) => items,
            Err(e) => {
                self.errors.push(format!("can't read the recycle bin: {e}"));
                return 0;
            }
        };
        let mut pick: std::collections::HashMap<PathBuf, trash::TrashItem> = Default::default();
        for it in items {
            let orig = it.original_path();
            if !want.contains(orig.as_path()) {
                continue;
            }
            if pick.get(&orig).is_none_or(|p| p.time_deleted < it.time_deleted) {
                pick.insert(orig, it);
            }
        }
        let n = pick.len();
        let dirs: Vec<PathBuf> = pick.keys().filter_map(|p| p.parent().map(Path::to_path_buf)).collect();
        if let Err(e) = trash::os_limited::restore_all(pick.into_values()) {
            self.errors.push(format!("restore from the recycle bin failed: {e}"));
        }
        for d in dirs {
            self.sizes.invalidate_subtree(&d);
            self.reload_showing(&d, false);
        }
        n
    }
}
