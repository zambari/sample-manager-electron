//! A pool sample opened in the editor from the device view's slot list:
//! saving it ("Update on device") uploads the edit next to the original on
//! the +Drive, puts it in the same slot and pushes, so the device plays it.
//!
//! "Save to → pool slot" ([`SlotSave`]) renders the editor's sample into
//! the selected slot (asking first when it's filled) or the first empty
//! one; once it lands, it becomes a pool edit of that slot.
//!
//! The slot is found again by the sample in it, so reordering the pool while
//! editing doesn't lose it. The original +Drive file is never overwritten
//! (other projects may use it); the first save makes `name 2`, later saves
//! replace that copy.

use super::*;

pub(super) struct PoolEdit {
    /// The editor's file for it (a local copy or the preview cache).
    local: PathBuf,
    /// The sample in the slot now, `(hash, length)`.
    key: (u32, u32),
    /// Where it was last seen (position in the current order).
    pos: usize,
    /// +Drive file it was opened from.
    home: PathBuf,
    /// Our own upload from an earlier save: the next save replaces it.
    ours: Option<PathBuf>,
    /// Rendering: `(staged file, +Drive folder, policy)`.
    rendering: Option<(PathBuf, PathBuf, Policy)>,
    /// Slot (as read) the upload is on its way into.
    waiting: Option<u16>,
}

/// Same naming as the slot list: `A012` with several banks, else `012`.
fn slot_label(pos: usize, pool_len: usize) -> String {
    const BANK: usize = 128;
    if pool_len > BANK {
        format!("{}{:03}", (b'A' + (pos / BANK) as u8) as char, pos % BANK)
    } else {
        format!("{pos:03}")
    }
}

impl PoolEdit {
    /// The render failed: the next save starts over.
    pub(super) fn cancel_render(&mut self) {
        self.rendering = None;
    }
}

/// "Save to slot": the editor's sample rendered, uploaded and put into the
/// empty slot selected in the device view. Once it lands it becomes a
/// [`PoolEdit`], so later saves update that slot.
pub(super) struct SlotSave {
    /// Slot as read, and its position when saved.
    old: u16,
    pos: usize,
    /// The render, and the +Drive folder it goes to.
    staged: PathBuf,
    dir: PathBuf,
    /// Rendered and on its way to the device.
    sent: bool,
    /// Replacing a filled slot: the sample it held when the user said yes.
    /// `None`: the slot was empty and must still be.
    expect: Option<(u32, u32)>,
    /// A kit went in: it lands and is pushed, but doesn't become a pool edit
    /// (the editor keeps the kit's own file).
    kit: bool,
}

/// Which pool slot "Save to" aims at.
pub use crate::ui::editor_view::SlotPick;

/// A save that overwrites something other than the open file, waiting on
/// the user's yes.
pub(super) enum SaveConfirm {
    /// The local original a device copy came from.
    Original(PathBuf),
    /// A filled pool slot: what you hear goes up as a new +Drive file and
    /// takes the slot's place.
    ReplaceSlot { old: u16, key: (u32, u32), label: String, name: String },
}

impl SaveConfirm {
    /// Title, lines (the first one plain, the rest quiet) and the yes button.
    pub(super) fn text(&self) -> (String, Vec<String>, String) {
        match self {
            SaveConfirm::Original(p) => (
                "Overwrite the original file?".to_owned(),
                vec![
                    format!("What you hear replaces {}.", p.display()),
                    "Its slice markers and other WAV metadata are lost (the device copy never had them).                      The device keeps playing its own copy until you update it (the slot turns amber)."
                        .to_owned(),
                ],
                "Overwrite original".to_owned(),
            ),
            SaveConfirm::ReplaceSlot { label, name, .. } => (
                format!("Replace slot {label}?"),
                vec![
                    format!("Slot {label} holds \"{name}\". What you hear goes into it instead."),
                    "Every pattern step using this slot plays the new sample. The old file stays on the +Drive;                      the new one is uploaded next to it. Undo in the Samples page puts the old one back until you push."
                        .to_owned(),
                ],
                format!("Replace slot {label}"),
            ),
        }
    }
}

