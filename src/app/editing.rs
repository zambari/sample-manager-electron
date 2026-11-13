//! Playback, auditioning and the audio editor: loading, exporting, unsaved-edit prompts.

use super::*;

impl App {
    // ---- playback ---------------------------------------------------------

    pub(super) fn playing_current(&self) -> bool {
        self.engine.is_playing()
            && self.voice_path.is_some()
            && self.voice_path.as_deref() == self.editor.path()
    }

    /// Play from wherever the toolbar's "From" setting says.
    pub(super) fn start_play(&mut self) {
        self.start_play_from(self.editor.prefs.play_from_in);
    }

    /// Play the editor's sample from the in point, or from the parked
    /// playhead (falling back to the in point if the playhead is outside
    /// the region).
    pub(super) fn start_play_from(&mut self, from_in: bool) {
        let Some(s) = self.editor.sample().cloned() else {
            return;
        };
        let (a, b) = self.editor.sel;
        let ph = self.editor.playhead;
        let from = if !from_in && (a..b).contains(&ph) {
            ph
        } else {
            a
        };
        self.voice_once = self.once_for.take().is_some_and(|p| p == s.path);
        self.voice_path = Some(s.path.clone());
        self.voice_sample = Some(s.clone());
        self.engine.play(
            s,
            PlayParams {
                from,
                loop_start: a,
                end: b,
                looping: self.editor.prefs.looping && !self.voice_once,
                gain: self.editor.gain(),
                mono: self.editor.fold(),
            },
        );
    }

    pub(super) fn toggle_play(&mut self) {
        if self.playing_current() {
            self.engine.stop();
        } else {
            self.start_play();
        }
    }

    /// Load `path` into the editor, optionally starting playback once it's in.
    pub(super) fn audition(&mut self, path: &Path, play: bool) {
        // A sliced "[N] name" file opens as a kit instead.
        if self.open_browsed_kit(path, play) {
            return;
        }
        // Browsing always happens in the sample slot, not the kit.
        self.show_kit(false);
        if self.editor.path() == Some(path) {
            if play {
                self.start_play();
            }
            return;
        }
        if self.editor.is_dirty() {
            self.leave_pending = Some(Leave::Audition(path.to_path_buf(), play));
            return;
        }
        if self.engine.is_playing() {
            self.engine.stop();
        }
        self.editor.play_when_loaded = play;
        self.editor.request(path);
        if self.opened.insert(path.to_path_buf()) {
            self.opened_dirty = true;
        }
    }

    /// Space / double-click on a file row.
    pub(super) fn preview_toggle(&mut self, path: &Path) {
        if self.editor.path() == Some(path) && self.playing_current() {
            self.once_for = None;
            self.engine.stop();
        } else {
            self.audition(path, true);
        }
    }

    /// Device previews take over from editor playback, even before a load
    /// or an unsaved-edit prompt finishes. Clicking a running preview stops it.
    pub(super) fn preview_device_sample(&mut self, path: &Path) {
        let stopping = self.voice_once
            && self.playing_current()
            && self.editor.path() == Some(path);
        self.engine.stop();
        self.editor.play_when_loaded = false;
        self.once_for = None;
        if !stopping {
            self.once_for = Some(path.to_path_buf());
            self.audition(path, true);
        }
    }

    /// Auto-load whatever the file cursor is on, when it changes.
    pub(super) fn follow_cursor(&mut self) {
        let tab = self.pane(self.focused).active_tab();
        let cur = tab
            .cursor_entry()
            .filter(|e| self.loadable(tab, e))
            .map(|e| e.path.clone());
        if self.hold_cursor_load {
            // Wait for the restored listing to settle, then adopt its cursor
            // without loading it.
            if !self.pane(self.focused).active_tab().loading {
                self.hold_cursor_load = false;
                self.last_cursor = cur;
            }
            return;
        }
        if cur == self.last_cursor {
            return;
        }
        // Building a multi-selection moves the cursor too; don't load each
        // row it passes. Leave `last_cursor` alone so collapsing back to a
        // single row still loads it.
        if self.pane(self.focused).active_tab().selected.len() > 1 {
            return;
        }
        self.last_cursor = cur.clone();
        if let Some(p) = cur {
            if self.editor.prefs.auto_load {
                self.audition_row(&p, self.editor.prefs.auto_play);
            }
        }
    }

    /// Step the file cursor to the next audio file in `dir` direction.
    pub(super) fn step_file(&mut self, dir: isize) {
        let n = self.pane(self.focused).active_tab().entries.len();
        for _ in 0..n {
            let tab = self.pane_mut(self.focused).active_tab_mut();
            let before = tab.cursor;
            tab.move_cursor(dir, false);
            let tab = self.pane(self.focused).active_tab();
            if tab.cursor == before || tab.cursor_entry().is_some_and(|e| self.loadable(tab, e)) {
                break;
            }
        }
        let tab = self.pane(self.focused).active_tab();
        let path = tab
            .cursor_entry()
            .filter(|e| self.loadable(tab, e))
            .map(|e| e.path.clone());
        if let Some(p) = path {
            // Explicit request: load even when auto-load is off.
            self.last_cursor = Some(p.clone());
            self.audition_row(&p, self.editor.prefs.auto_play);
        }
    }

