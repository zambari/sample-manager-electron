//! The Elek Sample Order view: the device's sample pool, docked as one pane
//! beside the files, or in two side-by-side lists so a slot can be dragged
//! onto any other (in any bank) to swap them. Also the device bar both
//! device pages share.

use crate::device::{ElekProject, PoolEntry, SavedProject};
use crate::ui::drag_drop::ResponseDnd;
use crate::ui::theme;
use egui::{Color32, RichText, Sense, Stroke};
use std::collections::{BTreeSet, HashMap, HashSet};

const ROW_H: f32 = 20.0;
const TEXT: f32 = 16.0;
const BANK: usize = 128;

pub use theme::oled_font;
use theme::{OLED, OLED_BG, OLED_DIM, OLED_HOVER};

#[derive(Default)]
pub struct ElekState {
    pub projects: Option<Result<Vec<SavedProject>, String>>,
    pub project: Option<Box<ElekProject>>,
    /// `order[slot] = slot it came from` in the project as read.
    pub order: Vec<u16>,
    /// A slot drag is under way (until the button comes up).
    slot_drag: bool,
    /// +Drive paths by `(hash, length)`.
    pub names: HashMap<(u32, u32), String>,
    /// Pool files we have a local copy of (uploaded from it, or fetched to
    /// it), by `(hash, length)`: `(device name when linked, local path,
    /// is the original)`. Filled by the app.
    pub linked: HashMap<(u32, u32), (String, std::path::PathBuf, bool)>,
    /// A row asked for its local copy: `(key, open in editor)`.
    want_local: Option<((u32, u32), bool)>,
    /// A row asked to show its original in the source pane.
    want_original: Option<(u32, u32)>,
    /// Renaming a pool sample's +Drive file: `(key, +Drive path, name typed
    /// so far, focus the field next frame)`.
    renaming: Option<((u32, u32), String, String, bool)>,
    want_files: Option<Vec<(u16, std::path::PathBuf)>>,
    pub bank: [usize; 2],
    /// `(label, done, total)` while the device works.
    pub busy: Option<(String, usize, usize)>,
    pub status: String,
    pub confirm_push: bool,
    /// A push was held back because the device's project changed since the
    /// read: asking whether to push anyway.
    pub confirm_stale: bool,
    /// Asking whether to drop unpushed changes and close the project.
    confirm_close: bool,
    /// Asking whether to clear these still-referenced slots (as read).
    confirm_clear: Option<Vec<u16>>,
    /// After a push: the pool the device should now have.
    pub expect: Option<Vec<PoolEntry>>,
    pub asked_projects: bool,
    /// Selected slots (positions in the new order). Slot 0 is never in it.
    sel: BTreeSet<usize>,
    /// Keyboard cursor / shift-click anchor.
    cursor: Option<usize>,
    /// Pane the keyboard drives.
    active: usize,
    /// Earlier orders and fills, for undo.
    undo: Vec<(Vec<u16>, HashMap<u16, PoolEntry>)>,
    /// New +Drive files put into slots that were empty, by slot as read.
    pub fills: HashMap<u16, PoolEntry>,
    /// Slots (as read) waiting for a dropped file to upload and be found
    /// on the +Drive: the file's name, for the row.
    pub filling: HashMap<u16, String>,
    /// Scroll offset of each pane last frame, and a slot to bring into view.
    scroll: [f32; 2],
    view_h: [f32; 2],
    reveal: Option<usize>,
    /// Clicking a slot (or moving the cursor onto it) plays it.
    pub preview: bool,
    /// Shown as one pane in the right column, beside the files and editor.
    pub docked: bool,
    /// Pass number (+1) when the dialogs were last drawn. The Patterns page
    /// and the docked pool both call `finish`, so this keeps them to once a frame.
    dialogs_pass: u64,
    /// The view gets the keyboard this frame (set by the app).
    pub keys_on: bool,
    /// Which full-window page is shown.
    pub page: ElekPage,
    /// The full-window pages hide the file panes (remembered by the app).
    pub hide_files: bool,
    /// The project editor's layout and selection.
    pub proj: crate::ui::elek_project_view::ProjEd,
    /// After a push with pattern edits: every pattern payload, by slot, the
    /// device should now hold.
    /// (`None`: a slot that wasn't loaded.)
    pub expect_patterns: Option<Vec<Option<Vec<u8>>>>,
    /// While a read runs: which banks to read first, and its stop flag.
    pub reading: Option<std::sync::Arc<crate::device::ReadCtl>>,
    /// The running read's pool has arrived and is what's shown.
    pub read_shown: bool,
    /// Pool files queued or on their way into the preview cache.
    pub fetching: HashSet<(u32, u32)>,
    /// Background fetch run: `(done, total)`.
    pub fetch: Option<(usize, usize)>,
    /// Preview cache on disk: `(bytes, files)`, once measured.
    pub cache: Option<(u64, u64)>,
    /// Every pool edit goes to the device straight away, every pattern
    /// renumbered as usual (overwriting changes made on the device). Only
    /// with the whole project read.
    pub live: bool,
    /// When the pool was last edited, so live pushes wait for a pause.
    pub edited_at: Option<std::time::Instant>,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum ElekPage {
    #[default]
    Order,
    Project,
}

pub enum ElekIntent {
    BackToEditor,
    ListProjects,
    Read,
    /// `force`: send even if the device's project changed since the read.
    Push { force: bool },
    /// Play the local copy of this pool file (`false`), or open it in the
    /// editor (`true`). Fetched into the preview cache first when there is
    /// no local copy.
    Local((u32, u32), bool),
    /// Select the local file this pool sample was uploaded from in the
    /// source pane.
    ShowOriginal((u32, u32)),
    /// Rename the +Drive file behind a pool sample. Projects refer to it by
    /// hash, so nothing that uses it breaks.
    RenameFile { key: (u32, u32), from: String, to: String },
    /// Move into the right column, next to the file panes.
    Dock,
    /// From the docked pool: open the project editor.
    OpenPatterns,
    /// Back to the full-window view.
    Undock,
    /// Fetch every pool file without a local copy, in the background.
    FetchAll,
    StopFetch,
    /// Move the preview cache to the Trash.
    PurgeCache,
    /// Local files dropped on the pool: each goes to the empty slot paired
    /// with it (numbered as read).
    AddFiles(Vec<(u16, std::path::PathBuf)>),
    /// Write the dump as read to disk, for format research.
    SaveDump,
    /// Save the project as edited to a file / open one (asks where).
    /// `true`: a bundle, the samples' WAVs saved alongside.
    SaveProject(bool),
    LoadProject,
    /// Save the pattern in this slot (with its shown name; bundle as for
    /// `SaveProject`) / load one into it.
    SavePattern(usize, String, bool),
    LoadPattern(usize),
}

impl ElekState {
    /// Pool changes can be pushed: every pattern they renumber has arrived,
    /// so none is left pointing at the wrong sample.
    pub fn pool_pushable(&self) -> bool {
        self.project.as_ref().is_some_and(|p| !p.partial)
    }

    /// Pool slots plus pattern slots changed since the read.
    pub fn changed(&self) -> usize {
        self.pool_changed() + self.proj.changed()
    }

    pub fn pool_changed(&self) -> usize {
        self.order
            .iter()
            .enumerate()
            .filter(|(i, o)| *i != **o as usize && !self.fills.contains_key(o))
            .count()
            + self.fills.len()
    }

    /// Pool entry `old` (numbered as read), with any new file put there.
    pub fn entry(&self, old: usize) -> PoolEntry {
        let Some(p) = &self.project else { return PoolEntry::default() };
        self.fills.get(&(old as u16)).copied().unwrap_or(p.pool[old])
    }

    /// Entry `old` holds nothing and nothing is on its way into it.
    pub(crate) fn free(&self, old: u16) -> bool {
        self.entry(old as usize).is_empty() && !self.filling.contains_key(&old)
    }

    /// The one selected slot, when it's empty and the pool can take a file:
    /// `(position now, slot as read)`.
    pub(crate) fn lone_empty_sel(&self) -> Option<(usize, u16)> {
        let p = self.project.as_ref()?;
        if self.sel.len() != 1 || p.read_only.is_some() || p.from_file.is_some() || self.edit_locked() {
            return None;
        }
        let pos = *self.sel.first()?;
        let old = *self.order.get(pos)?;
        (pos > 0 && self.free(old)).then_some((pos, old))
    }

    /// Uses of entry `old` (as read) with the edits so far: an emptied slot
    /// plays nothing, so whatever still points at it doesn't count.
    fn uses(&self, old: usize) -> u32 {
        let Some(p) = &self.project else { return 0 };
        if self.free(old as u16) { 0 } else { p.uses[old] }
    }

    /// Put a new +Drive file into empty entry `old`, as an undoable edit.
    pub fn fill(&mut self, old: u16, e: PoolEntry) {
        self.filling.remove(&old);
        self.undo.push((self.order.clone(), self.fills.clone()));
        self.fills.insert(old, e);
        self.edited_at = Some(std::time::Instant::now());
    }

    /// The read landing on the project shown while it streamed in (pool
    /// early, patterns one by one): keep the edits made meanwhile.
    pub fn finish_read(&mut self, p: Box<ElekProject>) {
        if self.project.as_ref().is_some_and(|old| old.partial && old.pool == p.pool) && self.order.len() == p.pool.len() {
            // Edits made while it streamed in, on patterns the full read
            // can't vouch for: dropped.
            let bad = p.patterns.as_deref().map_or(true, |pats| pats.read_only.is_some());
            if bad && self.proj.changed() > 0 {
                self.proj.reset(&p);
                self.status = "Pattern edits made during the read were dropped: the patterns turned out read-only".into();
            } else {
                self.proj.refresh(&p);
            }
            self.project = Some(p);
            return;
        }
        self.set_project(p);
    }