impl App {
    /// A pool sample was opened in the editor from `local`.
    pub(super) fn note_pool_edit(&mut self, k: (u32, u32), local: &Path) {
        let st = &self.elek;
        let Some(home) = st.names.get(&k).map(PathBuf::from) else {
            self.pool_edit = None;
            return;
        };
        let Some(pos) = (1..st.order.len()).find(|&s| {
            let e = st.entry(st.order[s] as usize);
            !e.is_empty() && e.key() == k
        }) else {
            self.pool_edit = None;
            return;
        };
        // Reopening the sample we last saved keeps replacing our own copy.
        let ours = self.pool_edit.take().filter(|p| p.key == k).and_then(|p| p.ours);
        self.pool_edit = Some(PoolEdit {
            local: local.to_path_buf(),
            key: k,
            pos,
            home,
            ours,
            rendering: None,
            waiting: None,
        });
    }

    /// The slot the sample in the editor belongs to: `(position now, slot
    /// as read)`. None when the editor shows something else or the project
    /// can't take it.
    fn pool_edit_slot(&self) -> Option<(usize, u16)> {
        let pe = self.pool_edit.as_ref()?;
        if self.showing_kit || self.browser().path() != Some(pe.local.as_path()) {
            return None;
        }
        let st = &self.elek;
        let p = st.project.as_ref()?;
        if p.read_only.is_some() || p.from_file.is_some() {
            return None;
        }
        let holds = |s: usize| {
            let old = *st.order.get(s)?;
            let e = st.entry(old as usize);
            (!e.is_empty() && e.key() == pe.key).then_some((s, old))
        };
        holds(pe.pos).or_else(|| (1..st.order.len()).find_map(holds))
    }

    /// Label for the editor's button, when saving goes to a pool slot.
    pub(super) fn pool_edit_label(&self) -> Option<String> {
        let (pos, _) = self.pool_edit_slot()?;
        let len = self.elek.project.as_ref().map_or(0, |p| p.pool.len());
        Some(slot_label(pos, len))
    }

    /// Save / Export go to the device slot instead of a file.
    pub(super) fn saves_to_pool(&self) -> bool {
        self.pool_edit_slot().is_some()
    }

    /// Render what you hear and send it to the slot it came from.
    pub(super) fn update_on_device(&mut self) {
        if self.editor.exporting {
            return;
        }
        let Some((pos, _)) = self.pool_edit_slot() else { return };
        if !self.editor.can_save() {
            self.status = "Nothing to update — the sample is unchanged".to_owned();
            if let Some(leave) = self.leave_after_save.take() {
                self.finish_leave(leave);
            }
            return;
        }
        let filling = &self.elek.filling;
        let pe = self.pool_edit.as_mut().expect("checked by pool_edit_slot");
        // A failed upload drops the slot from `filling`: not waiting any more.
        pe.waiting = pe.waiting.filter(|w| filling.contains_key(w));
        if pe.waiting.is_some() {
            self.status = "The last update is still on its way to the device".to_owned();
            return;
        }
        pe.pos = pos;
        let (ours, home) = (pe.ours.clone(), pe.home.clone());
        let (dir, name, policy) = match &ours {
            Some(o) => (
                o.parent().map_or_else(|| PathBuf::from("/"), Path::to_path_buf),
                o.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                Policy::Replace,
            ),
            None => {
                let dir = home.parent().map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
                let stem = home.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                let name = super::editing::next_name(&stem, |c| !self.device_has(&dir.join(c)));
                (dir, name, Policy::KeepBoth)
            }
        };
        let Some(cache) = crate::links::pool_cache_dir().map(|c| c.join("edits")) else {
            self.errors.push("No cache folder to render the update into".to_owned());
            return;
        };
        if let Err(e) = std::fs::create_dir_all(&cache) {
            self.errors.push(format!("{}: {e}", cache.display()));
            return;
        }
        let staged = cache.join(device::wav_name(&name));
        let pe = self.pool_edit.as_mut().expect("checked by pool_edit_slot");
        pe.rendering = Some((staged.clone(), dir, policy));
        self.status = format!("Rendering {name} for slot {}…", self.pool_edit_label().unwrap_or_default());
        let mono = self.editor.fold();
        self.editor.export(staged, mono);
    }