    pub(super) fn export(&mut self) {
        if self.editor.current.is_none() {
            return;
        }
        // Opened from a pool slot: its destination is that slot (the
        // button says "Export → slot …").
        if self.saves_to_pool() {
            return self.update_on_device();
        }
        let tab = self.pane(Role::Dest).active_tab();
        let (dest, to_dev) = (tab.cwd.clone(), tab.device);
        let mono = self.editor.fold();
        self.export_to(dest, to_dev, mono);
    }

    pub(super) fn export_to(&mut self, dest: PathBuf, to_dev: bool, mono: Option<Fold>) {
        let Some(src) = self.editor.path().map(Path::to_path_buf) else {
            return;
        };
        // Named as the kit will be, slot count included (see `export_name`).
        let named = self.export_name(&src);
        if to_dev {
            let dst = device::upload_target(&named, &dest);
            let op = ops::Op {
                conflict: self.device_has(&dst),
                approx_bytes: 0,
                src,
                dst,
                is_dir: false,
            };
            let plan = Plan {
                kind: Kind::Copy,
                ops: vec![op],
                rejected: Vec::new(),
            };
            return self.offer(plan, PlanKind::DeviceExport { mono });
        }
        let dst = crate::audio::export::target_for(&named, &dest);
        // A kit never saved yet, exported beside its planned file: that is its
        // first save, so the kit takes the exported name instead of forking.
        if self.editor.is_kit
            && !src.is_file()
            && dst != src
            && dst.parent() == src.parent()
            && !dst.exists()
        {
            self.editor.set_path(dst.clone());
        }
        // Export is "save as": a name that's taken asks for another one,
        // with overwriting the existing file as the alternative.
        if dst.exists() {
            let free = crate::fs_model::unique_sibling(&dst);
            let stem = |p: &Path| {
                p.file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            };
            self.export_naming = Some(ExportNaming {
                dir: dest,
                name: stem(&free),
                taken: dst
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                mono,
            });
            return;
        }
        self.start_export(dst, mono);
    }

    /// The path an export of the open file is named after: a grid kit
    /// carries its slot count as `[n] name`, replacing a stale one.
    pub(super) fn export_name(&self, src: &Path) -> PathBuf {
        match self.editor.grid.filter(|&n| self.editor.is_kit && n > 0) {
            Some(n) => with_slot_count(src, n),
            None => src.to_path_buf(),
        }
    }

    /// Render the editor to exactly `dst`, overwriting it.
    pub(super) fn start_export(&mut self, dst: PathBuf, mono: Option<Fold>) {
        if self.editor.exporting {
            return;
        }
        if let Some(p) = self.editor.path().map(Path::to_path_buf) {
            self.mark_committed([p]);
        }
        // A kit still pointing at source material: the export is its file now.
        if self.editor.is_kit && self.editor.path_is_source() && self.editor.path() != Some(dst.as_path()) {
            self.editor.set_path(dst.clone());
        }
        self.status = format!(
            "Exporting{} to {}…",
            mono.map_or(String::new(), |f| format!(" (mono {})", f.short())),
            dst.display()
        );
        self.saving.insert(dst.clone(), None);
        self.export_rename = Some(dst.clone());
        self.editor.export(dst, mono);
    }

    /// Write what you hear back over the open file (as `<stem>.wav` beside
    /// it when it isn't a WAV). Folded to mono only if you're listening so.
    /// When in / out leave part of the file out, ask whether to trim first.
    pub(super) fn save_in_place(&mut self) {
        if self.editor.current.is_none() || self.editor.exporting {
            return;
        }
        // Opened from a pool slot: saving replaces the slot's sample.
        if self.saves_to_pool() {
            return self.update_on_device();
        }
        // A new kit has no file yet: its first save is the export. Same for
        // a kit whose file is one its slices came from: never written over.
        if self.showing_kit && (!self.editor.path().is_some_and(Path::is_file) || self.editor.path_is_source()) {
            self.export();
            return;
        }
        // Same rule as the Save button: nothing changed, nothing to write.
        if !self.editor.can_save() {
            self.status = "Nothing to save — the file is unchanged".to_owned();
            // Don't strand a close / quit that was waiting on this save.
            if let Some(leave) = self.leave_after_save.take() {
                self.finish_leave(leave);
            }
            return;
        }
        if self.editor.is_trimmed() {
            self.save_scope_pending = true;
        } else {
            self.save_in_place_as(false);
        }
    }

