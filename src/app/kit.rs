//! The slice kit: a second editor slot that collects slices picked from the
//! sample being browsed. `editor` is always the slot on screen and `other`
//! the hidden one; [`App::show_kit`] swaps them. The clipboard and prefs
//! stay with the screen, so copy / paste work across both.

use super::*;
use crate::editor::AudioClip;
use crate::ui::editor_view::KitEntry;

/// The last slice K added to the kit, for undoing it from outside the kit.
pub(super) struct KitAdd {
    kit: Option<PathBuf>,
    /// Kit and browsed-sample history stamps right after it: if either
    /// moved, something newer is what ⌘Z should undo.
    kit_stamp: u64,
    browser_stamp: u64,
    /// The source sample, if this add is what marked it as used.
    newly_committed: Option<PathBuf>,
    /// Undone already; ⇧⌘Z can redo it.
    undone: bool,
}

fn slot_name(ed: &Editor) -> Option<String> {
    let p = ed.path().or(ed.loading.as_deref())?;
    Some(
        p.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    )
}

/// A file that already is a kit this app made: carrying our slice names
/// chunk, or named "[N] name" with slice cues. Anything else (a loop from a
/// library with its own cue markers included) is source material and is
/// never saved over as a kit.
pub(super) fn is_collection(p: &Path) -> bool {
    if crate::audio::cues::is_ours(p) {
        return true;
    }
    let stem = p.file_stem().unwrap_or_default().to_string_lossy();
    crate::editor::slot_prefix(&stem).is_some() && !crate::audio::cues::read(p).is_empty()
}

/// A kit opened by browsing onto a sliced file (see [`App::open_browsed_kit`]).
pub(super) struct BrowsedKit {
    pub path: PathBuf,
    /// The kit that was active before, to go back to when this one closes.
    pub prev: Option<PathBuf>,
}

/// "Close all kits" working through the open kits one at a time, so each
/// with unsaved slices gets its own question.
pub(super) struct CloseQueue {
    /// Still to close, last first.
    todo: Vec<PathBuf>,
    /// "All but current": the kit to leave active, and whether it was on screen.
    keep: Option<(PathBuf, bool)>,
    /// Kits open when it started, for the summary.
    start: usize,
}

fn slot_key(ed: &Editor) -> Option<PathBuf> {
    ed.path().or(ed.loading.as_deref()).map(Path::to_path_buf)
}

/// A slice waiting on the "name the kit" question.
pub(super) struct KitNaming {
    pub clip: AudioClip,
    pub bits: Option<u32>,
    pub name: String,
    pub dir: PathBuf,
    /// Equal slots to lay the kit out on, or free.
    pub grid: Option<usize>,
    /// The sample the slice came from; gets the committed check once added.
    pub src: PathBuf,
}

impl App {
    pub(super) fn kit(&self) -> &Editor {
        if self.showing_kit {
            &self.editor
        } else {
            &self.other
        }
    }

    pub(super) fn kit_mut(&mut self) -> &mut Editor {
        if self.showing_kit {
            &mut self.editor
        } else {
            &mut self.other
        }
    }

    /// The slot that follows the file cursor.
    pub(super) fn browser(&self) -> &Editor {
        if self.showing_kit {
            &self.other
        } else {
            &self.editor
        }
    }

    pub(super) fn has_kit(&self) -> bool {
        let k = self.kit();
        k.current.is_some() || k.loading.is_some()
    }

    pub(super) fn kit_name(&self) -> Option<String> {
        slot_name(self.kit())
    }

    /// Every open kit, the active one included, sorted by name so the
    /// list doesn't jump around as you switch.
    pub(super) fn kit_list(&self) -> Vec<KitEntry> {
        let mut v: Vec<KitEntry> = std::iter::once((self.kit(), None))
            .chain(self.parked.iter().enumerate().map(|(i, k)| (k, Some(i))))
            .filter_map(|(k, parked)| {
                Some(KitEntry {
                    name: slot_name(k)?,
                    dirty: k.is_dirty(),
                    parked,
                })
            })
            .collect();
        v.sort_by_key(|e| e.name.to_lowercase());
        v
    }

