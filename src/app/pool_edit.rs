//! A pool sample opened in the editor from the device view's slot list:
//! saving it ("Update on device") uploads the edit next to the original on
//! the +Drive, puts it in the same slot and pushes, so the device plays it.
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

    /// The empty slot "Save to slot" would fill: `(position now, slot as
    /// read)`. None for a kit, a pool sample (that one updates its own slot)
    /// or without exactly one empty slot selected.
    fn slot_save_target(&self) -> Option<(usize, u16)> {
        if self.showing_kit || self.editor.current.is_none() || self.saves_to_pool() {
            return None;
        }
        self.elek.lone_empty_sel()
    }

    /// Label for the editor's "Save to slot" button.
    pub(super) fn slot_save_label(&self) -> Option<String> {
        let (pos, _) = self.slot_save_target()?;
        let len = self.elek.project.as_ref().map_or(0, |p| p.pool.len());
        Some(slot_label(pos, len))
    }

    /// Render what you hear and put it into the selected empty slot.
    pub(super) fn save_to_slot(&mut self) {
        if self.editor.exporting {
            return;
        }
        let Some((pos, old)) = self.slot_save_target() else { return };
        // A failed upload drops the slot from `filling`: not waiting any more.
        if self.slot_save.as_ref().is_some_and(|s| s.sent && self.elek.filling.contains_key(&s.old)) {
            self.status = "The last save is still on its way to the device".to_owned();
            return;
        }
        // Next to its device original when it has one, else the +Drive tab.
        let dir = self
            .device_home()
            .and_then(|h| h.parent().map(Path::to_path_buf))
            .or_else(|| self.pool_upload_dir());
        let Some(dir) = dir else {
            self.status = "Open the device's +Drive in a file tab first, at the folder new samples should go to".to_owned();
            return;
        };
        let stem = self
            .editor
            .path()
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
        self.slot_save = Some(SlotSave { old, pos, staged: staged.clone(), dir, sent: false });
        self.status = format!("Rendering {name} for slot {}…", self.slot_save_label().unwrap_or_default());
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
        let (old, staged, dir) = (s.old, s.staged.clone(), s.dir.clone());
        if self.elek.project.is_none() || !self.elek.free(old) {
            self.slot_save = None;
            self.status = "The slot was filled or the project closed meanwhile; nothing sent".to_owned();
            return;
        }
        s.sent = true;
        let name = staged.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let bytes = std::fs::metadata(&staged).map_or(0, |m| m.len());
        let task = device::Task::Transfer {
            dir: device::Dir::Upload,
            items: vec![(staged.clone(), dir.clone())],
            policy: Policy::KeepBoth,
            mono: None,
            total_bytes: bytes,
        };
        let job = self.run_device(task, &format!("Saving {name} to the device"), 1, bytes);
        self.pool_fetch.uploads.insert(job, vec![(old, staged)]);
        self.elek.filling.insert(old, name.clone());
        self.status = format!("Sending {name} to {}…", device::dev_str(&dir));
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
        let st = &mut self.elek;
        let label = slot_label(pe.pos, st.project.as_ref().map_or(0, |p| p.pool.len()));
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