    /// `whole`: write the whole file (with gain), ignoring in / out and crop.
    pub(super) fn save_in_place_as(&mut self, whole: bool) {
        let Some(src) = self.editor.path().map(Path::to_path_buf) else {
            return;
        };
        if self.editor.exporting {
            return;
        }
        let Some(dir) = src.parent() else { return };
        // A kit is saved whole (see `Editor::export`), whatever is selected.
        let target = crate::audio::export::target_for(&src, dir);
        self.status = format!(
            "Saving {}…",
            target.file_name().unwrap_or_default().to_string_lossy()
        );
        let mono = self.editor.fold();
        self.saving.insert(target.clone(), None);
        self.editor.export_with(target, mono, whole);
    }

    /// Do what the unsaved-edits question interrupted.
    pub(super) fn finish_leave(&mut self, leave: Leave) {
        match leave {
            Leave::Audition(p, play) => self.audition(&p, play),
            Leave::LoadEditor(p) => self.load_into_editor(&p),
            Leave::CloseKit => self.close_kit_now = true,
            // Another slot or kit has unsaved edits too: ask about those next.
            Leave::Quit if self.front_dirty() => self.leave_pending = Some(Leave::Quit),
            Leave::Quit => {
                // Closed from `ui` once no job (an upload of the saved edit)
                // is running.
                self.allow_close = true;
                self.close_when_idle = true;
                if !self.active_jobs.is_empty() {
                    self.status = "Quitting once the transfer finishes…".to_owned();
                }
            }
        }
    }

    pub(super) fn answer_leave(&mut self, leave: Leave, choice: LeaveChoice) {
        match choice {
            LeaveChoice::Cancel => {
                self.relaunch = false;
                self.status = "Kept editing — the edits are still unsaved".to_owned();
            }
            LeaveChoice::Discard if matches!(leave, Leave::CloseKit) => {
                self.kit_mut().discard();
                self.finish_leave(leave);
            }
            LeaveChoice::Discard => {
                self.editor.discard();
                self.finish_leave(leave);
            }
            LeaveChoice::SaveInPlace => {
                self.leave_after_save = Some(leave);
                self.save_in_place();
            }
            LeaveChoice::Commit => {
                self.leave_after_save = Some(leave);
                self.export();
            }
        }
    }

    pub(super) fn perform_editor(&mut self, a: EdAction, wave: Option<egui::Rect>) {
        let ed = &mut self.editor;
        let frames = ed.frames();
        // Points set while playing land where you hear them.
        let here = if self.engine.is_playing() && self.voice_path.as_deref() == ed.path() {
            self.engine.position().min(frames)
        } else {
            ed.playhead
        };
        match a {
            EdAction::TogglePlay => self.toggle_play(),
            EdAction::TogglePlayAlt => {
                if self.playing_current() {
                    self.engine.stop();
                } else {
                    self.start_play_from(!self.editor.prefs.play_from_in);
                }
            }
            EdAction::ToggleFollow => ed.prefs.follow_playback = !ed.prefs.follow_playback,
            EdAction::ToggleSnap => {
                ed.prefs.snap_zero = !ed.prefs.snap_zero;
                if ed.prefs.snap_zero {
                    ed.snap_selection();
                }
            }
            EdAction::InZero(dir) | EdAction::OutZero(dir) => {
                if matches!(a, EdAction::InZero(_)) {
                    ed.step_in(dir);
                } else {
                    ed.step_out(dir);
                }
                let f = if matches!(a, EdAction::InZero(_)) {
                    ed.sel.0
                } else {
                    ed.sel.1
                };
                if let Some(r) = wave {
                    editor_view::ensure_visible(ed, r.width(), f);
                }
            }
            EdAction::Crop => ed.crop_to_selection(),
            EdAction::Uncrop => ed.uncrop(),
            EdAction::Undo => {
                if !self.undo_kit_add(true) {
                    self.editor.undo();
                }
            }
            EdAction::Redo => {
                if !self.redo_kit_add() {
                    self.editor.redo();
                }
            }
            EdAction::Stop => self.engine.stop(),
            EdAction::SetIn => ed.set_in(here),
            EdAction::SetOut => ed.set_out(here),
            EdAction::ToggleLoop => ed.prefs.looping = !ed.prefs.looping,
            EdAction::ToggleMono => {
                ed.toggle_mono();
            }
            EdAction::Nudge(n, dir) => {
                let px = ed.view.fpp.round().max(1.0) as isize;
                let step = match n {
                    Nudge::Pixel => px,
                    Nudge::TenPixels => px * 10,
                    Nudge::Frame => 1,
                };
                ed.playhead =
                    (ed.playhead as isize + step * dir as isize).clamp(0, frames as isize) as usize;
                if let Some(r) = wave {
                    editor_view::ensure_visible(ed, r.width(), ed.playhead);
                }
            }
            EdAction::JumpIn | EdAction::JumpOut => {
                ed.playhead = if a == EdAction::JumpIn {
                    ed.sel.0
                } else {
                    ed.sel.1.saturating_sub(1)
                };
                if let Some(r) = wave {
                    editor_view::ensure_visible(ed, r.width(), ed.playhead);
                }
            }
            EdAction::ZoomIn | EdAction::ZoomOut => {
                if let Some(r) = wave {
                    let factor = if a == EdAction::ZoomIn { 0.5 } else { 2.0 };
                    // Zoom around the playhead when it's on screen.
                    let xp = r.left() + ((ed.playhead as f64 - ed.view.start) / ed.view.fpp) as f32;
                    let anchor = if r.x_range().contains(xp) {
                        xp
                    } else {
                        r.center().x
                    };
                    editor_view::zoom_at(ed, r, anchor, factor);
                }
            }
            EdAction::ZoomFit => ed.view.fit_pending = true,
            EdAction::ZoomRegion => {
                if let Some(r) = wave {
                    let len = (ed.sel.1 - ed.sel.0).max(1) as f64;
                    ed.view.fpp = len * 1.1 / r.width().max(1.0) as f64;
                    ed.view.start = ed.sel.0 as f64 - len * 0.05;
                    editor_view::clamp_view(ed, r.width());
                }
            }
            EdAction::SelectAll => ed.select_all(),
            EdAction::Normalize if ed.is_kit => ed.normalize_sel(),
            EdAction::Normalize => ed.normalize(),
            EdAction::InsertSilence => {
                ed.insert_silence(0.1);
            }
            EdAction::Export => self.export(),
            EdAction::Delete => {
                if !ed.delete() {
                    self.status =
                        "Can't delete everything — select part of the file first".to_owned();
                }
            }
            EdAction::SaveInPlace => self.save_in_place(),
            EdAction::NextFile => self.step_file(1),
            EdAction::PrevFile => self.step_file(-1),
            EdAction::AddSlice => self.add_slice(),
            EdAction::ToggleKit => self.show_kit(!self.showing_kit),
            EdAction::SwitchPane => {
                self.editor_focused = false;
                self.focused = Role::Dest;
            }
        }
    }

