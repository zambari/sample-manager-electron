//! Project lifecycle, pool lookup, eligibility and cross-pane pointing.

use super::*;

impl ElekState {
    /// Pool changes can be pushed: every pattern they renumber has arrived,
    /// so none is left pointing at the wrong sample.
    pub fn pool_pushable(&self) -> bool {
        self.project.as_ref().is_some_and(|p| !p.partial && p.sounds_read)
    }

    /// Pool slots, pattern slots and pool sounds changed since the read.
    pub fn changed(&self) -> usize {
        self.pool_changed() + self.proj.changed() + self.lib.sound_changed()
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

    /// The slot whose sample came from (or was fetched to) local file `p`,
    /// unedited since: dropping `p` again would be refused as a duplicate.
    /// Other identical copies of the file are only caught after the drop.
    pub(crate) fn slot_of_local(&self, p: &std::path::Path) -> Option<usize> {
        (1..self.order.len()).find(|&s| {
            self.key_at(s).is_some_and(|k| {
                !self.changed_local.contains_key(&k) && self.linked.get(&k).is_some_and(|(_, l, _)| l == p)
            })
        })
    }

    /// Entry `old` holds nothing and nothing is on its way into it.
    pub(crate) fn free(&self, old: u16) -> bool {
        self.entry(old as usize).is_empty() && !self.filling.contains_key(&old)
    }

    /// The pool can take a file now: a project read from the device (not a
    /// file, not read-only) and no device work in the way.
    pub(crate) fn takes_files(&self) -> bool {
        self.project.as_ref().is_some_and(|p| p.read_only.is_none() && p.from_file.is_none()) && !self.edit_locked()
    }

    /// The one selected slot, empty or not, when the pool can take a file:
    /// `(position now, slot as read)`. Slot 0 is never a target.
    pub(crate) fn lone_sel(&self) -> Option<(usize, u16)> {
        if self.sel.len() != 1 || !self.takes_files() {
            return None;
        }
        let pos = *self.sel.first()?;
        let old = *self.order.get(pos)?;
        (pos > 0 && !self.filling.contains_key(&old)).then_some((pos, old))
    }

    /// The first empty slot in the pool's current order.
    pub(crate) fn first_free(&self) -> Option<(usize, u16)> {
        if !self.takes_files() {
            return None;
        }
        (1..self.order.len()).map(|pos| (pos, self.order[pos])).find(|&(_, old)| self.free(old))
    }

    /// Uses of entry `old` (as read) with the edits so far: an emptied slot
    /// plays nothing, so whatever still points at it doesn't count.
    pub(super) fn uses(&self, old: usize) -> u32 {
        let Some(p) = &self.project else { return 0 };
        if self.free(old as u16) { 0 } else { p.uses[old] }
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
            if let Some(s) = p.pool_sounds() {
                self.lib.pool_sounds = Some(Ok(s));
            }
            self.lib.patterns_changed();
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
        self.lib.project_changed(&p);
        self.order = (0..p.pool.len() as u16).collect();
        self.confirm_clear = None;
        self.fills.clear();
        self.filling.clear();
        let banks = p.pool.len().div_ceil(BANK);
        self.bank = [0, 1.min(banks - 1)];
        self.sel.clear();
        self.cursor = None;
        self.undo.clear();
        self.clip = None;
        self.project = Some(p);
    }

    /// Drops the loaded project, back to the project picker.
    pub(crate) fn close_project(&mut self) {
        self.project = None;
        self.proj = Default::default();
        self.expect_patterns = None;
        self.expect_sounds = None;
        self.keep_blank.clear();
        self.lib.forget();
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
        self.clip = None;
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
    /// The samples in a slot drag under way, with each one's slot label
    /// (`060`, or `A060` with more than one bank).
    pub(crate) fn dragged_slots(&self) -> Option<Vec<((u32, u32), String)>> {
        self.slot_drag.then(|| {
            self.sel
                .iter()
                .filter_map(|&s| Some((self.key_at(s)?, self.label(s))))
                .collect()
        })
    }

    /// The pool can take new files: a project read from the device (not
    /// a file, not read-only) and not locked by a running job.
    pub fn pool_editable(&self) -> bool {
        self.project.as_ref().is_some_and(|p| p.read_only.is_none() && p.from_file.is_none()) && !self.edit_locked()
    }

    /// Slot (as read) holding device file `key`, if any.
    pub fn slot_of(&self, key: (u32, u32)) -> Option<u16> {
        (1..self.order.len()).find(|&s| self.key_at(s) == Some(key)).map(|s| self.order[s])
    }

    /// The sample under the keyboard cursor (the last one clicked).
    pub fn cursor_key(&self) -> Option<(u32, u32)> {
        self.key_at(self.cursor?)
    }

    /// Flash the first slot holding device file `key` and bring it into
    /// view, leaving the selection alone. `false` when no slot holds it.
    pub fn ping_key(&mut self, matches: impl Fn((u32, u32)) -> bool) -> bool {
        let Some(pos) = (1..self.order.len()).find(|&s| self.key_at(s).is_some_and(&matches)) else {
            return false;
        };
        self.ping = Some((pos, std::time::Instant::now()));
        // A pane already on that bank scrolls; otherwise the active one
        // switches to it.
        let panes = if self.docked { 1 } else { 2 };
        let side = (0..panes).find(|&side| self.bank[side] == pos / BANK);
        self.ping_scroll = Some(side.unwrap_or(if self.docked { 0 } else { self.active }));
        true
    }

    /// The label of the first slot holding device file `key` (`A012`),
    /// without touching the view.
    pub fn slot_holding(&self, matches: impl Fn((u32, u32)) -> bool) -> Option<String> {
        (1..self.order.len())
            .find(|&s| self.key_at(s).is_some_and(&matches))
            .map(|s| self.label(s))
    }

    pub(super) fn key_at(&self, slot: usize) -> Option<(u32, u32)> {
        self.project.as_ref()?;
        let e = self.entry(*self.order.get(slot)? as usize);
        (slot > 0 && !e.is_empty()).then(|| e.key())
    }

    pub(super) fn label(&self, slot: usize) -> String {
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
}