    /// `p` is the render of an update on its way to the device.
    pub(super) fn pool_edit_rendered(&self, p: &Path) -> bool {
        self.pool_edit.as_ref().and_then(|pe| pe.rendering.as_ref()).is_some_and(|(s, _, _)| s == p)
    }

    /// The render is written: upload it, put it in the slot when it lands,
    /// and keep editing the rendered file.
    pub(super) fn pool_edit_upload(&mut self, p: PathBuf) {
        let slot = self.pool_edit_slot();
        let Some(pe) = self.pool_edit.as_mut() else { return };
        let Some((staged, dir, policy)) = pe.rendering.take() else { return };
        let leave = self.leave_after_save.take();
        let Some((_, old)) = slot else {
            self.status = "The slot's sample changed meanwhile; nothing sent".to_owned();
            if let Some(l) = leave {
                self.finish_leave(l);
            }
            return;
        };
        let name = staged.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let bytes = std::fs::metadata(&staged).map_or(0, |m| m.len());
        let task = device::Task::Transfer {
            dir: device::Dir::Upload,
            items: vec![(staged.clone(), dir.clone())],
            policy,
            mono: None,
            total_bytes: bytes,
            local_names: Default::default(),
        };
        let job = self.run_device(task, &format!("Updating {name} on the device"), 1, bytes);
        self.pool_fetch.uploads.insert(job, vec![(old, staged.clone())]);
        self.elek.filling.insert(old, name.clone());
        let pe = self.pool_edit.as_mut().expect("checked above");
        pe.waiting = Some(old);
        pe.local = staged.clone();
        self.status = format!("Sending {name} to {}…", device::dev_str(&dir));
        // Go on editing what was sent (trimmed, gain applied).
        match leave {
            Some(l) => self.finish_leave(l),
            None if self.editor.path() == Some(p.as_path()) => {
                if self.voice_path.as_deref() == Some(p.as_path()) {
                    self.engine.stop();
                }
                self.editor.reload_saved();
            }
            None => self.audition(&p, false),
        }
    }

    /// The slot "Save to" would fill: `(position now, slot as read)`. None
    /// for the slot the sample already belongs to (that one is Save). A kit
    /// goes in whole, like its export.
    fn slot_target(&self, pick: SlotPick) -> Option<(usize, u16)> {
        if self.editor.current.is_none() {
            return None;
        }
        let t = match pick {
            SlotPick::Selected => self.elek.lone_sel(),
            SlotPick::FirstFree => self.elek.first_free(),
        }?;
        let own = self.pool_edit_slot().is_some_and(|(_, old)| old == t.1);
        (!own).then_some(t)
    }

    /// The pool targets for the editor's "Save to" menu. None for a kit or
    /// without a project read from the device.
    pub(super) fn pool_targets(&self) -> Option<crate::ui::editor_view::PoolTargets> {
        let p = self.elek.project.as_ref()?;
        let len = p.pool.len();
        let note = if p.read_only.is_some() || p.from_file.is_some() {
            Some("This project was loaded from a file or can't be edited: read the device's project first")
        } else if self.elek.edit_locked() {
            Some("The device is busy: try again once it's done")
        } else {
            None
        };
        let selected = self.slot_target(SlotPick::Selected).map(|(pos, old)| {
            let e = self.elek.entry(old as usize);
            let name = (!e.is_empty()).then(|| self.elek.names.get(&e.key()).map_or_else(|| "a sample".to_owned(), |n| {
                Path::new(n).file_name().map_or_else(|| n.clone(), |f| f.to_string_lossy().into_owned())
            }));
            (slot_label(pos, len), name)
        });
        let first_free = self.slot_target(SlotPick::FirstFree).map(|(pos, _)| slot_label(pos, len));
        Some(crate::ui::editor_view::PoolTargets { selected, first_free, note })
    }