    /// Edits are off: the device is busy with something other than a read
    /// streaming in.
    /// Until a re-read's pool arrives, the old project is still shown and
    /// edits to it would be dropped, so they're off then too.
    pub fn edit_locked(&self) -> bool {
        self.busy.is_some() && (self.reading.is_none() || !self.read_shown)
    }

    pub fn set_project(&mut self, p: Box<ElekProject>) {
        self.proj.reset(&p);
        self.order = (0..p.pool.len() as u16).collect();
        self.confirm_clear = None;
        self.fills.clear();
        self.filling.clear();
        let banks = p.pool.len().div_ceil(BANK);
        self.bank = [0, 1.min(banks - 1)];
        self.sel.clear();
        self.cursor = None;
        self.undo.clear();
        self.project = Some(p);
    }

    /// Drops the loaded project, back to the project picker.
    pub(crate) fn close_project(&mut self) {
        self.project = None;
        self.proj = Default::default();
        self.expect_patterns = None;
        if let Some(ctl) = &self.reading {
            ctl.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.order.clear();
        self.confirm_clear = None;
        self.fills.clear();
        self.filling.clear();
        self.sel.clear();
        self.cursor = None;
        self.undo.clear();
        self.status.clear();
    }

    /// The pool as it will be after a push.
    pub fn new_pool(&self) -> Vec<PoolEntry> {
        let Some(p) = &self.project else {
            return Vec::new();
        };
        let _ = p;
        self.order.iter().map(|&o| self.entry(o as usize)).collect()
    }

    /// The file in `slot` (as ordered now), unless it's empty or "off".
    /// While slots are being dragged: the samples in the selection, by
    /// `(hash, length)`. Empty slots are left out.
    pub(crate) fn dragged_keys(&self) -> Option<Vec<(u32, u32)>> {
        self.slot_drag.then(|| self.sel.iter().filter_map(|&s| self.key_at(s)).collect())
    }

    fn key_at(&self, slot: usize) -> Option<(u32, u32)> {
        self.project.as_ref()?;
        let e = self.entry(*self.order.get(slot)? as usize);
        (slot > 0 && !e.is_empty()).then(|| e.key())
    }

    fn label(&self, slot: usize) -> String {
        let banks = self
            .project
            .as_ref()
            .map_or(1, |p| p.pool.len().div_ceil(BANK));
        if banks > 1 {
            format!("{}{:03}", (b'A' + (slot / BANK) as u8) as char, slot % BANK)
        } else {
            format!("{slot:03}")
        }
    }

    /// Runs an edit of `order`, keeping the selection on the same samples and
    /// recording undo when something changed.
    fn edit(&mut self, f: impl FnOnce(&mut Vec<u16>)) {
        let before = self.order.clone();
        let picked: HashSet<u16> = self.sel.iter().map(|&s| self.order[s]).collect();
        let cur = self.cursor.map(|c| self.order[c]);
        f(&mut self.order);
        if self.order == before {
            return;
        }
        self.undo.push((before, self.fills.clone()));
        self.edited_at = Some(std::time::Instant::now());
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.sel = self
            .order
            .iter()
            .enumerate()
            .filter(|(_, o)| picked.contains(o))
            .map(|(i, _)| i)
            .collect();
        self.cursor = cur.and_then(|c| self.order.iter().position(|&o| o == c));
        self.reveal = self.cursor;
    }

    fn undo(&mut self) {
        if let Some((prev, fills)) = self.undo.pop() {
            self.order = prev;
            self.fills = fills;
            self.status = "Undone".into();
            self.edited_at = Some(std::time::Instant::now());
        }
    }

    /// Moves the selection so it starts right before slot `to` (as numbered
    /// now). Everything in between closes up.
    fn move_sel_to(&mut self, to: usize) {
        let sel: Vec<usize> = self.sel.iter().copied().collect();
        if sel.is_empty() {
            return;
        }
        let n = self.order.len();
        let to = to.clamp(1, n);
        self.edit(|order| {
            let moved: Vec<u16> = sel.iter().map(|&s| order[s]).collect();
            let set: HashSet<usize> = sel.iter().copied().collect();
            let mut rest: Vec<u16> = order
                .iter()
                .enumerate()
                .filter(|(i, _)| !set.contains(i))
                .map(|(_, &o)| o)
                .collect();
            let at = to - sel.iter().filter(|&&s| s < to).count();
            rest.splice(at..at, moved);
            *order = rest;
        });
        if let (Some(&a), Some(&b)) = (self.sel.first(), self.sel.last()) {
            self.status = if a == b {
                format!("Moved to {}", self.label(a))
            } else {
                format!("Moved to {}–{}", self.label(a), self.label(b))
            };
        }
    }

    /// Puts the selection into slots `to, to+1, …` (in selection order). What
    /// was there goes to the slots the selection left, so nothing else moves:
    /// dropping onto empty slots leaves empty slots behind.
    fn place_sel_at(&mut self, to: usize) {
        let sel: Vec<usize> = self.sel.iter().copied().collect();
        let n = self.order.len();
        if sel.is_empty() {
            return;
        }
        let to = to.clamp(1, n - sel.len());
        let targets: Vec<usize> = (to..to + sel.len()).collect();
        let tset: HashSet<usize> = targets.iter().copied().collect();
        let sset: HashSet<usize> = sel.iter().copied().collect();
        let displaced: Vec<usize> = targets
            .iter()
            .copied()
            .filter(|t| !sset.contains(t))
            .collect();
        let freed: Vec<usize> = sel.iter().copied().filter(|s| !tset.contains(s)).collect();
        let occupied = displaced.iter().filter(|&&t| !self.is_empty_at(t)).count();
        self.edit(|order| {
            let old = order.clone();
            for (&s, &t) in sel.iter().zip(&targets) {
                order[t] = old[s];
            }
            for (&d, &f) in displaced.iter().zip(&freed) {
                order[f] = old[d];
            }
        });
        let what = if sel.len() == 1 {
            self.label(to)
        } else {
            format!("{}–{}", self.label(to), self.label(to + sel.len() - 1))
        };
        self.status = if occupied > 0 {
            format!("Placed at {what}; {occupied} sample(s) there swapped into the old slots")
        } else {
            format!("Placed at {what}")
        };
    }

    fn is_empty_at(&self, slot: usize) -> bool {
        self.project
            .as_ref()
            .is_some_and(|_| self.free(self.order[slot]))
    }

    /// Shifts the selection one slot up (`-1`) or down (`+1`), as a block.
    fn nudge(&mut self, dir: i32) {
        let sel: Vec<usize> = self.sel.iter().copied().collect();
        let n = self.order.len();
        let (Some(&lo), Some(&hi)) = (sel.first(), sel.last()) else {
            return;
        };
        if (dir < 0 && lo <= 1) || (dir > 0 && hi + 1 >= n) {
            return;
        }
        self.edit(|order| {
            if dir < 0 {
                for &s in &sel {
                    order.swap(s - 1, s);
                }
            } else {
                for &s in sel.iter().rev() {
                    order.swap(s, s + 1);
                }
            }
        });
    }

    /// Puts `n` empty slots at `at`, pushing `at` and the samples below it
    /// down; each one uses up the next empty slot further down.
    fn insert_empty(&mut self, at: usize, n: usize) {
        if self.project.is_none() || n == 0 {
            return;
        }
        let mut order = self.order.clone();
        let (mut made, mut last) = (0, at);
        while made < n {
            // Past the empties made so far (the first one may sit on `at`).
            let from = at + made.max(1);
            let Some(e) = (from..order.len()).find(|&i| self.free(order[i])) else { break };
            order[at..=e].rotate_right(1);
            made += 1;
            last = last.max(e);
        }
        if made == 0 {
            self.status = "No empty slot below to make room".into();
            return;
        }
        self.edit(|o| *o = order);
        let short = if made < n { format!(" (only {made} of {n}: no more empty slots below)") } else { String::new() };
        self.status = if made == 1 {
            format!("Inserted empty at {}; shifted down to {}{short}", self.label(at), self.label(last))
        } else {
            format!("Inserted {made} empty at {}–{}; shifted down to {}{short}", self.label(at), self.label(at + made - 1), self.label(last))
        };
    }

    /// Removes the empty slot at `at`, pulling the samples below it up as far
    /// as the next empty slot.
    fn close_gap(&mut self, at: usize) {
        if self.project.is_none() {
            return;
        }
        let n = self.order.len();
        let e = (at + 1..n)
            .find(|&i| self.free(self.order[i]))
            .unwrap_or(n);
        self.edit(|order| order[at..e].rotate_left(1));
        self.status = format!("Closed gap at {}", self.label(at));
    }

    /// Empties `slots` (as ordered now): the unused ones straight away, then
    /// asks before emptying any that kits, locks or sounds still point at.
    fn clear(&mut self, slots: &[usize]) {
        let Some(p) = &self.project else { return };
        if p.partial {
            self.status = "Wait for the read to finish: use counts aren't known yet".into();
            return;
        }
        let olds: Vec<u16> = slots
            .iter()
            .filter(|&&s| s > 0 && s < self.order.len())
            .map(|&s| self.order[s])
            .filter(|&o| !self.free(o))
            .collect();
        let (used, unused): (Vec<u16>, Vec<u16>) = olds.into_iter().partition(|&o| p.uses[o as usize] > 0);
        if !unused.is_empty() {
            self.clear_olds(&unused);
        }
        self.status = match (unused.len(), used.len()) {
            (0, 0) => "Nothing to clear".into(),
            (0, _) => String::new(),
            (n, _) => format!("Cleared {n} unused sample(s)"),
        };
        if !used.is_empty() {
            self.confirm_clear = Some(used);
        }
    }

    /// Empties pool entries `olds` (as read), as one undoable edit.
    fn clear_olds(&mut self, olds: &[u16]) {
        let Some(p) = &self.project else { return };
        let was: Vec<bool> = olds.iter().map(|&o| p.pool[o as usize].is_empty()).collect();
        self.undo.push((self.order.clone(), self.fills.clone()));
        self.edited_at = Some(std::time::Instant::now());
        for (&o, empty_as_read) in olds.iter().zip(was) {
            if empty_as_read {
                // A file added this session: just take it back out.
                self.fills.remove(&o);
            } else {
                self.fills.insert(o, PoolEntry::EMPTY);
            }
        }
    }

    /// Every non-empty slot nothing points at.
    fn unused_slots(&self) -> Vec<usize> {
        let Some(p) = &self.project else { return Vec::new() };
        (1..self.order.len())
            .filter(|&s| {
                let o = self.order[s];
                !self.free(o) && p.uses[o as usize] == 0
            })
            .collect()
    }

    /// Pair dropped files with this slot and the empty ones after it.
    fn drop_files(&mut self, slot: usize, files: Vec<std::path::PathBuf>) {
        let files: Vec<_> = files.into_iter().filter(|f| crate::fs_model::is_audio_path(f)).collect();
        if files.is_empty() {
            self.status = "Only audio files can go into the pool".into();
            return;
        }
        if !self.free(self.order[slot]) {
            self.status = "Drop onto an empty slot (\"-\"); replacing a sample isn't supported".into();
            return;
        }
        let free: Vec<usize> = (slot..self.order.len()).filter(|&s| self.free(self.order[s])).take(files.len()).collect();
        let short = files.len() - free.len();
        let pairs: Vec<(u16, std::path::PathBuf)> = free.iter().map(|&s| self.order[s]).zip(files).collect();
        let reused = pairs.iter().filter(|(o, _)| self.project.as_ref().is_some_and(|p| p.uses[*o as usize] > 0)).count();
        for (o, f) in &pairs {
            let name = f.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            self.filling.insert(*o, name);
        }
        let mut note = format!("Adding {} file(s) from {}", pairs.len(), self.label(slot));
        if short > 0 {
            note += &format!("; {short} left out (no more empty slots below)");
        }
        if reused > 0 {
            note += &format!("; {reused} of those empty slots are still referenced by kits or locks, which will now play the new file");
        }
        self.status = note;
        self.want_files = Some(pairs);
    }

    fn click(&mut self, slot: usize, m: egui::Modifiers) {
        if m.shift {
            let a = self.cursor.unwrap_or(slot);
            if !m.command {
                self.sel.clear();
            }
            self.sel.extend(a.min(slot)..=a.max(slot));
        } else if m.command {
            if !self.sel.remove(&slot) {
                self.sel.insert(slot);
            }
            self.cursor = Some(slot);
        } else {
            self.sel.clear();
            self.sel.insert(slot);
            self.cursor = Some(slot);
        }
        self.sel.remove(&0);
    }

    fn keys(&mut self, ctx: &egui::Context) {
        if !self.keys_on || ctx.egui_wants_keyboard_input() || self.project.is_none() {
            return;
        }
        let n = self.order.len();
        let (m, up, down, pgup, pgdn, esc, z, a, space) = ctx.input(|i| {
            (
                i.modifiers,
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::PageUp),
                i.key_pressed(egui::Key::PageDown),
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::Z),
                i.key_pressed(egui::Key::A),
                i.key_pressed(egui::Key::Space),
            )
        });
        // Space plays (or stops) the slot under the cursor.
        if space && !m.any() {
            if let Some(k) = self.cursor.and_then(|c| self.key_at(c)) {
                self.want_local = Some((k, false));
            }
        }
        if m.command && z && !self.edit_locked() {
            self.undo();
        }
        if m.command && a {
            let b = self.bank[self.active] * BANK;
            self.sel = (b.max(1)..(b + BANK).min(n)).collect();
        }
        if esc {
            self.sel.clear();
        }
        let step: i32 = if up {
            -1
        } else if down {
            1
        } else if pgup {
            -16
        } else if pgdn {
            16
        } else {
            0
        };
        if step == 0 {
            return;
        }
        if m.alt && (up || down) {
            if !self.edit_locked() {
                self.nudge(step);
            }
            return;
        }
        let c = self.cursor.unwrap_or(0) as i32;
        let next = (c + step).clamp(1, n as i32 - 1) as usize;
        if m.shift {
            let a = self.cursor.unwrap_or(next);
            self.sel.insert(a);
            self.sel.insert(next);
        } else {
            self.sel.clear();
            self.sel.insert(next);
        }
        self.cursor = Some(next);
        self.reveal = Some(next);
        if self.preview && !m.shift {
            if let Some(k) = self.key_at(next) {
                self.want_local = Some((k, false));
            }
        }
    }
}