    /// The local copy of device file `path`, if we know one and it hasn't
    /// changed since. Says so in the status bar. Touches the disk.
    pub(super) fn linked_local(&mut self, path: &Path) -> Option<PathBuf> {
        let e = self.device_entry(path)?;
        let l = self.links.local(e.hash, e.len as u32)?;
        let local = l.local.clone();
        self.status = link_note(l);
        self.device_origin = Some((local.clone(), path.to_path_buf()));
        Some(local)
    }

    /// Where on the device the sample in the editor came from (or was sent
    /// to): the device file it was opened through, else the newest link.
    /// What the sample slot's file is, relative to the device. Only links
    /// are consulted, plus a stat or two of the files involved.
    pub(super) fn editor_origin(&self) -> Option<editor_view::Origin> {
        let path = self.browser().path()?.to_path_buf();
        let cache = crate::links::pool_cache_dir();
        let in_cache = cache.as_ref().is_some_and(|c| path.starts_with(c));
        let link = self.links.for_local(&path);
        let pm = self.pool_match(&path);
        let pool = pm.as_ref().map(|m| (m.slot.clone(), m.same, m.by_name));
        let o = |kind, warn, tip: &str, original| editor_view::Origin {
            path: path.clone(),
            kind,
            warn,
            tip: tip.to_owned(),
            original,
            pool: pool.clone(),
        };
        let Some(l) = link else {
            return Some(match (in_cache, pm.as_ref().and_then(|m| m.same)) {
                (_, Some(true)) => o(
                    "Same audio as on device",
                    false,
                    "Never transferred by this app, but a pool sample holds the same audio (matched by fingerprint)",
                    None,
                ),
                (true, _) => o("Preview cache", false, "A file in the preview cache, not linked to a device file", None),
                (false, _) => o("Local file", false, "Not known to be on the device", None),
            });
        };
        let fresh = l.fresh();
        if l.uploaded {
            let (kind, warn, tip) = match (in_cache, fresh) {
                (true, _) => ("Your last device update", false, "Rendered by Update on device and uploaded from the preview cache"),
                (false, true) => ("Original", false, "The file this device sample was uploaded from, unchanged since"),
                (false, false) => (
                    "Original, changed since upload",
                    true,
                    "Edited since it went to the device: the device still plays the older version",
                ),
            };
            return Some(o(kind, warn, tip, None));
        }
        // A copy fetched from the device: name the original if we know it.
        let original = self
            .links
            .original(l.hash, l.size)
            .filter(|u| u.local != path && u.local.exists())
            .map(|u| (u.local.clone(), u.fresh()));
        let kind = if in_cache { "Device copy (preview cache)" } else { "Fetched from device" };
        let tip = "Downloaded from the device: slice markers and other WAV metadata didn't survive the trip";
        let warn = original.is_some();
        Some(o(kind, warn, tip, original))
    }

    /// Select local file `p` in the source pane, making the panes visible.
    pub(super) fn show_in_source(&mut self, p: &Path) {
        if !p.exists() {
            self.status = format!("{} isn't there any more", p.display());
            return;
        }
        if self.elek_mode {
            self.elek.hide_files = false;
        }
        self.show_file(Role::Source, false, p);
    }