    /// Why the kit can't be swapped out right now, if it can't.
    fn kit_busy(&self) -> Option<&'static str> {
        let k = self.kit();
        if k.loading.is_some() {
            Some("The kit is still loading — try again in a moment")
        } else if k.exporting {
            Some("The kit is still saving — try again in a moment")
        } else {
            None
        }
    }

    /// Swap parked kit `i` into the kit slot. The clipboard and prefs stay
    /// with the slot.
    fn swap_with_parked(&mut self, i: usize) {
        let slot = if self.showing_kit {
            &mut self.editor
        } else {
            &mut self.other
        };
        let p = &mut self.parked[i];
        std::mem::swap(slot, p);
        std::mem::swap(&mut slot.clip, &mut p.clip);
        std::mem::swap(&mut slot.prefs, &mut p.prefs);
        slot.rebase_prefs();
        p.rebase_prefs();
    }

    /// Fill the empty kit slot with the first parked kit.
    pub(super) fn unpark_first(&mut self) {
        self.swap_with_parked(0);
        self.parked.remove(0);
    }

    /// Move the active kit (if any) to the parked list, leaving the slot empty.
    fn park_kit(&mut self) {
        if self.has_kit() {
            let k = self.kit_mut().take();
            self.parked.push(k);
        }
        self.kit_pending = None;
        self.kit_seed = None;
    }

    /// Make parked kit `i` the one K adds to.
    pub(super) fn switch_kit(&mut self, i: usize) {
        if i >= self.parked.len() {
            return;
        }
        if let Some(e) = self.kit_busy() {
            self.status = e.to_owned();
            return;
        }
        if self.showing_kit && self.engine.is_playing() {
            self.engine.stop();
        }
        self.kit_pending = None;
        self.kit_seed = None;
        self.swap_with_parked(i);
        self.kit_grid = self.kit().grid_mode().or(self.kit_grid);
        self.status = format!("K now adds to {}", self.kit_name().unwrap_or_default());
    }

    /// The file is already open as a kit: make it the active one.
    fn switch_to_open(&mut self, p: &Path) -> bool {
        if self.kit().path() == Some(p) {
            return true;
        }
        match self.parked.iter().position(|k| k.path() == Some(p)) {
            Some(i) => {
                self.switch_kit(i);
                self.kit().path() == Some(p)
            }
            None => false,
        }
    }

    /// Any slot or parked kit with unsaved edits.
    pub(super) fn any_dirty(&self) -> bool {
        self.editor.is_dirty() || self.other.is_dirty() || self.parked.iter().any(Editor::is_dirty)
    }

    /// Put a slot with unsaved edits on screen, for the quit question.
    /// False when there's none left.
    pub(super) fn front_dirty(&mut self) -> bool {
        if self.editor.is_dirty() {
            return true;
        }
        if self.other.is_dirty() {
            self.swap_slots();
            return true;
        }
        let Some(i) = self.parked.iter().position(Editor::is_dirty) else {
            return false;
        };
        if !self.showing_kit {
            self.swap_slots();
        }
        self.swap_with_parked(i);
        true
    }

    /// Put the kit (`true`) or the browsed sample on screen.
    pub(super) fn show_kit(&mut self, on: bool) {
        if on == self.showing_kit {
            return;
        }
        if on && !self.has_kit() {
            self.status = "No slice kit yet — K adds the selection to one".to_owned();
            return;
        }
        self.swap_slots();
        if self.engine.is_playing() {
            self.engine.stop();
        }
    }

    pub(super) fn swap_slots(&mut self) {
        self.editor.prefs.stash_mono(self.showing_kit);
        self.editor.prefs.load_mono(!self.showing_kit);
        std::mem::swap(&mut self.editor, &mut self.other);
        std::mem::swap(&mut self.editor.clip, &mut self.other.clip);
        std::mem::swap(&mut self.editor.prefs, &mut self.other.prefs);
        self.editor.rebase_prefs();
        self.other.rebase_prefs();
        self.showing_kit = !self.showing_kit;
    }

    /// ⌘Z while the browsed sample (or a file list) is in front: take the
    /// last K back out of the kit, if nothing has happened since. From the
    /// sample editor, its own edits since the K come first.
    pub(super) fn undo_kit_add(&mut self, from_editor: bool) -> bool {
        let Some(m) = self.kit_add.as_ref() else {
            return false;
        };
        let live = !self.showing_kit
            && !m.undone
            && self.other.path() == m.kit.as_deref()
            && self.other.history_stamp() == m.kit_stamp
            && (!from_editor || self.editor.history_stamp() == m.browser_stamp)
            && self.other.can_undo();
        if !live {
            return false;
        }
        self.other.undo();
        let m = self.kit_add.as_mut().expect("checked above");
        m.undone = true;
        m.kit_stamp = self.other.history_stamp();
        m.browser_stamp = self.editor.history_stamp();
        if let Some(src) = m.newly_committed.clone() {
            self.committed_dirty |= self.committed.remove(&src);
        }
        self.status = format!(
            "Undid add to {} — ⇧⌘Z puts it back",
            self.kit_name().unwrap_or_default()
        );
        true
    }

    /// ⇧⌘Z right after `undo_kit_add`: put the slice back.
    pub(super) fn redo_kit_add(&mut self) -> bool {
        let Some(m) = self.kit_add.as_ref() else {
            return false;
        };
        let live = !self.showing_kit
            && m.undone
            && self.other.path() == m.kit.as_deref()
            && self.other.history_stamp() == m.kit_stamp
            && self.editor.history_stamp() == m.browser_stamp
            && self.other.can_redo();
        if !live {
            return false;
        }
        self.other.redo();
        let m = self.kit_add.as_mut().expect("checked above");
        m.undone = false;
        m.kit_stamp = self.other.history_stamp();
        m.browser_stamp = self.editor.history_stamp();
        if let Some(src) = m.newly_committed.clone() {
            self.mark_committed([src]);
        }
        self.status = format!("Added back to {}", self.kit_name().unwrap_or_default());
        true
    }

    /// K: add the browsed sample's selection to the end of the kit, or ask
    /// for a kit name when there's none yet.
    pub(super) fn add_slice(&mut self) {
        if self.showing_kit {
            self.status =
                "Showing the kit — press B to go back to the sample, then K adds its selection"
                    .to_owned();
            return;
        }
        let Some(clip) = self.editor.slice() else {
            self.status =
                "Load a sample first, then K adds its selection to the slice kit".to_owned();
            return;
        };
        if self.other.loading.is_some() {
            self.status = "The slice kit is still loading — try again in a moment".to_owned();
            return;
        }
        if self.other.current.is_some() {
            let secs = clip.frames() as f64 / clip.rate.max(1) as f64;
            // The add is one undo step of its own in the kit.
            self.other.checkpoint();
            if let Err(e) = self.other.append(&clip) {
                self.status = e;
                return;
            }
            self.other.checkpoint();
            let src = self.editor.path().map(Path::to_path_buf);
            let newly_committed = src.clone().filter(|s| !self.committed.contains(s));
            if let Some(src) = src {
                self.mark_committed([src]);
            }
            self.kit_add = Some(KitAdd {
                kit: self.other.path().map(Path::to_path_buf),
                kit_stamp: self.other.history_stamp(),
                browser_stamp: self.editor.history_stamp(),
                newly_committed,
                undone: false,
            });
            let where_ = match self.other.grid {
                Some(n) => format!(
                    "slot {}/{n} of",
                    self.other.sel.0 / self.other.frames().div_ceil(n).max(1) + 1
                ),
                None => "the end of".to_owned(),
            };
            self.status = format!(
                "Added {secs:.3}s to {where_} {} ({:.2}s) — B shows the kit, ⌘S there saves it",
                self.kit_name().unwrap_or_default(),
                self.other.sample().map_or(0.0, |s| s.seconds(s.frames())),
            );
            // A fixed-slot kit just filled its last slot: go to it.
            let fixed = self.other.grid_mode() != Some(crate::editor::AUTO_GRID);
            if let Some(n) = self.other.grid.filter(|_| fixed) {
                if self.other.kit_fill() >= n {
                    self.show_kit(true);
                    self.status = format!(
                        "Kit full ({n}/{n}) — showing {}. ⌘S saves it, B goes back to the sample",
                        self.kit_name().unwrap_or_default()
                    );
                }
            }
            return;
        }
        self.ask_kit_name(clip);
    }

    /// "New kit": start another kit from the browsed sample's selection.
    /// The current kit stays open in the list.
    pub(super) fn new_kit(&mut self) {
        if self.showing_kit {
            self.status =
                "Press B to go back to the sample, then start a new kit from its selection"
                    .to_owned();
            return;
        }
        let Some(clip) = self.editor.slice() else {
            self.status = "Load a sample first — a new kit starts from its selection".to_owned();
            return;
        };
        if let Some(e) = self.kit_busy() {
            self.status = e.to_owned();
            return;
        }
        self.ask_kit_name(clip);
    }

    fn ask_kit_name(&mut self, clip: AudioClip) {
        let bits = self.editor.sample().and_then(|s| s.bits);
        let src = self
            .editor
            .path()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let folder = src
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned());
        let name = folder.map_or_else(|| "slice kit".to_owned(), |f| format!("{f} kit"));
        let dir = self.kit_dir(&src);
        self.kit_naming = Some(KitNaming {
            clip,
            bits,
            name,
            dir,
            grid: self.kit_grid,
            src,
        });
    }

    /// New kits go in the active destination tab when it's a local folder,
    /// else beside the sample they start from.
    fn kit_dir(&self, src: &Path) -> PathBuf {
        let tab = self.pane(Role::Dest).active_tab();
        if !tab.device && !tab.loading {
            return tab.cwd.clone();
        }
        src.parent().map_or_else(dirs_home, Path::to_path_buf)
    }

    /// The name was given: start the kit, or add to the file of that name.
    pub(super) fn start_kit(&mut self, n: KitNaming) {
        let mut name = n.name.trim().replace(['/', '\\', ':'], "-");
        if name.to_lowercase().ends_with(".wav") {
            name.truncate(name.len() - 4);
        }
        if name.is_empty() {
            name = "slice kit".to_owned();
        }
        let mut path = n.dir.join(format!("{name}.wav"));
        // A plain sample of that name stays untouched: the kit gets its own.
        if path.is_file() && !is_collection(&path) {
            path = crate::fs_model::unique_sibling(&path);
        }
        let file = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        // Already open: switch to it and add the slice there.
        if self.switch_to_open(&path) {
            self.status = match self.kit_mut().append(&n.clip) {
                Ok(()) => {
                    self.mark_committed([n.src.clone()]);
                    format!(
                        "Added the slice to {file} (not saved yet) — B shows it, ⌘S there saves it"
                    )
                }
                Err(e) => format!("{file}: {e}"),
            };
            return;
        }
        if let Some(e) = self.kit_busy() {
            self.status = e.to_owned();
            return;
        }
        self.kit_grid = n.grid;
        self.park_kit();
        if !n.src.as_os_str().is_empty() {
            self.mark_committed([n.src.clone()]);
        }
        if path.is_file() {
            self.kit_pending = Some((Some(n.clip), n.grid));
            self.kit_mut().request(&path);
            self.status = format!("{file} already exists — adding the slice to it…");
        } else {
            self.kit_pending = None;
        self.kit_seed = None;
            self.kit_mut().start_new(path, &n.clip, n.bits, n.grid);
            let grid = match n.grid {
                None => String::new(),
                Some(crate::editor::AUTO_GRID) => " on an auto grid".to_owned(),
                Some(g) => format!(" on a {g}-slot grid"),
            };
            self.status = format!(
                "Started {file}{grid} (not saved yet) — K adds more, B shows it, click its tab there to rename, ⌘S saves it"
            );
        }
    }

    /// Rename the kit. A kit already on disk has its file renamed too; one
    /// that was never saved just gets the new name for when it is.
    pub(super) fn rename_kit(&mut self, name: &str) {
        let Some(old) = self.kit().path().map(Path::to_path_buf) else {
            return;
        };
        let mut name = name.trim().replace(['/', '\\', ':'], "-");
        if name.to_lowercase().ends_with(".wav") {
            name.truncate(name.len() - 4);
        }
        if name.is_empty() {
            return;
        }
        let file = format!("{name}.wav");
        let new = old.with_file_name(&file);
        if new == old {
            return;
        }
        // Case-only renames point at the same file on case-insensitive disks.
        let same_file =
            old.to_string_lossy().to_lowercase() == new.to_string_lossy().to_lowercase();
        if new.exists() && !(same_file && old.is_file() && !self.kit().path_is_source()) {
            self.status = format!("{file} already exists there — pick another name");
            return;
        }
        // A kit still pointing at a file its slices came from has no file
        // of its own: rename only the name it will be saved under.
        let on_disk = old.is_file() && !self.kit().path_is_source();
        if on_disk {
            if let Err(e) = std::fs::rename(&old, &new) {
                self.status = format!("Couldn't rename the kit: {e}");
                return;
            }
        }
        if self.voice_path.as_deref() == Some(old.as_path()) && self.engine.is_playing() {
            self.engine.stop();
        }
        self.kit_mut().set_path(new.clone());
        if on_disk {
            if let Some(dir) = new.parent() {
                self.sizes.invalidate_subtree(dir);
                self.reload_showing(dir, false);
            }
            self.status = format!("Renamed the kit to {file}");
        } else {
            self.status = format!("The kit is now {file} (not saved yet)");
        }
    }

    /// "Start New Grid Kit": open `p` as a kit and make it the one K adds to. The
    /// kit open before stays in the kit list.
    pub(super) fn use_as_kit(&mut self, p: &Path) {
        let name = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if self.switch_to_open(p) {
            self.status = format!(
                "{name} is the active kit — K adds the selection of whatever sample you're on"
            );
            return;
        }
        if !self.showing_kit && self.editor.path() == Some(p) && self.editor.is_dirty() {
            self.status = "Save this file first, then make it a kit".to_owned();
            return;
        }
        if let Some(e) = self.kit_busy() {
            self.status = e.to_owned();
            return;
        }
        if self.showing_kit && self.engine.is_playing() {
            self.engine.stop();
        }
        self.park_kit();
        self.kit_mut().request(p);
        // Lay it out on the grid picked for new kits once it loads.
        self.kit_pending = Some((None, self.kit_grid));
        // A plain sample only seeds the kit: it gets a name of its own, so
        // saving never writes over the sample in the library.
        if !is_collection(p) {
            let stem = p.file_stem().unwrap_or_default().to_string_lossy();
            let new = self.kit_dir(p).join(format!("{stem} kit.wav"));
            self.kit_seed = Some((p.to_path_buf(), crate::fs_model::unique_sibling(&new)));
        }
        let others = if self.parked.is_empty() {
            String::new()
        } else {
            " (the other kits stay in the ▾ list)".to_owned()
        };
        self.status = format!(
            "{name} is the active kit — K adds the selection of whatever sample you're on{others}"
        );
    }

    /// Browsing onto an audio file. A "[N] name" file with slice markers in
    /// it opens as a kit and is shown: returns true. Anything else closes the
    /// unedited kit the last such file opened and returns false, so the file
    /// loads as a plain sample.
    pub(super) fn open_browsed_kit(&mut self, p: &Path, play: bool) -> bool {
        let stem = p.file_stem().unwrap_or_default().to_string_lossy();
        let sliced =
            crate::editor::slot_prefix(&stem).is_some() && !crate::audio::cues::read(p).is_empty();
        let held_as_sample =
            !self.showing_kit && self.editor.path() == Some(p) && self.editor.is_dirty();
        if !sliced || held_as_sample {
            self.drop_browsed_kit(true);
            return false;
        }
        if self.engine.is_playing() {
            self.engine.stop();
        }
        let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if self.browsed_kit.as_ref().is_some_and(|b| b.path == p) || self.switch_to_open(p) {
            if self.kit().path() == Some(p) || self.kit().loading.as_deref() == Some(p) {
                self.show_kit(true);
                if play && self.kit().loading.is_none() {
                    self.start_play();
                }
                return true;
            }
        }
        // Replace the last browsed kit, and hand on which kit to go back to.
        let dropped = self.drop_browsed_kit(false);
        if let Some(e) = self.kit_busy() {
            self.status = e.to_owned();
            return false;
        }
        let prev = dropped.unwrap_or_else(|| self.kit().path().map(Path::to_path_buf));
        // Empty if the browsed kit was just closed out of the slot.
        self.park_kit();
        self.kit_mut().request(p);
        self.kit_mut().play_when_loaded = play;
        self.browsed_kit = Some(BrowsedKit {
            path: p.to_path_buf(),
            prev,
        });
        self.show_kit(true);
        self.status = format!(
            "{name} has slice markers — opened as a kit (B goes back to the sample). Closes again when you browse on, unless you edit it"
        );
        true
    }

    /// Close the kit browsing opened, unless it has been edited (it's a
    /// kit you're working on then, and stays). `restore`: make the kit
    /// that was active before it the active one again. Returns what that
    /// kit was (`Some(None)`: there wasn't one), or None when nothing closed.
    fn drop_browsed_kit(&mut self, restore: bool) -> Option<Option<PathBuf>> {
        let b = self.browsed_kit.take()?;
        let active = self.kit().path() == Some(b.path.as_path())
            || self.kit().loading.as_deref() == Some(b.path.as_path());
        if active {
            if self.kit().is_dirty() || self.kit().exporting {
                return None;
            }
            if self.showing_kit && self.engine.is_playing() {
                self.engine.stop();
            }
            self.kit_mut().close();
            self.kit_pending = None;
        self.kit_seed = None;
        } else {
            // Switched away from it through the kit list: drop it from there.
            let i = self.parked.iter().position(|k| k.path() == Some(b.path.as_path()))?;
            if self.parked[i].is_dirty() {
                return None;
            }
            self.parked.remove(i);
            return Some(b.prev);
        }
        if restore {
            let i = b
                .prev
                .as_deref()
                .and_then(|prev| self.parked.iter().position(|k| k.path() == Some(prev)))
                .or_else(|| self.parked.len().checked_sub(1));
            if let Some(i) = i {
                self.swap_with_parked(i);
                self.parked.remove(i);
                self.kit_grid = self.kit().grid_mode().or(self.kit_grid);
            }
        }
        Some(b.prev)
    }

    /// Lay the kit out on `n` equal slots, or free it.
    pub(super) fn set_kit_grid(&mut self, n: Option<usize>) {
        if self.kit().grid_mode() == n {
            return;
        }
        if self.showing_kit && self.engine.is_playing() {
            self.engine.stop();
        }
        match self.kit_mut().set_grid(n) {
            Ok(()) => {
                self.kit_grid = n;
                self.status = match n {
                    Some(crate::editor::AUTO_GRID) => format!(
                        "Auto grid — kit on {} slots now; grows to 8 and 16 as K adds slices, shrinks when you cut them",
                        self.kit().grid.unwrap_or(4)
                    ),
                    Some(n) => format!("Kit laid out on {n} equal slots, padded to the longest slice — K fills the next free slot"),
                    None => "Grid off — empty slots at the end trimmed; K adds slices to the end of the kit again".to_owned(),
                };
            }
            Err(e) => self.status = e,
        }
    }

    /// Right-click → Close all kits / Close all but current.
    pub(super) fn close_all_kits(&mut self, keep_current: bool) {
        if self.close_queue.is_some() {
            return;
        }
        let keep = keep_current
            .then(|| slot_key(self.kit()).map(|p| (p, self.showing_kit)))
            .flatten();
        let mut todo: Vec<PathBuf> = std::iter::once(&*self.kit())
            .chain(self.parked.iter())
            .filter_map(slot_key)
            .filter(|p| keep.as_ref().is_none_or(|(k, _)| k != p))
            .collect();
        if todo.is_empty() {
            self.status = "No other kits open".to_owned();
            return;
        }
        // Parked ones first (popped from the end), the active one last, so
        // closing doesn't keep unparking kits that are about to go anyway.
        todo.reverse();
        self.close_queue = Some(CloseQueue {
            todo,
            keep,
            start: self.kit_list().len(),
        });
        self.step_close_queue();
    }

    /// Close queued kits until one needs a question (or is busy), then wait
    /// for the answer. Cancel there just keeps that kit; the rest go on.
    pub(super) fn step_close_queue(&mut self) {
        if self.close_queue.is_none() {
            return;
        }
        if self.leave_pending.is_some() || self.leave_after_save.is_some() || self.close_kit_now {
            return;
        }
        loop {
            if self.kit_busy().is_some() {
                return;
            }
            let Some(p) = self.close_queue.as_mut().and_then(|q| q.todo.pop()) else {
                break;
            };
            if slot_key(self.kit()).as_deref() == Some(p.as_path()) {
                self.close_kit();
            } else if let Some(i) = self.parked.iter().position(|k| slot_key(k).as_deref() == Some(p.as_path())) {
                let k = &self.parked[i];
                if k.is_dirty() && k.sounding_slices() > 1 {
                    // Bring it in so the question shows it.
                    self.switch_kit(i);
                    self.close_kit();
                } else {
                    self.parked.remove(i);
                }
            }
            if self.leave_pending.is_some() {
                return;
            }
        }
        let q = self.close_queue.take().expect("checked above");
        if let Some((k, showing)) = &q.keep {
            self.switch_to_open(k);
            if *showing {
                self.show_kit(true);
            }
        }
        if let Some(b) = &self.browsed_kit {
            let open = std::iter::once(&*self.kit())
                .chain(self.parked.iter())
                .any(|k| slot_key(k).as_deref() == Some(b.path.as_path()));
            if !open {
                self.browsed_kit = None;
            }
        }
        let left = self.kit_list().len();
        let closed = q.start.saturating_sub(left);
        let kept = left - usize::from(q.keep.is_some() && left > 0);
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        self.status = match kept {
            0 => format!("Closed {closed} kit{}", plural(closed)),
            _ => format!(
                "Closed {closed} kit{}, kept {kept} with unsaved slices",
                plural(closed)
            ),
        };
    }

    pub(super) fn close_kit(&mut self) {
        if self.kit().is_dirty() {
            if self.kit().sounding_slices() > 1 {
                // Worth keeping: ask, with the kit on screen.
                if self.leave_pending.is_none() && self.leave_after_save.is_none() {
                    self.show_kit(true);
                    self.leave_pending = Some(Leave::CloseKit);
                }
                return;
            }
            // One slice or none: not worth a question.
            self.kit_mut().discard();
        }
        let name = self.kit_name().unwrap_or_default();
        self.show_kit(false);
        self.kit_pending = None;
        self.kit_seed = None;
        self.other.close();
        if self.parked.is_empty() {
            self.status = format!("Closed {name}");
        } else {
            self.unpark_first();
            self.status = format!(
                "Closed {name} — K now adds to {}",
                self.kit_name().unwrap_or_default()
            );
        }
    }

    /// Absorb worker results for both slots. The hidden one is swapped in
    /// for the duration, since the handling is written against `editor`.
    pub(super) fn absorb_editors(&mut self) {
        self.absorb_editor(self.showing_kit);
        std::mem::swap(&mut self.editor, &mut self.other);
        self.absorb_editor(!self.showing_kit);
        std::mem::swap(&mut self.editor, &mut self.other);
        // Parked kits only ever load (a restored session); the pending slice
        // belongs to the active kit, so keep it out of their way.
        let pending = self.kit_pending.take();
        let seed = self.kit_seed.take();
        for i in 0..self.parked.len() {
            std::mem::swap(&mut self.editor, &mut self.parked[i]);
            self.absorb_editor(true);
            std::mem::swap(&mut self.editor, &mut self.parked[i]);
        }
        self.kit_pending = pending;
        self.kit_seed = seed;
        for ed in std::iter::once(&mut self.editor)
            .chain(std::iter::once(&mut self.other))
            .chain(self.parked.iter_mut())
        {
            if let Some(n) = ed.notice.take() {
                self.status = n;
            }
        }
    }
}