    /// Render what you hear into a pool slot: an empty one right away, a
    /// filled one after asking.
    pub(super) fn save_to_slot(&mut self, pick: SlotPick) {
        if self.editor.exporting {
            return;
        }
        let Some((pos, old)) = self.slot_target(pick) else {
            self.status = "No pool slot to save into".to_owned();
            return;
        };
        let e = self.elek.entry(old as usize);
        if !e.is_empty() {
            let len = self.elek.project.as_ref().map_or(0, |p| p.pool.len());
            let name = self.elek.names.get(&e.key()).cloned().unwrap_or_else(|| "a sample".to_owned());
            self.save_confirm = Some(SaveConfirm::ReplaceSlot { old, key: e.key(), label: slot_label(pos, len), name });
            return;
        }
        self.save_into_slot(pos, old, None);
    }

    /// The user said yes to an overwrite.
    pub(super) fn confirmed_save(&mut self, c: SaveConfirm) {
        match c {
            SaveConfirm::Original(p) => self.overwrite_original(p),
            SaveConfirm::ReplaceSlot { old, key, .. } => {
                let st = &self.elek;
                let Some(pos) = st.order.iter().position(|&o| o == old) else { return };
                let e = st.entry(old as usize);
                if e.is_empty() || e.key() != key || st.filling.contains_key(&old) {
                    self.status = "The slot changed meanwhile; nothing saved".to_owned();
                    return;
                }
                self.save_into_slot(pos, old, Some(key));
            }
        }
    }

    /// "Save to → over the original": ask first.
    pub(super) fn ask_overwrite_original(&mut self) {
        if let Some((orig, _)) = self.editor_origin().and_then(|o| o.original) {
            self.save_confirm = Some(SaveConfirm::Original(orig));
        }
    }

    /// Render what you hear over `orig`, the local file a device copy came
    /// from. The editor stays on the copy.
    fn overwrite_original(&mut self, orig: PathBuf) {
        if self.editor.exporting || self.editor.current.is_none() {
            return;
        }
        self.status = format!("Saving over {}…", orig.display());
        self.saving.insert(orig.clone(), None);
        let mono = self.editor.fold();
        self.editor.export(orig, mono);
    }

    /// Render what you hear and put it into slot `old` (at `pos`).
    /// `expect`: the sample a filled slot must still hold.
    fn save_into_slot(&mut self, pos: usize, old: u16, expect: Option<(u32, u32)>) {
        // A failed upload drops the slot from `filling`: not waiting any more.
        if self.slot_save.as_ref().is_some_and(|s| s.sent && self.elek.filling.contains_key(&s.old)) {
            self.status = "The last save is still on its way to the device".to_owned();
            return;
        }
        // Next to its device original when it has one, else the +Drive tab.
        // A kit's device home would be the browsed sample's: not used.
        let home = if self.showing_kit { None } else { self.device_home() };
        let dir = home
            .and_then(|h| h.parent().map(Path::to_path_buf))
            .or_else(|| self.pool_upload_dir());
        let Some(dir) = dir else {
            self.status = "Open the device's +Drive in a file tab first, at the folder new samples should go to".to_owned();
            return;
        };
        // A grid kit carries its slot count, as its export would.
        let named = self.editor.path().map(|p| self.export_name(p));
        let stem = named
            .as_deref()
            .and_then(|p| p.file_stem())
            .map_or_else(|| "sample".to_owned(), |s| s.to_string_lossy().into_owned());
        let name = if self.device_has(&dir.join(&stem)) {
            super::editing::next_name(&stem, |c| !self.device_has(&dir.join(c)))
        } else {
            stem
        };
        let Some(cache) = crate::links::pool_cache_dir().map(|c| c.join("edits")) else {
            self.errors.push("No cache folder to render the sample into".to_owned());
            return;
        };
        if let Err(e) = std::fs::create_dir_all(&cache) {
            self.errors.push(format!("{}: {e}", cache.display()));
            return;
        }
        let staged = cache.join(device::wav_name(&name));
        self.slot_save = Some(SlotSave { old, pos, staged: staged.clone(), dir, sent: false, expect, kit: self.showing_kit });
        let len = self.elek.project.as_ref().map_or(0, |p| p.pool.len());
        self.status = format!("Rendering {name} for slot {}…", slot_label(pos, len));
        let mono = self.editor.fold();
        self.editor.export(staged, mono);
    }