    /// Where each slice of the shown kit came from, for its right-click
    /// menu. A slice keeps the full path of its source file; older kits kept
    /// only a file name, which counts only when it names a file this kit
    /// took audio from in this session. The pool slot is matched by the
    /// +Drive content hash of a link to that file, never by name. No disk
    /// access: a missing file is caught when it's revealed.
    pub(super) fn slice_homes(&self) -> Vec<editor_view::SliceHome> {
        let (names, _) = self.kit().slice_sources();
        let cache = crate::links::pool_cache_dir();
        (0..names.len())
            .map(|i| {
                let Some((local, by_audio)) = self.slice_local(i) else {
                    let n = &names[i];
                    return editor_view::SliceHome { saved_as: (!n.is_empty()).then(|| n.clone()), ..Default::default() };
                };
                let link = self.links.for_local(&local);
                // A preview cache copy: point at its original when known.
                let in_cache = cache.as_ref().is_some_and(|c| local.starts_with(c));
                let shown = link
                    .filter(|_| in_cache)
                    .and_then(|l| self.links.original(l.hash, l.size))
                    .map_or_else(|| local.clone(), |o| o.local.clone());
                let m = self.pool_match(&local);
                editor_view::SliceHome {
                    local: Some(shown),
                    by_audio,
                    pool_same: m.as_ref().and_then(|m| m.same),
                    pool_by_name: m.as_ref().is_some_and(|m| m.by_name),
                    pool: m.map(|m| m.slot),
                    saved_as: None,
                }
            })
            .collect()
    }

    /// Kit slice `i`'s right-click **Reveal in sample pool**: bring the pool
    /// into the right column and flash the slot holding its source.
    pub(super) fn slice_in_pool(&mut self, i: usize) {
        let local = self.slice_local(i).map(|(p, _)| p);
        let Some(m) = local.and_then(|p| self.pool_match(&p)) else {
            self.status = "No pool slot holds that slice's file".into();
            return;
        };
        self.reveal_slot(m.key);
    }

    /// The editor's file in the pool (the origin line's slot).
    pub(super) fn origin_in_pool(&mut self) {
        let Some(m) = self.browser().path().map(Path::to_path_buf).and_then(|p| self.pool_match(&p)) else {
            return;
        };
        self.reveal_slot(m.key);
    }

    fn reveal_slot(&mut self, key: (u32, u32)) {
        if !self.pool_in_col() && !self.elek_mode {
            self.dock_elek();
        }
        if !self.elek.ping_key(|k| k == key) {
            self.status = "No pool slot holds that file".into();
        }
    }

    pub(super) fn device_home(&self) -> Option<PathBuf> {
        let p = self.browser().path()?;
        match &self.device_origin {
            Some((local, dev)) if local == p => Some(dev.clone()),
            _ => self.links.for_local(p).map(|l| PathBuf::from(&l.device)),
        }
    }

    /// Open the "send to device as" question for the editor's sample.
    pub(super) fn begin_send_back(&mut self) {
        if self.showing_kit || self.editor.current.is_none() {
            return;
        }
        let (Some(home), Some(src)) = (self.device_home(), self.editor.path().map(Path::to_path_buf)) else {
            return;
        };
        let dir = home.parent().map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
        let from = home
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Beside the local original, unless it's one of our temp downloads.
        let local_dir = src
            .parent()
            .filter(|d| !d.starts_with(std::env::temp_dir()) && !d.starts_with("/private/var/folders"))
            .map(Path::to_path_buf);
        let free = |c: &str| {
            !self.device_has(&dir.join(c))
                && local_dir.as_ref().is_none_or(|d| !d.join(format!("{c}.wav")).exists())
        };
        let name = next_name(&from, free);
        self.send_back = Some(dialogs::SendBack {
            dir,
            from,
            name,
            keep_local: local_dir.is_some(),
            local_dir,
        });
    }

    /// Render the editor and upload it under the name picked.
    pub(super) fn start_send_back(&mut self, f: dialogs::SendBack) {
        let name = f.name.trim().to_owned();
        if name.is_empty() || self.editor.exporting {
            return;
        }
        let staged = match f.local_dir.filter(|_| f.keep_local) {
            Some(d) => d.join(format!("{name}.wav")),
            None => {
                let tmp = std::env::temp_dir().join("sample_manager_upload");
                if let Err(e) = std::fs::create_dir_all(&tmp) {
                    self.errors.push(format!("{}: {e}", tmp.display()));
                    return;
                }
                tmp.join(format!("{name}.wav"))
            }
        };
        self.status = format!("Rendering {name} for the device…");
        self.staged_upload = Some(StagedUpload {
            staged: staged.clone(),
            dest_dir: f.dir,
            // Never over an existing file: a taken name gets a number.
            policy: Policy::KeepBoth,
        });
        let mono = self.editor.fold();
        self.editor.export(staged, mono);
    }

