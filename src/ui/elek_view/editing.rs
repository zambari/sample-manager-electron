//! Undoable sample-pool ordering, clearing, clipboard and file-drop edits.

use super::*;

impl ElekState {
    /// Put a new +Drive file into empty entry `old`, as an undoable edit.
    pub fn fill(&mut self, old: u16, e: PoolEntry) {
        self.filling.remove(&old);
        self.undo.push((self.order.clone(), self.fills.clone()));
        self.fills.insert(old, e);
        self.edited_at = Some(std::time::Instant::now());
    }
    /// Cut (`cut`) or copy these slots (as ordered now); empty ones are
    /// left out. Their names also go on the system clipboard as text, which
    /// is what makes ⌘V reach the app at all.
    pub(super) fn set_clip(&mut self, slots: &[usize], cut: bool, ctx: &egui::Context) {
        let slots: Vec<usize> = slots.iter().copied().filter(|&s| self.key_at(s).is_some()).collect();
        if slots.is_empty() {
            self.status = "Nothing to cut or copy: those slots are empty".into();
            return;
        }
        let names: Vec<String> = slots
            .iter()
            .map(|&s| {
                let k = self.key_at(s).unwrap_or_default();
                let name = self.names.get(&k).map_or_else(|| self.label(s), |p| p.rsplit('/').next().unwrap_or(p).to_owned());
                format!("{} {name}", self.label(s))
            })
            .collect();
        ctx.copy_text(names.join("\n"));
        let n = slots.len();
        self.status = if cut {
            format!("Cut {n} sample(s): ⌘V pastes into empty slots from the cursor, ⇧⌘V inserts there and shifts the rest down")
        } else {
            format!("Copied {n} sample(s): paste into a file pane to download them")
        };
        self.clip = Some(PoolClip { olds: slots.iter().map(|&s| self.order[s]).collect(), cut });
        self.clipped = true;
    }

    /// Drops the cut or copied slots (the file clipboard took over).
    pub(crate) fn drop_clip(&mut self) {
        self.clip = None;
    }

    /// Positions now of the clipped samples still in the pool.
    pub(super) fn clip_positions(&self) -> Vec<usize> {
        let Some(c) = &self.clip else { return Vec::new() };
        let mut v: Vec<usize> = c
            .olds
            .iter()
            .filter_map(|&o| self.order.iter().position(|&x| x == o))
            .filter(|&s| self.key_at(s).is_some())
            .collect();
        v.sort_unstable();
        v
    }

    /// The cut or copied samples with their slot labels, for downloading
    /// into a file pane.
    pub(crate) fn clip_keys(&self) -> Vec<((u32, u32), String)> {
        self.clip_positions()
            .into_iter()
            .filter_map(|s| Some((self.key_at(s)?, self.label(s))))
            .collect()
    }

    /// Pastes the cut slots at `at`; the slots they were cut from are left
    /// empty. `shift`: inserted at `at`, pushing the samples below down,
    /// each into the next empty slot further down. Otherwise they go into
    /// the empty slots from `at` on and nothing else moves.
    pub(super) fn paste(&mut self, at: usize, shift: bool) {
        let Some(clip) = &self.clip else {
            self.want_paste_files = Some(at);
            return;
        };
        if !clip.cut {
            self.status = "Copied slots paste into a file pane (downloading them). To move them in the pool, cut with ⌘X".into();
            return;
        }
        let from = self.clip_positions();
        if from.is_empty() {
            self.clip = None;
            self.status = "Nothing to paste: the cut slots are empty now".into();
            return;
        }
        let n = self.order.len();
        let at = at.clamp(1, n - 1);
        let cut: HashSet<usize> = from.iter().copied().collect();
        // `None`: a slot being cut, empty once its sample leaves.
        let mut v: Vec<Option<u16>> = self.order.iter().enumerate().map(|(i, &o)| (!cut.contains(&i)).then_some(o)).collect();
        // Empty entries taken out of the way, to fill the cut slots after.
        let mut spare = Vec::new();
        let mut placed = Vec::new();
        let mut pos = at;
        for &f in &from {
            let Some(e) = (pos..n).find(|&i| v[i].is_none_or(|o| self.free(o))) else {
                self.status = format!(
                    "Not enough empty slots from {} down for {} sample(s); nothing pasted",
                    self.label(at),
                    from.len()
                );
                return;
            };
            spare.extend(v[e]);
            if shift {
                v[pos..=e].rotate_right(1);
                v[pos] = Some(self.order[f]);
                placed.push(pos);
                pos += 1;
            } else {
                v[e] = Some(self.order[f]);
                placed.push(e);
                pos = e + 1;
            }
        }
        let mut spare = spare.into_iter();
        for s in v.iter_mut().filter(|s| s.is_none()) {
            *s = spare.next();
        }
        let Some(new) = v.into_iter().collect::<Option<Vec<u16>>>() else {
            self.status = "Paste failed: the slot count didn't add up; nothing changed".into();
            return;
        };
        self.edit(|o| *o = new);
        self.sel = placed.iter().copied().collect();
        self.cursor = placed.first().copied();
        self.reveal = self.cursor;
        self.clip = None;
        let (a, b) = (placed[0], placed[placed.len() - 1]);
        let what = if a == b { self.label(a) } else { format!("{}–{}", self.label(a), self.label(b)) };
        self.status = if shift {
            format!("Pasted at {what}; the samples below shifted down into the next empty slots")
        } else {
            format!("Pasted into {what}")
        };
    }