    /// `p` is the render of a "Save to slot".
    pub(super) fn slot_save_rendered(&self, p: &Path) -> bool {
        self.slot_save.as_ref().is_some_and(|s| !s.sent && s.staged == p)
    }

    /// The render failed: forget it.
    pub(super) fn slot_save_cancel(&mut self) {
        self.slot_save.take_if(|s| !s.sent);
    }

    /// The render is written: upload it, fill the slot when it lands, and
    /// go on editing what was sent.
    pub(super) fn slot_save_upload(&mut self, p: PathBuf) {
        let Some(s) = self.slot_save.as_mut() else { return };
        let (old, staged, dir, expect) = (s.old, s.staged.clone(), s.dir.clone(), s.expect);
        let st = &self.elek;
        let still = match expect {
            None => st.free(old),
            Some(k) => {
                let e = st.entry(old as usize);
                !st.filling.contains_key(&old) && !e.is_empty() && e.key() == k
            }
        };
        if st.project.is_none() || !still {
            self.slot_save = None;
            self.status = "The slot changed or the project closed meanwhile; nothing sent".to_owned();
            return;
        }
        let s = self.slot_save.as_mut().expect("checked above");
        s.sent = true;
        let name = staged.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let bytes = std::fs::metadata(&staged).map_or(0, |m| m.len());
        let task = device::Task::Transfer {
            dir: device::Dir::Upload,
            items: vec![(staged.clone(), dir.clone())],
            policy: Policy::KeepBoth,
            mono: None,
            total_bytes: bytes,
            local_names: Default::default(),
        };
        let job = self.run_device(task, &format!("Saving {name} to the device"), 1, bytes);
        self.pool_fetch.uploads.insert(job, vec![(old, staged)]);
        self.elek.filling.insert(old, name.clone());
        self.status = format!("Sending {name} to {}…", device::dev_str(&dir));
        // A kit stays the kit; a sample goes on with what was sent.
        if self.showing_kit {
            return;
        }
        if self.voice_path.as_deref() == Some(p.as_path()) {
            self.engine.stop();
        }
        self.audition(&p, false);
    }

    /// A file landed in slot `old` (as read): if it was our update, follow
    /// it and push so the device plays it straight away.
    pub(super) fn pool_edit_filled(&mut self, old: u16, key: (u32, u32), path: &str) {
        // A "Save to slot" landed: from now on it's that slot's sample.
        if let Some(s) = self.slot_save.take_if(|s| s.sent && s.old == old) {
            if s.kit {
                let label = slot_label(s.pos, self.elek.project.as_ref().map_or(0, |p| p.pool.len()));
                return self.push_filled(label);
            }
            let path = PathBuf::from(path);
            self.pool_edit = Some(PoolEdit {
                local: s.staged,
                key,
                pos: s.pos,
                home: path.clone(),
                ours: Some(path),
                rendering: None,
                waiting: Some(old),
            });
        }
        let Some(pe) = self.pool_edit.as_mut().filter(|pe| pe.waiting == Some(old)) else { return };
        pe.waiting = None;
        pe.key = key;
        pe.ours = Some(PathBuf::from(path));
        let label = slot_label(pe.pos, self.elek.project.as_ref().map_or(0, |p| p.pool.len()));
        self.push_filled(label);
    }

    /// Slot `label` was just filled from the editor: push, so the device
    /// plays it, unless other edits or the device say wait.
    fn push_filled(&mut self, label: String) {
        let st = &mut self.elek;
        if st.live {
            // Live mode pushes it once the edits pause.
            st.status = format!("Updated slot {label}; going to the device");
            return;
        }
        if st.changed() > 1 {
            st.status = format!("Slot {label} updated here; push to send it with your other changes");
            return;
        }
        if st.busy.is_some() || !st.pool_pushable() {
            st.status = format!("Slot {label} updated here; push when the device is free");
            return;
        }
        st.status = format!("Updating slot {label} on the device…");
        self.elek_push(false, false);
    }
}