    /// Is the file under `tab`'s cursor one to load: audio on disk, or a
    /// device file we have a local copy of. Cheap, no disk access.
    fn loadable(&self, tab: &crate::tab::DirTab, e: &fs_model::Entry) -> bool {
        if tab.device {
            !e.is_dir && self.links.known(e.hash, e.len as u32).is_some()
        } else {
            e.is_audio
        }
    }

    /// Load the file at `p` in the focused pane: a device file through its
    /// local copy.
    fn audition_row(&mut self, p: &Path, play: bool) {
        if !self.pane(self.focused).active_tab().device {
            self.audition(p, play);
        } else if let Some(local) = self.linked_local(p) {
            self.audition(&local, play);
        }
    }

    /// Ask before fetching a device file for the editor: it's slow. One we
    /// have a local copy of opens right away instead.
    pub(super) fn ask_device_load(&mut self, path: PathBuf) {
        if let Some(local) = self.linked_local(&path) {
            let note = std::mem::take(&mut self.status);
            self.load_into_editor(&local);
            self.status = note;
            return;
        }
        let bytes = self.device_entry(&path).map_or(0, |e| e.len);
        self.device_load_pending = Some((path, bytes));
    }

    /// Download a device file to a temp folder; it opens in the editor when done.
    pub(super) fn start_device_load(&mut self, src: PathBuf, bytes: u64) {
        let src_path = src.clone();
        // Mirror the device path, so same-named files in different folders
        // don't overwrite each other.
        let rel = src
            .parent()
            .and_then(|p| p.strip_prefix("/").ok())
            .unwrap_or(Path::new(""));
        let dir = std::env::temp_dir().join("sample_manager_device").join(rel);
        let name = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let local = dir.join(device::wav_name(&name));
        let task = device::Task::Transfer {
            dir: device::Dir::Download,
            items: vec![(src, dir)],
            policy: Policy::Replace,
            mono: None,
            total_bytes: bytes,
            local_names: Default::default(),
        };
        let id = self.run_device(task, &format!("Loading {name}"), 1, bytes);
        self.device_origin = Some((local.clone(), src_path));
        self.device_load = Some((id, local));
        self.status = format!("Fetching {name} from the device…");
    }