    /// Local files from the file clipboard: into the empty slots from `at` on.
    pub(crate) fn paste_files(&mut self, at: usize, files: Vec<std::path::PathBuf>) {
        if !self.pool_editable() {
            self.status = "The pool can't take files now (read-only, from a file, or the device is busy)".into();
            return;
        }
        let Some(s) = (at.max(1)..self.order.len()).find(|&s| self.free(self.order[s])) else {
            self.status = "No empty slot from the cursor down".into();
            return;
        };
        self.drop_files(s, files);
    }

    /// Runs an edit of `order`, keeping the selection on the same samples and
    /// recording undo when something changed.
    pub(super) fn edit(&mut self, f: impl FnOnce(&mut Vec<u16>)) {
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

    pub(super) fn undo(&mut self) {
        if let Some((prev, fills)) = self.undo.pop() {
            self.order = prev;
            self.fills = fills;
            self.status = "Undone".into();
            self.edited_at = Some(std::time::Instant::now());
        }
    }

    /// Moves the selection so it starts right before slot `to` (as numbered
    /// now). Everything in between closes up.
    pub(super) fn move_sel_to(&mut self, to: usize) {
        let sel: Vec<usize> = self.sel.iter().copied().collect();
        if sel.is_empty() {
            return;
        }
        let n = self.order.len();
        let to = to.clamp(1, n);
        self.edit(|order| slot_list::insert(order, &sel, to, 1));
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
    pub(super) fn place_sel_at(&mut self, to: usize) {
        let sel: Vec<usize> = self.sel.iter().copied().collect();
        let n = self.order.len();
        if sel.is_empty() {
            return;
        }
        let to = to.clamp(1, n - sel.len());
        let targets: Vec<usize> = (to..to + sel.len()).collect();
        let sset: HashSet<usize> = sel.iter().copied().collect();
        let displaced: Vec<usize> = targets
            .iter()
            .copied()
            .filter(|t| !sset.contains(t))
            .collect();
        let occupied = displaced.iter().filter(|&&t| !self.is_empty_at(t)).count();
        self.edit(|order| {
            slot_list::place(order, &sel, to, 1);
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
    pub(super) fn nudge(&mut self, dir: i32) {
        let sel: Vec<usize> = self.sel.iter().copied().collect();
        let n = self.order.len();
        let (Some(&lo), Some(&hi)) = (sel.first(), sel.last()) else {
            return;
        };
        if (dir < 0 && lo <= 1) || (dir > 0 && hi + 1 >= n) {
            return;
        }
        self.edit(|order| slot_list::nudge(order, &sel, dir, 1));
    }

    /// Puts `n` empty slots at `at`, pushing `at` and the samples below it
    /// down; each one uses up the next empty slot further down.
    pub(super) fn insert_empty(&mut self, at: usize, n: usize) {
        if self.project.is_none() || n == 0 {
            return;
        }
        let mut order = self.order.clone();
        let (made, last) = slot_list::insert_empty(&mut order, at, n, |o| self.free(*o));
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
    pub(super) fn close_gap(&mut self, at: usize) {
        if self.project.is_none() {
            return;
        }
        let mut order = self.order.clone();
        slot_list::close_gap(&mut order, at, |o| self.free(*o));
        self.edit(|o| *o = order);
        self.status = format!("Closed gap at {}", self.label(at));
    }

    /// Empties `slots` (as ordered now): the unused ones straight away, then
    /// asks before emptying any that kits, locks or sounds still point at.
    pub(super) fn clear(&mut self, slots: &[usize]) {
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
    pub(super) fn clear_olds(&mut self, olds: &[u16]) {
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
    pub(super) fn unused_slots(&self) -> Vec<usize> {
        let Some(p) = &self.project else { return Vec::new() };
        (1..self.order.len())
            .filter(|&s| {
                let o = self.order[s];
                !self.free(o) && p.uses[o as usize] == 0
            })
            .collect()
    }

    /// Pair dropped files with this slot and the empty ones after it.
    pub(super) fn drop_files(&mut self, slot: usize, files: Vec<std::path::PathBuf>) {
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

    pub(super) fn click(&mut self, slot: usize, m: egui::Modifiers) {
        slot_list::click(&mut self.sel, &mut self.cursor, slot, m, 1);
    }
}