/// A drag of the selected slots.
#[derive(Clone, Copy)]
struct SlotDrag;

enum RowAct {
    Click(usize, egui::Modifiers),
    DragStart(usize),
    DropBefore(usize),
    DropOn(usize),
    /// `(slot, how many)`.
    InsertEmpty(usize, usize),
    CloseGap(usize),
    MoveHere(usize),
    Nudge(i32),
    /// Empty these slots (unused ones at once, asks about the rest).
    Clear(Vec<usize>),
    ClearUnused,
    Local((u32, u32), bool),
    ShowOriginal((u32, u32)),
    Rename((u32, u32)),
    /// Local files dropped on this slot.
    DropFiles(usize, Vec<std::path::PathBuf>),
}

/// The mode switch across the top of both full-window pages, with the
/// layout switches on the right.
pub(crate) fn mode_header(ui: &mut egui::Ui, st: &mut ElekState, out: &mut Vec<ElekIntent>) {
    if ui.add(theme::pane_title("Sample Editor", false)).clicked() {
        out.push(ElekIntent::BackToEditor);
    }
    ui.add_space(14.0);
    theme::device_group(ui, |ui| {
        theme::device_switch(ui, true, true);
        if crate::device::presence().available() || st.project.is_some() {
            if let Some(page) = device_pages(ui, Some(st.page)) {
                st.page = page;
            }
        }
    });
    ui.separator();
    if let Some(p) = &st.project {
        ui.label(RichText::new(format!("{} · {}", p.model.name(), p.versions)).weak().small());
        if let Some(f) = &p.from_file {
            let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            ui.label(RichText::new(format!("from file: {name}")).color(theme::DEVICE_ACCENT_BRIGHT).small())
                .on_hover_text(format!("{}\nNot on the device yet: Push sends the whole project into the device's memory.", f.display()));
        }
    }
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if ui
            .button("⇥ Dock beside files")
            .on_hover_text("Back to the editor, with the pool as one pane in the right column")
            .clicked()
        {
            out.push(ElekIntent::Dock);
        }
        ui.toggle_value(&mut st.hide_files, "Hide file panes")
            .on_hover_text("Give this view the whole window. Off: the file panes stay on the sides and this view takes the editor's place. Remembered.");
    });
}

/// Samples / Patterns: the Device's sub-pages, drawn small right after the
/// Device switch. `active` is the page shown (`None` from the editor, where
/// neither is). Returns the page clicked.
pub(crate) fn device_pages(ui: &mut egui::Ui, active: Option<ElekPage>) -> Option<ElekPage> {
    theme::device_tab_strip(
        ui,
        &[
            (ElekPage::Order, "Samples", "The sample pool in two side-by-side panes"),
            (ElekPage::Project, "Patterns", "Experimental: move, copy, clear and rename patterns (Digitakt II support is newer and less tested)"),
        ],
        active,
    )
}

/// The full-window top: mode tabs, then the device bar with the page's own
/// tools and the status.
pub(crate) fn top(ui: &mut egui::Ui, st: &mut ElekState, out: &mut Vec<ElekIntent>, page_tools: impl FnOnce(&mut egui::Ui, &mut ElekState)) {
    ui.horizontal(|ui| mode_header(ui, st, out));
    ui.horizontal_wrapped(|ui| {
        device_bar(ui, st, out, true);
        if st.project.is_some() {
            ui.separator();
            page_tools(ui, st);
        }
        status_line(ui, st, true);
    });
    if st.project.is_none() {
        return;
    }
    // The page's own view options (the Samples / Patterns switch is up by Device).
    ui.add_space(2.0);
    ui.horizontal_wrapped(|ui| {
        match st.page {
            ElekPage::Order => preview_tools(ui, st, out),
            ElekPage::Project => crate::ui::elek_project_view::view_tools(ui, st),
        }
    });
    ui.separator();
}

pub fn show(ui: &mut egui::Ui, st: &mut ElekState) -> Vec<ElekIntent> {
    let mut out = Vec::new();
    top(ui, st, &mut out, order_tools);
    let busy = st.busy.is_some();
    body(ui, st, &mut out, busy);
    finish(ui, st, &mut out);
    out
}