    /// Open a local file in the editor, re-reading it if it's already open
    /// (a fresh download over the same temp file).
    pub(super) fn load_into_editor(&mut self, path: &Path) {
        self.show_kit(false);
        let play = self.editor.prefs.auto_play;
        if self.editor.is_dirty() {
            self.leave_pending = Some(Leave::LoadEditor(path.to_path_buf()));
            return;
        }
        if self.editor.path() == Some(path) {
            if self.voice_path.as_deref() == Some(path) {
                self.engine.stop();
            }
            self.editor.play_when_loaded = play;
            self.editor.reload();
        } else {
            self.audition(path, play);
        }
    }
    /// `is_kit`: `editor` is the kit slot right now.
    pub(super) fn absorb_editor(&mut self, is_kit: bool) {
        let fade = std::time::Duration::from_secs_f32(crate::ui::file_table::SAVE_FLASH_SECS);
        self.saving
            .retain(|_, done| done.is_none_or(|at| at.elapsed() < fade));
        for o in self.editor.drain() {
            match o {
                Outcome::Loaded => {
                    // A different file starts at zero pitch; a reload of the
                    // same one (after an export, say) keeps it.
                    if self.editor.path() != self.pitch_path.as_deref() {
                        self.pitch_path = self.editor.path().map(Path::to_path_buf);
                        self.pitch = 0.0;
                    }
                    if is_kit && self.editor.path().is_some() {
                        let seed = self
                            .kit_seed
                            .take_if(|(seed, _)| self.editor.path() == Some(seed.as_path()));
                        if let Some((seed, new)) = seed {
                            // A plain sample seeded the kit: it's source
                            // material, the kit gets a file of its own.
                            self.editor.detach(new);
                            self.editor.note_source(Some(&seed));
                        } else if let Some(p) = self
                            .editor
                            .path()
                            .filter(|p| p.is_file() && !super::kit::is_collection(p))
                            .map(Path::to_path_buf)
                        {
                            // Opened as a kit some other way, but not a kit
                            // file of ours: never saved over either.
                            self.editor.note_source(Some(&p));
                        }
                    }
                    if let (true, Some((clip, grid))) = (is_kit, self.kit_pending.take()) {
                        let name = self
                            .editor
                            .path()
                            .and_then(|p| p.file_name())
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        // A grid was asked for and the file isn't on it yet.
                        // A file made the kit as it is keeps any slicing it has.
                        let keep = clip.is_none() && self.editor.has_markers();
                        let regrid = match grid {
                            Some(_) if !keep && self.editor.grid_mode() != grid => {
                                self.editor.set_grid(grid)
                            }
                            _ => Ok(()),
                        };
                        match clip {
                            Some(c) => {
                                self.status = match regrid.and_then(|()| self.editor.append(&c)) {
                                    Ok(()) => format!(
                                        "Added the slice to {name} (not saved yet) — B shows it, ⌘S there saves it"
                                    ),
                                    Err(e) => format!("{name}: {e}"),
                                };
                            }
                            None => {
                                if let Err(e) = regrid {
                                    self.status = format!("{name}: {e}");
                                }
                            }
                        }
                    }
                    if std::mem::take(&mut self.editor.play_when_loaded) {
                        self.start_play();
                    }
                }
                Outcome::Exported(p) if self.pool_edit_rendered(&p) => self.pool_edit_upload(p),
                Outcome::Exported(p) if self.slot_save_rendered(&p) => self.slot_save_upload(p),
                Outcome::Exported(p)
                    if self.staged_upload.as_ref().is_some_and(|s| s.staged == p) =>
                {
                    let s = self.staged_upload.take().expect("checked above");
                    // Sent back with a copy kept beside the original: show it.
                    if let Some(d) = s.staged.parent().filter(|d| !d.starts_with(std::env::temp_dir())) {
                        let d = d.to_path_buf();
                        self.reload_showing(&d, false);
                    }
                    let leave = self.leave_after_save.take();
                    let on_device = device::upload_target(&s.staged, &s.dest_dir);
                    let task = device::Task::Transfer {
                        dir: device::Dir::Upload,
                        items: vec![(s.staged, s.dest_dir)],
                        policy: s.policy,
                        mono: None,
                        total_bytes: std::fs::metadata(&p).map_or(0, |m| m.len()),
                        local_names: Default::default(),
                    };
                    self.status = "Sending to the device…".to_owned();
                    let job = self.run_device(task, "", 1, 0);
                    self.reveal_after = Some((job, on_device));
                    if let Some(l) = leave {
                        self.finish_leave(l);
                    }
                }
                Outcome::Exported(p) => {
                    self.saving
                        .insert(p.clone(), Some(std::time::Instant::now()));
                    self.status = format!(
                        "Exported {}",
                        p.file_name().unwrap_or_default().to_string_lossy()
                    );
                    // An export gets its name typed right away (the default
                    // is only a start). Not while closing or quitting.
                    let rename = self.export_rename.take_if(|e| *e == p).is_some()
                        && self.leave_after_save.is_none();
                    // Replaced the file that's open: show what's on disk now.
                    if self.editor.path() == Some(p.as_path()) {
                        if self.voice_path.as_deref() == Some(p.as_path()) {
                            self.engine.stop();
                        }
                        self.editor.reload_saved();
                    }
                    self.reveal_when_listed(&p, false);
                    if let Some(dir) = p.parent() {
                        self.sizes.invalidate_subtree(dir);
                        self.reload_showing(dir, false);
                    }
                    if rename {
                        self.status += " — type a name for it, ↩ to rename";
                        self.rename_kept(vec![p.clone()]);
                    }
                    if let Some(l) = self.leave_after_save.take() {
                        self.finish_leave(l);
                    }
                }
                Outcome::BatchExported(dir, n) => {
                    self.status = format!("Folded {n} file(s) to mono");
                    self.sizes.invalidate_subtree(&dir);
                    self.reload_showing(&dir, false);
                }
                Outcome::Error(e) => {
                    if let Some(pe) = self.pool_edit.as_mut() {
                        pe.cancel_render();
                    }
                    self.slot_save_cancel();
                    // A failed render never reaches the upload, and the
                    // edits it was saving are still unsaved: stay put.
                    self.staged_upload = None;
                    self.export_rename = None;
                    self.leave_after_save = None;
                    self.saving.retain(|_, done| done.is_some());
                    self.errors.push(e);
                }
            }
        }
    }