/// The pool as one pane, for the right column.
pub fn show_docked(ui: &mut egui::Ui, st: &mut ElekState) -> Vec<ElekIntent> {
    let mut out = Vec::new();
    let busy = st.busy.is_some();
    ui.horizontal_wrapped(|ui| {
        if ui.button("⤢").on_hover_text("Open the pool in two side-by-side panes, to drag between banks").clicked() {
            out.push(ElekIntent::Undock);
        }
        if ui.button("Patterns").on_hover_text("Open the Device view on Patterns: move, copy, clear and rename patterns").clicked() {
            out.push(ElekIntent::OpenPatterns);
        }
        ui.separator();
        device_bar(ui, st, &mut out, false);
    });
    if st.project.is_some() {
        ui.horizontal_wrapped(|ui| {
            order_tools(ui, st);
            preview_tools(ui, st, &mut out);
        });
        ui.horizontal(|ui| status_line(ui, st, false));
        if let Some(why) = st.project.as_ref().and_then(|p| p.read_only.clone()).filter(|_| !busy) {
            ui.colored_label(Color32::from_rgb(230, 120, 90), "Read-only").on_hover_text(why);
        }
        st.active = 0;
        st.keys(ui.ctx());
        let size = egui::vec2(ui.available_width(), (ui.available_height() - MEM_BAR_H).max(ROW_H * 4.0));
        ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.set_min_size(size);
            ui.push_id("docked", |ui| slot_pane(ui, st, 0));
        });
        memory_bar(ui, st);
    } else {
        ui.add_space(6.0);
        if let Some((label, done, total)) = &st.busy {
            ui.label(format!("{label}…"));
            ui.add(egui::ProgressBar::new(*done as f32 / (*total).max(1) as f32).text(format!("{done}/{total}")));
        } else {
            ui.label(RichText::new("Reads whatever project is open on the device now. To work on another one, load it on the device first (PROJECT › LOAD).").weak().small());
            if !st.status.is_empty() {
                ui.colored_label(Color32::from_rgb(230, 120, 90), &st.status);
            }
        }
    }
    finish(ui, st, &mut out);
    out
}

/// Height kept free under the slot panes for `memory_bar`.
const MEM_BAR_H: f32 = 26.0;

/// How much of the device's sample memory the pool fills, as it will be
/// after a push (each slot's file size; uploads still on their way not
/// counted).
fn memory_bar(ui: &mut egui::Ui, st: &ElekState) {
    let Some(p) = &st.project else { return };
    let total = p.model.sample_memory();
    let bytes = |pool: &mut dyn Iterator<Item = PoolEntry>| -> u64 {
        pool.filter(|e| !e.is_empty()).map(|e| e.size as u64).sum()
    };
    let now = bytes(&mut st.new_pool().into_iter());
    let read = bytes(&mut p.pool.iter().copied());
    let frac = now as f64 / total as f64;
    let fill = if frac > 1.0 {
        Color32::from_rgb(220, 70, 60)
    } else if frac > 0.9 {
        Color32::from_rgb(230, 150, 60)
    } else {
        theme::DEVICE_ACCENT
    };
    let mut text = format!("Sample memory {} / {} ({:.0}%)", mb(now), mb(total), frac * 100.0);
    if now != read {
        let d = now as i64 - read as i64;
        text += &format!("  ·  {}{} since read", if d >= 0 { "+" } else { "−" }, mb(d.unsigned_abs()));
    }
    if frac > 1.0 {
        text += "  ·  over the limit";
    }
    let waiting = st.filling.len();
    let mut tip = format!(
        "Sum of the file sizes in every filled slot, as the pool will be after a push. As read from the device: {}.",
        mb(read)
    );
    if waiting > 0 {
        tip += &format!(" {waiting} sample(s) still uploading aren't counted yet.");
    }
    // Device-font label over a thin square bar.
    ui.add_space(4.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), MEM_BAR_H - 6.0), Sense::hover());
    let painter = ui.painter();
    painter.text(rect.left_top(), egui::Align2::LEFT_TOP, text, oled_font(11.0), if frac > 0.9 { fill } else { OLED_DIM });
    let bar = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - 4.0), rect.right_bottom());
    painter.rect_filled(bar, 0.0, OLED_BG);
    let w = bar.width() * frac.min(1.0) as f32;
    painter.rect_filled(egui::Rect::from_min_size(bar.min, egui::vec2(w, bar.height())), 0.0, fill);
    resp.on_hover_text(tip);
}

/// Bytes as "12.3 MB".
fn mb(b: u64) -> String {
    format!("{:.1} MB", b as f64 / 1_048_576.0)
}

/// Preview toggle, background fetch and the cache size.
fn preview_tools(ui: &mut egui::Ui, st: &mut ElekState, out: &mut Vec<ElekIntent>) {
    ui.toggle_value(&mut st.preview, "🔊 Preview on click")
        .on_hover_text("Clicking a slot, or moving onto it with ↑/↓, plays it. Without a local copy it is fetched from the device first (that slot jumps the fetch queue). Space plays the slot under the cursor either way.");
    match st.fetch {
        Some((done, total)) => {
            ui.add(egui::ProgressBar::new(done as f32 / total.max(1) as f32).desired_width(140.0).text(format!("Fetching {done}/{total}")));
            if ui.small_button("Stop").on_hover_text("Stop fetching; what's already here stays").clicked() {
                out.push(ElekIntent::StopFetch);
            }
        }
        None => {
            if ui
                .button("⬇ Fetch all for preview")
                .on_hover_text("Copy every pool sample without a local copy into the preview cache, one at a time in the background. Other device work and clicked slots go first.")
                .clicked()
            {
                out.push(ElekIntent::FetchAll);
            }
        }
    }
    let (txt, files) = match st.cache {
        Some((b, f)) => (format!("Cache {}", mb(b)), f),
        None => ("Cache …".to_owned(), 0),
    };
    ui.menu_button(txt, |ui| {
        ui.label(format!("{files} sample(s) fetched for previewing, kept by content hash so they play instantly next time, whichever project uses them."));
        if ui
            .add_enabled(files > 0 && st.fetch.is_none(), egui::Button::new("Move cache to Trash"))
            .on_hover_text("Previews fetch again on demand")
            .clicked()
        {
            out.push(ElekIntent::PurgeCache);
            ui.close();
        }
    });
}

/// Load / reload, push, the +Drive project list, dump and close: what every
/// layout shares. `wide`: with words, else icons.
pub(crate) fn device_bar(ui: &mut egui::Ui, st: &mut ElekState, out: &mut Vec<ElekIntent>, wide: bool) {
    let busy = st.busy.is_some();
    if st.project.is_none() {
        // The full-window page shows a big load button of its own.
        if !wide && !busy {
            let b = egui::Button::new(RichText::new("⬇ Load current project").color(theme::DEVICE_ACCENT_BRIGHT));
            if ui.add(b).on_hover_text("Read the project open on the device now (from its memory)").clicked() {
                out.push(ElekIntent::Read);
            }
        }
        projects_menu(ui, st, out, wide);
        if !busy {
            file_menu(ui, st, out, wide);
        }
        return;
    }
    if ui.add_enabled(!busy, egui::Button::new(if wide { "⬇ Reload" } else { "⬇" })).on_hover_text("Read the project open on the device again (from its memory). Unpushed changes here are dropped.").clicked() {
        out.push(ElekIntent::Read);
    }
    let changed = st.changed();
    let waiting = st.filling.len();
    let from_file = st.project.as_ref().is_some_and(|p| p.from_file.is_some());
    // A stopped read: pool changes rewrite every pattern, so they need them all.
    let pool_blocked = !st.pool_pushable() && st.pool_changed() > 0;
    let can_push = !busy && (changed > 0 || from_file) && waiting == 0 && !pool_blocked && st.project.as_ref().is_some_and(|p| p.read_only.is_none());
    let label = if wide { format!("⬆ Push to device ({changed})") } else { format!("⬆ {changed}") };
    let push = ui
        .add_enabled(can_push, egui::Button::new(RichText::new(label).color(if can_push { theme::DEVICE_ACCENT_BRIGHT } else { Color32::GRAY })))
        .on_hover_text(if waiting > 0 {
            format!("Waiting for {waiting} file(s) to reach or be found on the +Drive")
        } else if pool_blocked && !busy {
            "The read was stopped before every pattern arrived, and sample pool changes rewrite every pattern. Reload to push them, or reset the sample order to push just the pattern changes.".to_owned()
        } else if from_file {
            "Write the whole project from the file into the device's memory, replacing the project loaded there (the saved copy on +Drive is not touched)".to_owned()
        } else {
            format!(
                "Write {} pool and {} pattern change(s) into the project loaded on the device (its memory, not the saved copy on +Drive)",
                st.pool_changed(),
                st.proj.changed()
            )
        });
    if push.clicked() {
        st.confirm_push = true;
    }
    projects_menu(ui, st, out, wide);
    file_menu(ui, st, out, wide);
    let close = theme::close_button(None, false);
    if ui.add_enabled(!busy, close).on_hover_text("Close this project here (the device keeps it)").clicked() {
        if st.changed() > 0 {
            st.confirm_close = true;
        } else {
            st.close_project();
        }
    }
}

/// Save / load project files, and the research dump.
fn file_menu(ui: &mut egui::Ui, st: &mut ElekState, out: &mut Vec<ElekIntent>, wide: bool) {
    let busy = st.busy.is_some();
    let can_save = !busy && st.project.as_ref().is_some_and(|p| !p.partial);
    ui.menu_button(if wide { "💾 File" } else { "💾" }, |ui| {
        if ui
            .add_enabled(can_save, egui::Button::new("Save project…"))
            .on_hover_text("The project as you see it here, unpushed changes included, as a JSON file. Samples are noted by their +Drive hash, not included.")
            .clicked()
        {
            out.push(ElekIntent::SaveProject(false));
            ui.close();
        }
        if ui
            .add_enabled(can_save, egui::Button::new("Save project with samples…"))
            .on_hover_text("A folder: the project file plus a samples/ folder with every pool sample's WAV, from local copies or fetched from the device. Loading it elsewhere uploads only the samples that device lacks (to /imported/<name>).")
            .clicked()
        {
            out.push(ElekIntent::SaveProject(true));
            ui.close();
        }
        if ui
            .add_enabled(!busy, egui::Button::new("Load project…"))
            .on_hover_text("Open a saved project file (or the .json inside a bundle folder) here. Its samples are checked on the device; bundled ones it lacks are uploaded. Nothing else goes to the device until you push.")
            .clicked()
        {
            out.push(ElekIntent::LoadProject);
            ui.close();
        }
        ui.separator();
        let can_dump = can_save && st.project.as_ref().is_some_and(|p| p.from_file.is_none());
        if ui
            .add_enabled(can_dump, egui::Button::new("Save debug dump"))
            .on_hover_text("The project as read (raw .syx, decoded structs, names and strings) to ~/Documents/sample_manager/project_dumps, for format research")
            .clicked()
        {
            out.push(ElekIntent::SaveDump);
            ui.close();
        }
    });
}

/// The projects saved on the +Drive, as a dropdown. They can't be opened
/// from here: they're listed so you know what to load on the device.
fn projects_menu(ui: &mut egui::Ui, st: &mut ElekState, out: &mut Vec<ElekIntent>, wide: bool) {
    let n = match &st.projects {
        Some(Ok(v)) => format!(" ({})", v.len()),
        _ => String::new(),
    };
    let text = if wide { format!("+Drive projects{n}") } else { format!("+Drive{n}") };
    egui::ComboBox::from_id_salt("elek_drive_projects")
        .selected_text(text)
        .height(420.0)
        .width(if wide { 180.0 } else { 90.0 })
        .show_ui(ui, |ui| {
            ui.set_min_width(260.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Saved on the +Drive").strong());
                if ui.small_button("⟳").on_hover_text("List again").clicked() {
                    out.push(ElekIntent::ListProjects);
                }
            });
            ui.label(RichText::new("To work on one: load it on the device (PROJECT › LOAD), then Load / Reload here.").weak().small());
            ui.separator();
            match &st.projects {
                None => {
                    ui.spinner();
                }
                Some(Err(e)) => {
                    ui.colored_label(Color32::from_rgb(230, 120, 90), e);
                }
                Some(Ok(v)) if v.is_empty() => {
                    ui.label(RichText::new("No saved projects").font(oled_font(12.0)).color(OLED_DIM));
                }
                Some(Ok(v)) => {
                    for p in v {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{:03}", p.slot)).font(oled_font(13.0)).color(OLED_DIM));
                            ui.label(RichText::new(&p.name).font(oled_font(13.0)).color(OLED));
                            ui.label(RichText::new(format!("{} KB", p.size / 1024)).font(oled_font(10.0)).color(OLED_DIM));
                        });
                    }
                }
            }
        });
}

/// The progress bar while the device works, else the last status.
pub(crate) fn status_line(ui: &mut egui::Ui, st: &ElekState, wide: bool) {
    if let Some((label, done, total)) = &st.busy {
        // Leave room for Stop: a bar that fills the row pushes the button
        // past the edge, and a side panel then grows to fit it.
        let stop_w = if st.reading.is_some() { 80.0 } else { 0.0 };
        let room = ui.available_width() - stop_w;
        let w = if wide { room.clamp(160.0, 420.0) } else { room.max(60.0) };
        ui.add(egui::ProgressBar::new(*done as f32 / (*total).max(1) as f32).desired_width(w).text(format!("{label} {done}/{total}")));
        stop_read(ui, st);
    } else if !st.status.is_empty() && st.project.is_some() {
        ui.add(egui::Label::new(RichText::new(&st.status).weak()).truncate()).on_hover_text(&st.status);
    }
}

/// Undo / reset for the sample order.
fn order_tools(ui: &mut egui::Ui, st: &mut ElekState) {
    let busy = st.edit_locked();
    let changed = st.pool_changed();
    let wide = !st.docked;
    if ui.add_enabled(!busy && !st.undo.is_empty(), egui::Button::new(if wide { "↶ Undo" } else { "↶" })).on_hover_text("Undo the last move (⌘Z)").clicked() {
        st.undo();
    }
    if ui.add_enabled(!busy && changed > 0, egui::Button::new(if wide { "↺ Reset" } else { "↺" })).on_hover_text("Back to the order as read from the device").clicked() {
        st.undo.push((st.order.clone(), st.fills.clone()));
        st.order = (0..st.order.len() as u16).collect();
        st.fills.clear();
        st.edited_at = Some(std::time::Instant::now());
    }
    // The whole project, read from the device: every pattern can follow
    // its samples, so a live push never leaves one on the wrong sample.
    let can_live = st.project.as_ref().is_some_and(|p| p.read_only.is_none() && p.from_file.is_none() && !p.partial);
    let was = st.live;
    ui.add_enabled_ui(can_live || st.live, |ui| {
        ui.toggle_value(&mut st.live, if wide { "⚡ Live" } else { "⚡" })
            .on_hover_text(
                "Push every sample order change straight away, without asking. Every pattern that uses a moved \
                 sample is renumbered to follow it, as with a normal push, so nothing plays the wrong sample. \
                 Changes made on the device since the last read are overwritten. Each push is read back in full \
                 before the next one goes.",
            )
            .on_disabled_hover_text("Needs the whole project read from the device (let the read finish, or reload)");
    });
    if st.live && !was {
        st.status = "Live: sample order changes push right away; every pattern follows its samples".into();
    }
}

fn body(ui: &mut egui::Ui, st: &mut ElekState, out: &mut Vec<ElekIntent>, busy: bool) {
    if st.project.as_ref().is_some_and(|p| p.partial) && busy {
        ui.label(RichText::new("Sample pool loaded; use counts fill in as the patterns arrive. You can start arranging, pushing unlocks when the read finishes.").weak());
    } else if st.project.as_ref().is_some_and(|p| p.partial) {
        ui.colored_label(
            Color32::from_rgb(230, 120, 90),
            "The read was stopped: use counts are incomplete and sample order changes can't be pushed. Reload to read the whole project.",
        );
    } else if let Some(why) = st.project.as_ref().and_then(|p| p.read_only.clone()) {
        ui.colored_label(Color32::from_rgb(230, 120, 90), format!("Read-only: {why}"))
            .on_hover_text(
                "Pushing is disabled because the dump doesn't match the layouts we know.",
            );
    }

    if st.project.is_none() {
        load_screen(ui, st, out);
    } else {
        st.keys(ui.ctx());
        ui.label(
            RichText::new("Click / ⇧click / ⌘click to select · drag to place into slots (swaps with what's there; ⌃/⌥ while dropping inserts and shifts instead) · drag onto a bank tab or the other pane · ⌥↑↓ nudge · right-click for more")
                .weak()
                .small(),
        );
        let w = (ui.available_width() - 8.0) / 2.0;
        let h = (ui.available_height() - MEM_BAR_H).max(ROW_H * 4.0);
        ui.horizontal_top(|ui| {
            for side in 0..2 {
                ui.allocate_ui_with_layout(
                    egui::vec2(w, h),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_min_size(egui::vec2(w, h));
                        ui.push_id(side, |ui| slot_pane(ui, st, side));
                    },
                );
            }
        });
        memory_bar(ui, st);
    }
}

/// Stop a read under way; the patterns that arrived stay.
fn stop_read(ui: &mut egui::Ui, st: &ElekState) {
    let Some(ctl) = &st.reading else { return };
    let stopping = ctl.stop.load(std::sync::atomic::Ordering::Relaxed);
    if ui
        .add_enabled(!stopping, egui::Button::new(if stopping { "Stopping…" } else { "Stop" }).small())
        .on_hover_text("Stop reading after the pattern on its way. Banks that loaded completely can be edited and their pattern changes pushed; sample order changes need a full read.")
        .clicked()
    {
        ctl.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// No project loaded: one big load button.
pub(crate) fn load_screen(ui: &mut egui::Ui, st: &mut ElekState, out: &mut Vec<ElekIntent>) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        if let Some((label, done, total)) = &st.busy {
            ui.label(RichText::new(format!("{label}…")).font(oled_font(14.0)).color(OLED));
            ui.add(egui::ProgressBar::new(*done as f32 / (*total).max(1) as f32).desired_width(260.0).text(format!("{done}/{total}")));
            stop_read(ui, st);
        } else {
            let big = egui::Button::new(RichText::new("⬇  Load current project from device memory").size(15.0).color(theme::DEVICE_ACCENT_BRIGHT))
                .min_size(egui::vec2(320.0, 36.0));
            if ui.add(big).clicked() {
                out.push(ElekIntent::Read);
            }
            ui.add_space(6.0);
            ui.label(RichText::new("Reads whatever project is open on the device now.\nTo work on another one, load it on the device first (PROJECT › LOAD).").weak());
            ui.add_space(10.0);
            if ui.button("📂  Open a project file…").on_hover_text("A project saved from here earlier. Nothing goes to the device until you push.").clicked() {
                out.push(ElekIntent::LoadProject);
            }
            if !st.status.is_empty() {
                ui.add_space(6.0);
                ui.colored_label(Color32::from_rgb(230, 120, 90), &st.status);
            }
        }
    });
}