    pub(super) fn show_editor(&mut self, ui: &mut egui::Ui) {
        let pv = PlayView {
            playing: self.playing_current(),
            pos: self.engine.position(),
        };
        let can_export = !self.pane(Role::Dest).active_tab().loading;
        let slots = editor_view::Slots {
            drive_shown: self.drive_in_col(),
            showing_kit: self.showing_kit,
            kit_name: self.kit_name(),
            kit_dirty: self.kit().is_dirty(),
            kit_grid: self.kit().grid_mode(),
            kit_unlocked: !self.kit().markers_locked,
            kit_fill: (self.kit().kit_fill(), self.kit().grid),
            kits: self.kit_list(),
            kit_on_disk: self.kit().path().is_some_and(Path::is_file) && !self.kit().path_is_source(),
            sample_name: self
                .browser()
                .path()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned()),
            device_home: self.device_home().map(|p| device::dev_str(&p)),
            pool_slot: self.pool_edit_label(),
            pool: self.pool_targets(),
            export_drive: {
                let tab = self.pane(Role::Dest).active_tab();
                tab.device.then(|| device::dev_str(&tab.cwd))
            },
            origin: self.editor_origin(),
            // Only while a slice's right-click menu is open.
            slice_homes: if self.showing_kit && self.editor.slice_menu.is_some() { self.slice_homes() } else { Vec::new() },
        };
        let intents = editor_view::show(
            ui,
            &mut self.editor,
            pv,
            self.editor_focused,
            can_export,
            &slots,
        );
        for i in intents {
            match i {
                EdIntent::TookFocus => self.editor_focused = true,
                EdIntent::TogglePlay => {
                    self.editor_focused = true;
                    self.toggle_play();
                }
                EdIntent::Seek(f) => self.engine.seek(f),
                EdIntent::Export => self.export(),
                EdIntent::ExportAndClose => {
                    // Same path as "export" in the close-kit question: the
                    // kit closes once the file is written, and stays open if
                    // the export is cancelled or fails.
                    if self.leave_pending.is_none() && self.leave_after_save.is_none() {
                        self.leave_after_save = Some(Leave::CloseKit);
                        self.export();
                    }
                }
                EdIntent::SaveInPlace => self.save_in_place(),
                EdIntent::ShowKit(on) => self.show_kit(on),
                EdIntent::AddSlice => self.add_slice(),
                EdIntent::UseAsKit => {
                    if let Some(p) = self.browser().path().map(Path::to_path_buf) {
                        self.use_as_kit(&p);
                        // The sample is the kit's first slice: check it like
                        // samples added with K.
                        let k = self.kit();
                        if k.path() == Some(p.as_path()) || k.loading.as_deref() == Some(p.as_path()) {
                            self.mark_committed([p]);
                        }
                    }
                }
                EdIntent::SwitchKit(i) => self.switch_kit(i),
                EdIntent::NewKit => self.new_kit(),
                EdIntent::ToggleDevice => self.toggle_device(),
                EdIntent::ShowDrive => self.show_drive(),
                EdIntent::DevicePage(page) => {
                    self.elek.page = page;
                    self.enter_full_elek();
                }
                EdIntent::TrimTails => {
                    let rate = self.editor.sample().map_or(1, |s| s.rate.max(1)) as f64;
                    self.status = match self.editor.trim_tails() {
                        Some((was, now, fades)) => format!(
                            "Trimmed: {:.2} s → {:.2} s, freed {:.2} s, {fades} fade{} — ⌘Z undoes",
                            was as f64 / rate,
                            now as f64 / rate,
                            was.saturating_sub(now) as f64 / rate,
                            if fades == 1 { "" } else { "s" }
                        ),
                        None => "Nothing to trim".to_owned(),
                    };
                }
                EdIntent::CloseKit => self.close_kit(),
                EdIntent::CloseAllKits(keep) => self.close_all_kits(keep),
                EdIntent::SetGrid(n) => self.set_kit_grid(n),
                EdIntent::RenameKit(name) => self.rename_kit(&name),
                EdIntent::SendToDevice => self.begin_send_back(),
                EdIntent::SaveToSlot(pick) => self.save_to_slot(pick),
                EdIntent::SaveOverOriginal => self.ask_overwrite_original(),
                EdIntent::ShowInSource(p) => self.show_in_source(&p),
                EdIntent::SliceInPool(i) => self.slice_in_pool(i),
                EdIntent::OriginInPool => self.origin_in_pool(),
            }
        }
    }
}

/// Export found its file name taken and is waiting on a new one.
pub(super) struct ExportNaming {
    pub dir: PathBuf,
    /// Without the extension.
    pub name: String,
    /// The name that was taken, for the message.
    pub taken: String,
    pub mono: Option<Fold>,
}

/// `[n] name.wav`, replacing a slot count already at the start of the name.
fn with_slot_count(path: &Path, n: usize) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let base = crate::editor::slot_prefix(&stem).map_or(stem.trim_start(), |(_, rest)| rest);
    path.with_file_name(format!("[{n}] {base}.wav"))
}

/// Status line for opening a device file through its local copy.
pub(super) fn link_note(l: &crate::links::Link) -> String {
    let name = l.local.file_name().unwrap_or_default().to_string_lossy();
    if l.uploaded {
        let fold = if l.folded {
            " (the device has it folded to mono)"
        } else {
            ""
        };
        format!("{name}: opened the local file it was uploaded from{fold}, no fetch needed")
    } else {
        format!("{name}: opened the copy fetched from the device earlier")
    }
}

/// `name 2`, or one past a number already at the end (`kick 3` → `kick 4`),
/// the first that `free` accepts.
pub(super) fn next_name(name: &str, free: impl Fn(&str) -> bool) -> String {
    let (base, start) = match name.rsplit_once(' ') {
        Some((b, n)) if !b.is_empty() && n.parse::<u32>().is_ok() => (b, n.parse::<u32>().unwrap_or(1) + 1),
        _ => (name, 2),
    };
    (start..start + 10_000)
        .map(|n| format!("{base} {n}"))
        .find(|c| free(c))
        .unwrap_or_else(|| format!("{base} edit"))
}