/// Requests and confirmations common to both layouts.
pub(crate) fn finish(ui: &mut egui::Ui, st: &mut ElekState, out: &mut Vec<ElekIntent>) {
    if let Some((k, open)) = st.want_local.take() {
        out.push(ElekIntent::Local(k, open));
    }
    if let Some(k) = st.want_original.take() {
        out.push(ElekIntent::ShowOriginal(k));
    }
    if let Some(pairs) = st.want_files.take() {
        out.push(ElekIntent::AddFiles(pairs));
    }
    let pass = ui.ctx().cumulative_pass_nr() + 1;
    if st.dialogs_pass == pass {
        return;
    }
    st.dialogs_pass = pass;
    if let Some(r) = st.renaming.as_mut() {
        if let Some(ok) = rename_dialog(ui.ctx(), r) {
            let (key, from, to, _) = st.renaming.take().unwrap_or_default();
            if ok {
                out.push(ElekIntent::RenameFile { key, from, to });
            }
        }
    }
    if st.confirm_close {
        if let Some(yes) = confirm_close(ui.ctx(), st.changed()) {
            st.confirm_close = false;
            if yes {
                st.close_project();
            }
        }
    }
    if let Some(olds) = &st.confirm_clear {
        if let Some(yes) = confirm_clear(ui.ctx(), st, olds) {
            let olds = st.confirm_clear.take().unwrap_or_default();
            if yes {
                st.clear_olds(&olds);
                st.status = format!("Cleared {} used sample(s); what pointed at them now plays nothing", olds.len());
            } else {
                st.status = format!("Kept {} used sample(s)", olds.len());
            }
        }
    }
    if st.confirm_push {
        let whole = st.project.as_ref().is_some_and(|p| p.from_file.is_some());
        if let Some(yes) = confirm_push(ui.ctx(), st.pool_changed(), st.proj.changed(), whole) {
            st.confirm_push = false;
            if yes {
                out.push(ElekIntent::Push { force: false });
            }
        }
    }
    if st.confirm_stale {
        if let Some(a) = confirm_stale(ui.ctx()) {
            st.confirm_stale = false;
            match a {
                Stale::PushAnyway => out.push(ElekIntent::Push { force: true }),
                Stale::Read => out.push(ElekIntent::Read),
                Stale::Cancel => {}
            }
        }
    }
}

enum Stale {
    PushAnyway,
    Read,
    Cancel,
}

fn confirm_stale(ctx: &egui::Context) -> Option<Stale> {
    let mut answer = None;
    egui::Modal::new(egui::Id::unique("elek_stale_modal")).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.heading("The project on the device changed");
        ui.add_space(6.0);
        ui.label(
            "Something changed on the device since the project was read. Even turning a knob counts. \
             Nothing was sent.",
        );
        ui.add_space(4.0);
        ui.label(RichText::new(
            "Push anyway sends the changes as planned: any pattern, kit or pool data it sends replaces \
             what the device has there now, including edits made on the device since the read. \
             Read again first to keep those edits (your unpushed changes here will be lost).",
        ).weak());
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(RichText::new("Push anyway").color(Color32::from_rgb(230, 120, 90))).clicked() {
                answer = Some(Stale::PushAnyway);
            }
            if ui.button("Read again").clicked() {
                answer = Some(Stale::Read);
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                answer = Some(Stale::Cancel);
            }
        });
    });
    answer
}

fn slot_pane(ui: &mut egui::Ui, st: &mut ElekState, side: usize) {
    // A slot drag ends with the button release (handled the frame it
    // happens) or Escape.
    if ui.input(|i| (!i.pointer.any_down() && !i.pointer.any_released()) || i.key_pressed(egui::Key::Escape)) {
        st.slot_drag = false;
    }
    let Some(p) = &st.project else { return };
    let n = p.pool.len();
    let banks = n.div_ceil(BANK);
    // Keep the keyboard cursor visible in the pane it drives.
    if let Some(r) = st.reveal.filter(|_| side == st.active) {
        st.bank[side] = r / BANK;
    }
    if banks > 1 {
        // Device-style bank strip: "< A B C D E F G H >", current one boxed.
        let (strip, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), ROW_H + 6.0),
            Sense::empty(),
        );
        ui.painter().rect_filled(strip, 0.0, OLED_BG);
        // Narrow cells in a narrow pane so the strip never spills past the
        // left edge (it is centred, so overflow would be clipped there).
        let cells = banks as f32 + 2.0;
        let cell = (strip.width() / cells).clamp(12.0, 26.0);
        let x0 = (strip.center().x - cell * cells / 2.0).max(strip.left());
        let font = oled_font(TEXT);
        let arrow = |ui: &mut egui::Ui, i: f32, txt: &str, to: Option<usize>| {
            let r = egui::Rect::from_min_size(
                egui::pos2(x0 + i * cell, strip.top() + 3.0),
                egui::vec2(cell, ROW_H),
            );
            let resp = ui.interact(r, ui.scope_id().with(("bank_tab", txt)), Sense::click());
            let on = to.is_some();
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                txt,
                font.clone(),
                if on && resp.hovered() { OLED } else { OLED_DIM },
            );
            (resp, to)
        };
        let mut pick = None;
        let cur = st.bank[side];
        let (r, to) = arrow(ui, 0.0, "<", cur.checked_sub(1));
        if r.clicked() {
            pick = to;
        }
        for b in 0..banks {
            let r = egui::Rect::from_min_size(
                egui::pos2(x0 + (b + 1) as f32 * cell, strip.top() + 3.0),
                egui::vec2(cell, ROW_H),
            );
            let resp = ui.interact(r, ui.scope_id().with(("bank", b)), Sense::click());
            let letter = ((b'A' + b as u8) as char).to_string();
            let p = ui.painter();
            if b == cur {
                p.rect_stroke(
                    r.shrink2(egui::vec2(4.0, 0.0)),
                    0.0,
                    Stroke::new(2.0, OLED),
                    egui::StrokeKind::Inside,
                );
            } else if resp.hovered() {
                p.rect_filled(
                    r.shrink2(egui::vec2(4.0, 0.0)),
                    0.0,
                    OLED_HOVER,
                );
            }
            p.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                letter,
                font.clone(),
                OLED,
            );
            // Dragging slots over a bank tab opens that bank.
            if resp.clicked() || resp.dnd_hover_payload::<SlotDrag>().is_some() {
                pick = Some(b);
            }
        }
        let (r, to) = arrow(
            ui,
            banks as f32 + 1.0,
            ">",
            Some(cur + 1).filter(|&b| b < banks),
        );
        if r.clicked() {
            pick = to;
        }
        if let Some(b) = pick {
            st.bank[side] = b;
            st.active = side;
        }
    }
    let bank = st.bank[side].min(banks - 1);
    let first = bank * BANK;
    let rows = BANK.min(n - first);
    let mut acts = Vec::new();
    let mut scroll = egui::ScrollArea::vertical()
        .id_salt(("elek_slots", side, bank))
        .auto_shrink([false, false]);
    if let Some(r) = st.reveal.filter(|_| side == st.active) {
        let y = (r - first) as f32 * ROW_H;
        let (top, h) = (st.scroll[side], st.view_h[side]);
        if y < top {
            scroll = scroll.vertical_scroll_offset(y);
        } else if y + ROW_H > top + h {
            scroll = scroll.vertical_scroll_offset(y + ROW_H - h);
        }
    }
    let out = egui::Frame::new()
        .fill(OLED_BG)
        .corner_radius(2.0)
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            // show_rows sizes the list with the spacing set *before* it is
            // called, so zero it here, not inside, or the list gets a blank
            // tail of rows × spacing.
            ui.spacing_mut().item_spacing.y = 0.0;
            theme::always_show_scrollbar(ui);
            scroll.show_rows(ui, ROW_H, rows, |ui, range| {
                for i in range {
                    slot_row(ui, st, first + i, &mut acts);
                }
            })
        });
    st.scroll[side] = out.inner.state.clamped_offset().y;
    st.view_h[side] = out.inner.inner_rect.height();
    if side == st.active {
        st.reveal = None;
    }
    let locked = st.edit_locked();
    for a in acts {
        let edits = !matches!(a, RowAct::Click(..) | RowAct::DragStart(_) | RowAct::Local(..) | RowAct::ShowOriginal(_) | RowAct::Rename(_));
        if locked && edits {
            st.status = "Wait a moment: the device is busy, and an edit now would be lost".into();
            continue;
        }
        match a {
            RowAct::Click(s, m) => {
                st.active = side;
                st.click(s, m);
                if st.preview && !m.shift && !m.command {
                    if let Some(k) = st.key_at(s) {
                        st.want_local = Some((k, false));
                    }
                }
            }
            RowAct::DragStart(s) => {
                st.slot_drag = true;
                st.active = side;
                if !st.sel.contains(&s) {
                    st.click(s, egui::Modifiers::NONE);
                }
            }
            RowAct::DropOn(s) => {
                st.active = side;
                st.place_sel_at(s);
            }
            RowAct::DropBefore(s) => {
                st.active = side;
                st.move_sel_to(s);
            }
            RowAct::InsertEmpty(s, n) => st.insert_empty(s, n),
            RowAct::CloseGap(s) => st.close_gap(s),
            RowAct::MoveHere(s) => st.move_sel_to(s),
            RowAct::Nudge(d) => st.nudge(d),
            RowAct::Clear(slots) => st.clear(&slots),
            RowAct::ClearUnused => {
                let slots = st.unused_slots();
                st.clear(&slots);
            }
            RowAct::Local(k, open) => st.want_local = Some((k, open)),
            RowAct::ShowOriginal(k) => st.want_original = Some(k),
            RowAct::Rename(k) => {
                if let Some(path) = st.names.get(&k).cloned() {
                    let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
                    st.renaming = Some((k, path, name, true));
                }
            }
            RowAct::DropFiles(s, files) => {
                st.active = side;
                st.drop_files(s, files);
            }
        }
    }
}

/// One slot row, laid out like the device's file list.
fn slot_row(ui: &mut egui::Ui, st: &ElekState, slot: usize, acts: &mut Vec<RowAct>) {
    let Some(p) = st.project.as_ref() else { return };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::empty());
    let from = st.order[slot] as usize;
    let e = st.entry(from);
    let filling = st.filling.get(&(from as u16));
    let added = st.fills.contains_key(&(from as u16)) || filling.is_some();
    let off = slot == 0;
    let resp = ui.interact(
        rect,
        ui.scope_id().with(("slot", slot)),
        if off {
            Sense::hover()
        } else {
            Sense::click_and_drag()
        },
    );
    let selected = st.sel.contains(&slot);
    let moved = from != slot || added;

    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, 0.0, OLED);
    } else if resp.hovered() && !off {
        painter.rect_filled(rect, 0.0, OLED_HOVER);
    }
    if st.cursor == Some(slot) && !selected {
        painter.rect_stroke(
            rect.shrink(0.5),
            0.0,
            Stroke::new(1.0, OLED_DIM),
            egui::StrokeKind::Inside,
        );
    }
    if moved {
        painter.rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
            0.0,
            theme::DEVICE_ACCENT_BRIGHT,
        );
    }
    let (fg, dim) = if selected {
        (OLED_BG, Color32::from_rgb(60, 70, 66))
    } else {
        (OLED, OLED_DIM)
    };
    let font = oled_font(TEXT);
    let y = rect.center().y;
    let label_color = if moved && !selected {
        theme::DEVICE_ACCENT_BRIGHT
    } else {
        dim
    };
    let label_w = 46.0;
    painter.text(
        egui::pos2(rect.left() + 8.0, y),
        egui::Align2::LEFT_CENTER,
        format!("{:03}", slot % BANK),
        font.clone(),
        label_color,
    );
    let name = if off {
        "OFF".to_owned()
    } else if let Some(n) = filling {
        format!("+ {n} …")
    } else if e.is_empty() {
        "-".to_owned()
    } else {
        match (st.names.get(&e.key()), st.linked.get(&e.key())) {
            (Some(path), _) => path.rsplit('/').next().unwrap_or(path).to_owned(),
            // Known from our own transfers while the device lookup runs.
            (None, Some((name, ..))) => name.clone(),
            (None, None) => format!("#{:08x}", e.hash),
        }
    };
    let linked = (!off && !e.is_empty()).then(|| st.linked.get(&e.key())).flatten();
    let uses = st.uses(from);
    let size = if e.is_empty() || off {
        String::new()
    } else {
        format!("{:.2}MB", e.size.saturating_sub(16) as f64 / 1_048_576.0)
    };
    let size_w = 58.0;
    let uses_w = 30.0;
    let name_x = rect.left() + 8.0 + label_w;
    // The sample moved and patterns point at it: the push renumbers them.
    let dot_c = if selected { OLED_BG } else { theme::DEVICE_ACCENT_BRIGHT };
    if !off && from != slot && uses > 0 {
        painter.circle_filled(egui::pos2(name_x - 7.0, y), 2.5, dot_c);
    }
    // Name brightness shows the preview cache: full white plays at once,
    // 0.8 opacity still has to be pulled from the device.
    let name_c = if selected {
        fg
    } else if linked.is_some() {
        Color32::WHITE
    } else {
        fg.gamma_multiply(0.8)
    };
    let clip = egui::Rect::from_min_max(
        egui::pos2(name_x, rect.top()),
        egui::pos2(rect.right() - size_w - uses_w, rect.bottom()),
    );
    painter.with_clip_rect(clip).text(
        egui::pos2(name_x, y),
        egui::Align2::LEFT_CENTER,
        name,
        font.clone(),
        if (e.is_empty() && filling.is_none()) || off { dim } else { name_c },
    );
    if uses > 0 {
        painter.text(
            egui::pos2(rect.right() - size_w - 4.0, y),
            egui::Align2::RIGHT_CENTER,
            uses.to_string(),
            oled_font(TEXT - 4.0),
            dim,
        );
    }
    painter.text(
        egui::pos2(rect.right() - 8.0, y),
        egui::Align2::RIGHT_CENTER,
        size,
        font,
        fg,
    );

    if off {
        return;
    }
    // With preview on, the first click of a double-click already played it.
    if resp.clicked() && !(st.preview && resp.double_clicked()) {
        acts.push(RowAct::Click(slot, ui.input(|i| i.modifiers)));
    }
    if resp.double_clicked() && !st.preview && !e.is_empty() {
        acts.push(RowAct::Local(e.key(), false));
    }
    if resp.drag_started() {
        acts.push(RowAct::DragStart(slot));
        resp.dnd_set_drag_payload(SlotDrag);
    }
    // Drop: by default the selection lands in this slot and the ones after it,
    // swapping with what's there. With ⌃ or ⌥ held it's inserted instead,
    // before this row (top half) or after it (bottom half), shifting the rest.
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let shift_mode = ui.input(|i| i.modifiers.ctrl || i.modifiers.alt);
    let before = |pos: Option<egui::Pos2>| {
        if pos.is_some_and(|p| p.y > rect.center().y) {
            slot + 1
        } else {
            slot
        }
    };
    if resp.dnd_hover_payload::<SlotDrag>().is_some() {
        if shift_mode {
            let at = before(pointer);
            let ly = if at == slot {
                rect.top()
            } else {
                rect.bottom()
            };
            ui.painter()
                .hline(rect.x_range(), ly, Stroke::new(2.0, theme::DEVICE_ACCENT_BRIGHT));
        } else {
            let n = st.sel.len().max(1).min(st.order.len() - slot);
            let r = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), ROW_H * n as f32));
            ui.painter().rect_stroke(
                r,
                0.0,
                Stroke::new(2.0, theme::DEVICE_ACCENT_BRIGHT),
                egui::StrokeKind::Inside,
            );
        }
    }
    // Local files from a file pane (or Finder) onto an empty slot.
    if let Some(d) = resp.dnd_hover_payload::<crate::ui::file_table::DragPayload>().filter(|d| !d.device) {
        let ok = st.free(from as u16);
        let n = if ok { d.paths.len().min(st.order.len() - slot) } else { 1 };
        let r = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), ROW_H * n as f32));
        let c = if ok { theme::DEVICE_ACCENT_BRIGHT } else { Color32::from_rgb(230, 120, 90) };
        ui.painter().rect_stroke(r, 0.0, Stroke::new(2.0, c), egui::StrokeKind::Inside);
    }
    if let Some(d) = resp.release_payload::<crate::ui::file_table::DragPayload>().filter(|d| !d.device) {
        acts.push(RowAct::DropFiles(slot, d.paths.clone()));
    }
    // Our own drag flag too: if anything else cleared egui's payload this
    // frame, the drop still lands.
    let released = resp.contains_pointer() && ui.input(|i| i.pointer.any_released());
    if resp.release_payload::<SlotDrag>().is_some() || (released && st.slot_drag) {
        acts.push(if shift_mode {
            RowAct::DropBefore(before(pointer))
        } else {
            RowAct::DropOn(slot)
        });
    }

    let n_sel = st.sel.len();
    resp.context_menu(|ui| {
        if !e.is_empty() {
            let (play, open, tip) = if linked.is_some() {
                ("Play local copy  (Space)", "Open local copy in editor", "Nothing is fetched from the device")
            } else {
                ("Fetch and play  (Space)", "Fetch and open in editor", "Copied into the preview cache first; other fetches wait")
            };
            if ui.button(play).on_hover_text(tip).clicked() {
                acts.push(RowAct::Local(e.key(), false));
                ui.close();
            }
            if ui.button(open).on_hover_text(tip).clicked() {
                acts.push(RowAct::Local(e.key(), true));
                ui.close();
            }
            // Uploaded from a local file: point at it, so it's clear which
            // of several same-named copies is the one on the device.
            if let Some((_, local, true)) = linked {
                if ui
                    .button("Show original in source pane")
                    .on_hover_text(format!("The file it was uploaded from:\n{}", local.display()))
                    .clicked()
                {
                    acts.push(RowAct::ShowOriginal(e.key()));
                    ui.close();
                }
            }
            // Only once the device told us where the file lives.
            if let Some(path) = st.names.get(&e.key()) {
                if ui
                    .button("Rename on +Drive…")
                    .on_hover_text(format!(
                        "{path}\nProjects find samples by content hash, so every project using it keeps working"
                    ))
                    .clicked()
                {
                    acts.push(RowAct::Rename(e.key()));
                    ui.close();
                }
            }
            ui.separator();
        }
        if ui
            .button("Insert empty slot here")
            .on_hover_text(
                "Shift this and the samples below down by one, up to the next empty slot",
            )
            .clicked()
        {
            acts.push(RowAct::InsertEmpty(slot, 1));
            ui.close();
        }
        // Several selected: make room for as many, above the selection.
        if selected && n_sel > 1 {
            let top = st.sel.first().copied().unwrap_or(slot);
            if ui
                .button(format!("Insert {n_sel} empty slots above selection"))
                .on_hover_text(format!(
                    "Shift {} and everything below down by {n_sel}, using up the next {n_sel} empty slots further down",
                    st.label(top)
                ))
                .clicked()
            {
                acts.push(RowAct::InsertEmpty(top, n_sel));
                ui.close();
            }
        }
        if st.free(from as u16)
            && ui
                .button("Remove this empty slot")
                .on_hover_text("Pull the samples below up by one, up to the next empty slot")
                .clicked()
        {
            acts.push(RowAct::CloseGap(slot));
            ui.close();
        }
        if n_sel > 0 && !(n_sel == 1 && selected) {
            ui.separator();
            if ui
                .button(format!("Move {n_sel} selected here"))
                .on_hover_text("Insert the selection above this slot")
                .clicked()
            {
                acts.push(RowAct::MoveHere(slot));
                ui.close();
            }
        }
        if selected {
            ui.separator();
            if ui.button("Move selection up  (⌥↑)").clicked() {
                acts.push(RowAct::Nudge(-1));
            }
            if ui.button("Move selection down  (⌥↓)").clicked() {
                acts.push(RowAct::Nudge(1));
            }
        }
        // Clearing: the selection when right-clicking inside it, else this row.
        let targets: Vec<usize> = if selected {
            st.sel.iter().copied().filter(|&s| !st.free(st.order[s])).collect()
        } else if !st.free(from as u16) {
            vec![slot]
        } else {
            Vec::new()
        };
        let counted = !p.partial;
        ui.separator();
        if !targets.is_empty() {
            let used = targets.iter().filter(|&&s| p.uses[st.order[s] as usize] > 0).count();
            let label = if targets.len() == 1 {
                "Clear sample from project".to_owned()
            } else {
                format!("Clear {} samples from project", targets.len())
            };
            let tip = if used > 0 {
                format!("Unused ones are cleared right away; {used} still used by kits, locks or sounds will ask first")
            } else {
                "Empties the slot in the pool; the file stays on the +Drive. Undo with ⌘Z".to_owned()
            };
            if ui.add_enabled(counted, egui::Button::new(label)).on_hover_text(tip).on_disabled_hover_text("Wait for the read to finish").clicked() {
                acts.push(RowAct::Clear(targets));
                ui.close();
            }
        }
        if ui
            .add_enabled(counted, egui::Button::new("Clear all unused samples"))
            .on_hover_text("Empties every slot nothing in the project points at; the files stay on the +Drive. Undo with ⌘Z")
            .on_disabled_hover_text("Wait for the read to finish")
            .clicked()
        {
            acts.push(RowAct::ClearUnused);
            ui.close();
        }
    });
    if !e.is_empty() {
        let mut tip = st
            .names
            .get(&e.key())
            .cloned()
            .unwrap_or_else(|| "(not found on +Drive)".into());
        if let Some((_, local, original)) = linked {
            let cached = crate::links::pool_cache_dir().is_some_and(|c| local.starts_with(c));
            tip.push_str(&format!(
                "\n{}: {}",
                if *original {
                    "uploaded from"
                } else if cached {
                    "in the preview cache"
                } else {
                    "fetched to"
                },
                local.display()
            ));
        } else if st.fetching.contains(&e.key()) {
            tip.push_str("\nqueued for the preview cache");
        }
        if st.fills.contains_key(&(from as u16)) {
            tip.push_str("\nnew in this slot: goes to the device with the next push");
        }
        tip.push_str(&format!("\npool slot {slot}"));
        if moved {
            tip.push_str(&format!(", was {}", st.label(from)));
        }
        if from != slot && uses > 0 {
            tip.push_str("\npatterns using it get renumbered on push");
        }
        tip.push_str(&format!(
            "\nused {uses}× (once per pattern track, plus pool sounds)"
        ));
        resp.on_hover_text(tip);
    } else if filling.is_some() {
        resp.on_hover_text("Uploading to the +Drive, then checking the device can find it. Pushing waits for this.");
    }
}

fn confirm_push(ctx: &egui::Context, pool: usize, patterns: usize, whole: bool) -> Option<bool> {
    let mut answer = None;
    egui::Modal::new(egui::Id::unique("elek_push_modal")).show(ctx, |ui| {
        ui.set_width(440.0);
        let what = match (pool, patterns) {
            _ if whole => "the whole project from the file".to_owned(),
            (0, n) => format!("{n} pattern slot(s)"),
            (n, 0) => format!("new sample order, {n} slot(s)"),
            (a, b) => format!("{a} sample slot(s) and {b} pattern slot(s)"),
        };
        ui.heading(format!("Push {what}?"));
        ui.add_space(6.0);
        if whole {
            ui.label(
                "This replaces the project loaded on the device (its memory) with the one from the file: every pattern, \
                 kit, pool sound and the sample pool. Whatever is in the device's memory now is overwritten, saved or not. \
                 The saved copies on +Drive are not touched.",
            );
            ui.add_space(4.0);
            ui.colored_label(
                Color32::from_rgb(230, 120, 90),
                "Samples are not sent: the pool points at +Drive files by their hash. Any the device doesn't have will play nothing. \
                 Experimental: if the device was holding something you want, save it there (PROJECT › SAVE) before pushing.",
            );
        } else {
            ui.label(
                "This rewrites the project currently loaded on the device: only the patterns, kits and pool sounds \
                 that change are sent, plus the sample pool if it changed. Everything else on the device is left as is. \
                 The saved copy on +Drive is not touched.",
            );
        }
        if patterns > 0 && !whole {
            ui.add_space(4.0);
            ui.colored_label(
                Color32::from_rgb(230, 120, 90),
                "Pattern editing is experimental (Digitakt 1). Each changed slot is sent as a whole pattern with its kit, \
                 replacing what the device has there now, including edits made on the device since the read.",
            );
        }
        ui.add_space(4.0);
        ui.label(RichText::new("Wait for the device's own progress bar to finish. If it sounds right, save via PROJECT › SAVE. If anything is off, don't save: reload the project from +Drive.").weak());
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(RichText::new("Push to device").color(theme::DEVICE_ACCENT_BRIGHT)).clicked() {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                answer = Some(false);
            }
        });
    });
    answer
}

fn confirm_clear(ctx: &egui::Context, st: &ElekState, olds: &[u16]) -> Option<bool> {
    let Some(p) = &st.project else { return Some(false) };
    let mut answer = None;
    egui::Modal::new(egui::Id::unique("elek_clear_modal")).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.heading(format!("Clear {} used sample(s)?", olds.len()));
        ui.add_space(6.0);
        if !st.status.is_empty() {
            ui.label(RichText::new(format!("{}.", st.status)).weak());
        }
        ui.label(
            "These samples are still used by kit tracks, sample locks or pool sounds. \
             Clearing them disconnects those sounds: they will play nothing after the push.",
        );
        ui.add_space(4.0);
        egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
            for &o in olds {
                let slot = st.order.iter().position(|&x| x == o).unwrap_or(o as usize);
                let e = st.entry(o as usize);
                let name = st
                    .names
                    .get(&e.key())
                    .map(|n| n.rsplit('/').next().unwrap_or(n).to_owned())
                    .unwrap_or_else(|| format!("#{:08x}", e.hash));
                ui.label(format!("{}  {name}  (used {}×)", st.label(slot), p.uses[o as usize]));
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(RichText::new("Clear anyway").color(Color32::from_rgb(230, 120, 90))).clicked() {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                answer = Some(false);
            }
        });
    });
    answer
}

/// `Some(true)` to rename, `Some(false)` to cancel.
fn rename_dialog(ctx: &egui::Context, r: &mut ((u32, u32), String, String, bool)) -> Option<bool> {
    let (_, path, name, focus) = r;
    let mut answer = None;
    egui::Modal::new(egui::Id::unique("elek_rename_modal")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.heading("Rename on +Drive");
        ui.add_space(4.0);
        ui.label(RichText::new(path.as_str()).weak());
        ui.add_space(6.0);
        let te = ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY));
        if std::mem::take(focus) {
            te.request_focus();
            // Select the stem, leaving the extension alone.
            let stem = name.rfind('.').unwrap_or(name.len());
            let stem = name[..stem].chars().count();
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), te.id) {
                state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(stem),
                )));
                state.store(ui.ctx(), te.id);
            }
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "Only the file's name changes. Projects refer to samples by content hash, \
                 so this one and every other project using the file keep working.",
            )
            .weak(),
        );
        ui.add_space(8.0);
        let enter = te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.horizontal(|ui| {
            let ok = !name.trim().is_empty() && !name.contains('/');
            if ui.add_enabled(ok, egui::Button::new("Rename")).clicked() || (ok && enter) {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                answer = Some(false);
            }
        });
    });
    answer
}

fn confirm_close(ctx: &egui::Context, changed: usize) -> Option<bool> {
    let mut answer = None;
    egui::Modal::new(egui::Id::unique("elek_close_modal")).show(ctx, |ui| {
        ui.set_width(400.0);
        ui.heading("Discard unpushed changes?");
        ui.add_space(6.0);
        ui.label(format!("{changed} slot(s) have been moved but not pushed to the device. Closing the project drops these changes."));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(RichText::new("Discard and close").color(Color32::from_rgb(230, 120, 90))).clicked() {
                answer = Some(true);
            }
            if ui.button("Keep editing").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                answer = Some(false);
            }
        });
    });
    answer
}
