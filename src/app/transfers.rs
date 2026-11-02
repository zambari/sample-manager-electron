//! Directory listing, copy/move/commit planning, device jobs and absorbing worker events.

use super::*;

impl Batch {
    pub(super) fn absorb(&mut self, a: &ActiveJob, cancelled: bool) {
        // A finished job counts in full; a cancelled one only for what it did.
        let items = if cancelled { a.done_items } else { a.total_items.max(a.done_items) };
        let bytes = if cancelled { a.done_bytes } else { a.total_bytes.max(a.done_bytes) };
        self.done_items += items;
        self.total_items += items;
        self.done_bytes += bytes;
        self.total_bytes += bytes;
        self.bytes_unknown |= a.total_bytes == 0;
        self.file_bar |= a.file_bar;
    }
}

impl App {
    // ---- requests ---------------------------------------------------------

    pub(super) fn reload(&mut self, role: Role, tab_idx: usize) {
        let show_hidden = self.show_hidden;
        let pane = self.pane_mut(role);
        if let Some(tab) = pane.tabs.get_mut(tab_idx) {
            let generation = tab.begin_reload();
            let device = tab.device;
            let mut requests = vec![(tab.cwd.clone(), generation)];
            // Expanded tree folders are part of what's shown; refresh them too.
            for dir in tab.expanded_dirs() {
                let g = tab.begin_child_load(&dir);
                requests.push((dir, g));
            }
            for (dir, g) in requests {
                self.list(device, dir, g, show_hidden);
            }
        }
    }

    pub(super) fn list(&self, device: bool, dir: PathBuf, generation: u64, show_hidden: bool) {
        if device {
            self.device.list(dir, generation);
        } else {
            self.jobs.list(dir, generation, show_hidden);
        }
    }

    /// Expand a folder in the active tab's tree and fetch its contents.
    pub(super) fn expand(&mut self, role: Role, dir: PathBuf) {
        let show_hidden = self.show_hidden;
        let tab = self.pane_mut(role).active_tab_mut();
        if tab.expand(&dir) {
            let g = tab.begin_child_load(&dir);
            let device = tab.device;
            self.list(device, dir, g, show_hidden);
        }
    }

    pub(super) fn reload_all(&mut self) {
        for role in [Role::Source, Role::Dest] {
            for i in 0..self.pane(role).tabs.len() {
                self.reload(role, i);
            }
        }
    }

    /// Re-list every local (non-device) tab.
    pub(super) fn reload_local(&mut self) {
        for role in [Role::Source, Role::Dest] {
            for i in 0..self.pane(role).tabs.len() {
                if !self.pane(role).tabs[i].device {
                    self.reload(role, i);
                }
            }
        }
    }

    /// Re-list any tab showing a directory at or under `dir`, on the disk or
    /// (`device`) on the device.
    pub(super) fn reload_showing(&mut self, dir: &Path, device: bool) {
        let mut todo = Vec::new();
        for role in [Role::Source, Role::Dest] {
            for (i, tab) in self.pane(role).tabs.iter().enumerate() {
                if tab.device == device && (tab.cwd == dir || dir.starts_with(&tab.cwd)) {
                    todo.push((role, i));
                }
            }
        }
        for (role, i) in todo {
            self.reload(role, i);
        }
    }

    pub(super) fn mark_committed(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        for p in paths {
            self.committed_dirty |= self.committed.insert(p);
        }
    }

    pub(super) fn request_sizes(&mut self, paths: Vec<PathBuf>) {
        for p in paths {
            if p.is_dir() && self.sizes.request(&p) {
                self.jobs.dir_size(p);
            }
        }
    }

    // ---- the core verb ----------------------------------------------------

    /// Commit the source pane's selection into the active destination tab.
    ///
    /// Deliberately directional rather than Commander's "active pane -> other
    /// pane": the panes have fixed roles here. To get the Commander behaviour,
    /// swap the two role arguments for `(self.focused, self.focused.other())`.
    pub(super) fn commit(&mut self, kind: Kind) {
        let (src_tab, dst_tab) = (self.pane(Role::Source).active_tab(), self.pane(Role::Dest).active_tab());
        if src_tab.device || dst_tab.device {
            let (from_dev, to_dev) = (src_tab.device, dst_tab.device);
            let sources = src_tab.effective_selection();
            let dest_dir = dst_tab.cwd.clone();
            // The editor's file as you hear it, like a local commit.
            let editor_file = !from_dev && kind == Kind::Copy && self.editor.current.is_some();
            let as_heard = match sources.as_slice() {
                [] => editor_file,
                [only] => {
                    editor_file
                        && self.editor.path() == Some(only.as_path())
                        && (self.editor.is_edited() || self.editor.exports_mono())
                }
                _ => false,
            };
            if as_heard {
                self.export();
            } else if sources.is_empty() {
                self.status = "Nothing selected in the source pane".to_owned();
            } else {
                self.transfer(kind, from_dev, sources, to_dev, dest_dir);
            }
            return;
        }
        let sources = self.pane(Role::Source).active_tab().effective_selection();
        // Nothing selected (e.g. right after launch, with a file restored in
        // the editor): commit the editor's file as you hear it.
        if sources.is_empty() && kind == Kind::Copy && self.editor.current.is_some() {
            self.export();
            return;
        }
        if sources.is_empty() {
            self.status = "Nothing selected in the source pane".to_owned();
            return;
        }
        let dest_dir = self.pane(Role::Dest).active_tab().cwd.clone();
        if kind == Kind::Move {
            self.start_transfer(kind, sources, dest_dir);
            return;
        }

        // A single file that's open in the editor goes out as you hear it:
        // trimmed, with gain, and folded to mono if that's set.
        if let [only] = sources.as_slice() {
            if self.editor.path() == Some(only.as_path())
                && (self.editor.is_edited() || self.editor.exports_mono())
            {
                self.export();
                return;
            }
        }

        // Otherwise trim and gain don't apply; files are copied as they are,
        // except multichannel audio when "save mono" is on, which gets folded.
        let (fold, copy): (Vec<PathBuf>, Vec<PathBuf>) = if self.editor.prefs.mono.is_some() {
            sources.into_iter().partition(|p| {
                p.is_file()
                    && crate::fs_model::is_audio_path(p)
                    && crate::audio::decode::channel_count(p).is_some_and(|n| n > 1)
            })
        } else {
            (Vec::new(), sources)
        };
        // Copies and folds go into one plan so existing files are asked about
        // once, in one popup.
        let mut plan = ops::plan(kind, &copy, &dest_dir, |p| self.sizes.bytes_of(p));
        for src in &fold {
            let dst = crate::audio::export::target_for(src, &dest_dir);
            plan.ops.push(ops::Op {
                conflict: dst.exists(),
                approx_bytes: std::fs::metadata(src).map(|m| m.len()).unwrap_or(0),
                src: src.clone(),
                dst,
                is_dir: false,
            });
        }
        let what = PlanKind::Commit { fold: fold.into_iter().collect(), dest_dir };
        self.offer(plan, what);
    }

    pub(super) fn start_transfer(&mut self, kind: Kind, sources: Vec<PathBuf>, dest_dir: PathBuf) {
        let plan = ops::plan(kind, &sources, &dest_dir, |p| self.sizes.bytes_of(p));
        self.offer(plan, PlanKind::Transfer);
    }

    /// Copy or move between any two places, either of which may be the
    /// device.
    pub(super) fn transfer(&mut self, kind: Kind, from_dev: bool, sources: Vec<PathBuf>, to_dev: bool, dest_dir: PathBuf) {
        let dir = match (from_dev, to_dev) {
            (false, false) => return self.start_transfer(kind, sources, dest_dir),
            (true, true) if kind == Kind::Move => device::Dir::Move,
            (true, true) => device::Dir::Copy,
            (false, true) => device::Dir::Upload,
            (true, false) => device::Dir::Download,
        };
        let mut plan = Plan { kind, ops: Vec::new(), rejected: Vec::new() };
        for src in sources {
            let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let (is_dir, bytes) = if from_dev {
                self.device_entry(&src).map_or((false, 0), |e| (e.is_dir, e.len))
            } else {
                let dir = src.is_dir();
                let bytes = if dir {
                    self.sizes.bytes_of(&src).unwrap_or(0)
                } else {
                    std::fs::metadata(&src).map_or(0, |m| m.len())
                };
                (dir, bytes)
            };
            if from_dev == to_dev && is_dir && dest_dir.starts_with(&src) {
                plan.rejected.push((src, "cannot put a folder inside itself".to_owned()));
                continue;
            }
            // Also catches a drag let go over its own list.
            if from_dev == to_dev && src.parent() == Some(dest_dir.as_path()) {
                plan.rejected.push((src, "already in that folder".to_owned()));
                continue;
            }
            if dir == device::Dir::Upload && !is_dir && !fs_model::is_audio_path(&src) {
                plan.rejected.push((src, "not an audio file".to_owned()));
                continue;
            }
            let dst = match dir {
                device::Dir::Upload if !is_dir => device::upload_target(&src, &dest_dir),
                device::Dir::Download if !is_dir => dest_dir.join(device::wav_name(&name)),
                _ => dest_dir.join(&name),
            };
            let conflict = if to_dev { self.device_has(&dst) } else { dst.exists() };
            plan.ops.push(ops::Op { src, dst, approx_bytes: bytes, is_dir, conflict });
        }
        if kind == Kind::Move && dir != device::Dir::Move {
            self.status = "Between the computer and the device, Move copies; originals are kept".to_owned();
        }
        let mono = if dir == device::Dir::Upload { self.editor.prefs.mono } else { None };
        self.offer(plan, PlanKind::Device { dir, mono });
    }

    /// A device item as last listed by any device tab.
    pub(super) fn device_entry(&self, path: &Path) -> Option<fs_model::Entry> {
        let parent = path.parent()?;
        [&self.source, &self.dest]
            .into_iter()
            .flat_map(|p| p.tabs.iter())
            .filter(|t| t.device)
            .find_map(|t| t.listing_of(parent)?.iter().find(|e| e.path == path).cloned())
    }

    pub(super) fn device_has(&self, path: &Path) -> bool {
        self.device_entry(path).is_some()
    }

    /// Hand `task` to the device worker as a job with a progress bar.
    pub(super) fn run_device(&mut self, task: device::Task, label: &str, total_items: usize, total_bytes: u64) -> u64 {
        let (id, cancel) = self.jobs.register();
        self.device.run(id, cancel, task);
        self.active_jobs.push(ActiveJob {
            id,
            label: label.to_owned(),
            done_bytes: 0,
            total_bytes,
            done_items: 0,
            total_items,
            started: false,
            file: None,
            file_bar: false,
        });
        id
    }

    /// Run `plan` now, or hold it and ask first if anything already exists.
    pub(super) fn offer(&mut self, mut plan: Plan, what: PlanKind) {
        for (path, why) in &plan.rejected {
            self.errors.push(format!("{}: {why}", path.display()));
        }
        if plan.ops.is_empty() {
            self.status = "Nothing to transfer".to_owned();
            return;
        }
        // A local copy whose destination already holds the same bytes has
        // nothing to do: drop it rather than asking about it. Mono folds are
        // rendered, so their output can't be compared up front.
        let same_ok = plan.kind == Kind::Copy && matches!(what, PlanKind::Transfer | PlanKind::Commit { .. });
        if same_ok {
            let fold = match &what {
                PlanKind::Commit { fold, .. } => Some(fold),
                _ => None,
            };
            let (same, keep): (Vec<ops::Op>, Vec<ops::Op>) = std::mem::take(&mut plan.ops).into_iter().partition(|o| {
                o.conflict && !fold.is_some_and(|f| f.contains(&o.src)) && ops::already_there(&o.src, &o.dst)
            });
            plan.ops = keep;
            if !same.is_empty() {
                let n = same.len();
                self.mark_committed(same.into_iter().map(|o| o.src));
                self.status = if n == 1 {
                    "Already at the destination (identical) — skipped".to_owned()
                } else {
                    format!("{n} items already at the destination (identical) — skipped")
                };
                if plan.ops.is_empty() {
                    return;
                }
            }
        }
        if plan.conflicts() > 0 {
            // Hold the plan and let the modal decide. No bytes move until then.
            self.pending = Some(PendingPlan { plan, what });
            return;
        }
        self.launch(&plan, what, Policy::KeepBoth);
    }

    pub(super) fn launch(&mut self, plan: &Plan, what: PlanKind, policy: Policy) {
        match what {
            PlanKind::Device { dir, mono } => {
                let items: Vec<(PathBuf, PathBuf)> = plan
                    .ops
                    .iter()
                    .filter(|o| !(o.conflict && policy == Policy::Skip))
                    .filter_map(|o| Some((o.src.clone(), o.dst.parent()?.to_path_buf())))
                    .collect();
                if items.is_empty() {
                    self.status = "All items skipped".to_owned();
                    return;
                }
                if dir == device::Dir::Upload {
                    self.mark_committed(items.iter().map(|(s, _)| s.clone()));
                }
                let total_bytes = plan.total_bytes();
                let n = items.len();
                self.run_device(device::Task::Transfer { dir, items, policy, mono, total_bytes }, "", n, total_bytes);
                return;
            }
            PlanKind::DeviceExport { mono } => {
                if policy == Policy::Skip && plan.conflicts() > 0 {
                    self.status = "Export skipped".to_owned();
                    return;
                }
                let (Some(op), Some(src)) = (plan.ops.first(), self.editor.path().map(Path::to_path_buf)) else { return };
                let Some(dest_dir) = op.dst.parent().map(Path::to_path_buf) else { return };
                let tmp = std::env::temp_dir().join("sample_manager_upload");
                if let Err(e) = std::fs::create_dir_all(&tmp) {
                    self.errors.push(format!("{}: {e}", tmp.display()));
                    return;
                }
                let staged = crate::audio::export::target_for(&src, &tmp);
                self.mark_committed([src]);
                self.status = "Rendering for the device…".to_owned();
                self.staged_upload = Some(StagedUpload { staged: staged.clone(), dest_dir, policy });
                self.editor.export(staged, mono);
                return;
            }
            _ => {}
        }
        let mut resolved = ops::resolve(plan, policy);
        // "Keep both" copies get their name picked by the user once they exist.
        let mut kept: Vec<PathBuf> = if policy == Policy::KeepBoth {
            plan.ops.iter().zip(&resolved).filter(|(o, _)| o.conflict).map(|(_, r)| r.dst.clone()).collect()
        } else {
            Vec::new()
        };
        if resolved.is_empty() {
            self.status = "All items skipped".to_owned();
            return;
        }
        match what {
            PlanKind::Transfer | PlanKind::Device { .. } | PlanKind::DeviceExport { .. } => {}
            PlanKind::Export { mono } => {
                let op = resolved.remove(0);
                if let Some(p) = self.editor.path().map(Path::to_path_buf) {
                    self.mark_committed([p]);
                }
                self.status = format!("Exporting{} to {}…", mono.map_or(String::new(), |f| format!(" (mono {})", f.short())), op.dst.display());
                self.editor.export(op.dst, mono);
                return;
            }
            PlanKind::Commit { fold, dest_dir } => {
                let (folds, copies): (Vec<_>, Vec<_>) = resolved.into_iter().partition(|o| fold.contains(&o.src));
                resolved = copies;
                if !folds.is_empty() {
                    self.mark_committed(folds.iter().map(|o| o.src.clone()));
                    self.status = format!("Folding {} file(s) to mono…", folds.len());
                    self.editor.export_mono_batch(folds.into_iter().map(|o| (o.src, o.dst)).collect(), dest_dir, self.editor.prefs.mono.unwrap_or_default());
                }
                if resolved.is_empty() {
                    return;
                }
            }
        }
        if plan.kind == Kind::Copy {
            self.mark_committed(resolved.iter().map(|o| o.src.clone()));
        }
        kept.retain(|p| resolved.iter().any(|o| &o.dst == p));
        let id = self.run_local(plan.kind, resolved);
        let undo = if plan.kind == Kind::Move { undo::UndoJob::Move } else { undo::UndoJob::Copy };
        self.undo_jobs.insert(id, undo);
        if !kept.is_empty() {
            self.rename_after = Some((id, kept));
        }
    }

    /// Merge conflicting folders into the existing ones: add what's new,
    /// treat same name + size as a duplicate, leave other clashes in place.
    pub(super) fn launch_merge(&mut self, plan: &Plan, what: PlanKind) {
        let mut plan = plan.clone();
        // Mono folds are rendered rather than copied, so they can't be
        // compared: they go the normal way, skipping any that exist.
        if let PlanKind::Commit { fold, dest_dir } = what {
            let (folds, rest): (Vec<_>, Vec<_>) = plan.ops.into_iter().partition(|o| fold.contains(&o.src));
            plan.ops = rest;
            if !folds.is_empty() {
                let fp = Plan { kind: plan.kind, ops: folds, rejected: Vec::new() };
                self.launch(&fp, PlanKind::Commit { fold, dest_dir }, Policy::Skip);
            }
        }
        let m = ops::merge(&plan, |_| true);
        let moving = plan.kind == Kind::Move;
        for c in &m.collisions {
            crate::log::line(format!("merge: left in place (same name, different size): {}", c.display()));
        }
        let mut note = format!("Merged: {} added", m.ops.len());
        if !m.dupes.is_empty() {
            let fate = if moving { "moved to the Trash" } else { "skipped" };
            note += &format!(", {} duplicate(s) {fate}", m.dupes.len());
        }
        if !m.collisions.is_empty() {
            let place = if moving { "left in the source folder" } else { "not copied" };
            note += &format!(", {} with the same name but a different size {place}", m.collisions.len());
        }
        if !moving {
            self.mark_committed(m.ops.iter().map(|o| o.src.clone()).chain(m.dupes.iter().cloned()));
        }
        if m.ops.is_empty() && (!moving || m.dupes.is_empty()) {
            self.status = note;
            return;
        }
        let tidy = moving.then(|| crate::jobs::Tidy { dupes: m.dupes, roots: m.roots });
        let id = self.run_local_then(plan.kind, m.ops, tidy);
        let undo = if moving { undo::UndoJob::Move } else { undo::UndoJob::Copy };
        self.undo_jobs.insert(id, undo);
        self.finish_note = Some((id, note));
    }

    /// Select the "Keep both" copies a finished job made and, for a single
    /// one, open the inline rename on it, in a pane showing its folder.
    fn rename_kept(&mut self, paths: Vec<PathBuf>) {
        let paths: Vec<PathBuf> = paths.into_iter().filter(|p| std::fs::symlink_metadata(p).is_ok()).collect();
        let Some(first) = paths.first() else { return };
        let Some(dir) = first.parent().map(Path::to_path_buf) else { return };
        let Some(role) = [self.focused, self.focused.other()].into_iter().find(|&r| {
            let t = self.pane(r).active_tab();
            !t.device && (t.cwd == dir || t.expanded.contains(&dir))
        }) else {
            return;
        };
        self.focused = role;
        self.editor_focused = false;
        let tab = self.pane_mut(role).active_tab_mut();
        tab.multi_rename = None;
        tab.selected = paths.iter().cloned().collect();
        tab.cursor = None;
        if let [only] = paths.as_slice() {
            let name = only.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            tab.renaming = Some((only.clone(), name));
        } else {
            self.status += " · new copies selected — ⇧F2 renames them together";
        }
    }

    // ---- worker results ---------------------------------------------------

    pub(super) fn absorb(&mut self, events: Vec<Ev>) {
        for ev in events {
            match ev {
                Ev::Listed { device, generation, path, result } => {
                    for role in [Role::Source, Role::Dest] {
                        for tab in self.pane_mut(role).tabs.iter_mut().filter(|t| t.device == device) {
                            if tab.cwd == path {
                                tab.accept_listing(generation, result.clone());
                            } else if tab.expanded.contains(&path) {
                                tab.accept_child_listing(&path, generation, result.clone());
                            }
                        }
                    }
                }
                Ev::Duplicates { groups, errors } => {
                    self.errors.extend(errors);
                    if groups.is_empty() {
                        self.status = "No duplicates found".to_owned();
                    } else {
                        self.status = String::new();
                        self.dupes_pending = Some(groups);
                    }
                }
                Ev::Applied { job, done, replaced } => self.applied(job, done, replaced),
                Ev::Note { job, text } => self.finish_note = Some((job, text)),
                Ev::DeviceStatus(s) => {
                    if let Err(e) = &s {
                        crate::log::line(format!("device status: {e}"));
                    }
                    self.device_status = Some(s)
                }
                Ev::DeviceLost => {
                    for role in [Role::Source, Role::Dest] {
                        if !self.pane_mut(role).leave_device() {
                            let id = self.new_tab_id();
                            self.pane_mut(role).add_tab(DirTab::new(id, dirs_home()));
                            let i = self.pane(role).active;
                            self.reload(role, i);
                        }
                    }
                }
                Ev::DirSize { path, bytes, files } => {
                    self.sizes.set(path, bytes, files);
                }
                Ev::Progress {
                    job,
                    done_bytes,
                    total_bytes,
                    done_items,
                    total_items,
                    label,
                    file,
                } => {
                    if let Some(a) = self.active_jobs.iter_mut().find(|a| a.id == job) {
                        a.started = true;
                        a.file = file;
                        a.file_bar |= file.is_some();
                        a.done_bytes = done_bytes;
                        a.total_bytes = total_bytes;
                        a.done_items = done_items;
                        a.total_items = total_items;
                        if !label.is_empty() {
                            a.label = label;
                        }
                    }
                }
                Ev::Counted { job, items, bytes } => {
                    if let Some(a) = self.active_jobs.iter_mut().find(|a| a.id == job) {
                        a.total_items = items;
                        if let Some(b) = bytes {
                            a.total_bytes = b;
                        }
                    }
                }
                Ev::Finished {
                    job,
                    touched,
                    device_touched,
                    errors,
                    cancelled,
                } => {
                    self.jobs.forget(job);
                    self.undo_jobs.remove(&job);
                    if let Some(a) = self.active_jobs.iter().find(|a| a.id == job) {
                        self.batch.absorb(a, cancelled);
                    }
                    self.active_jobs.retain(|a| a.id != job);
                    if self.active_jobs.is_empty() {
                        self.batch = Batch::default();
                    }
                    let load = self.device_load.take_if(|(id, _)| *id == job).map(|(_, p)| p);
                    let load = load.filter(|p| !cancelled && errors.is_empty() && p.is_file());
                    self.status = if cancelled {
                        "Cancelled".to_owned()
                    } else if errors.is_empty() {
                        "Done".to_owned()
                    } else {
                        format!("Finished with {} error(s)", errors.len())
                    };
                    if !self.active_jobs.is_empty() {
                        self.status += &format!(" · {} more job(s) to go", self.active_jobs.len());
                    }
                    self.errors.extend(errors);
                    for dir in touched {
                        // A transfer changes sizes up the whole chain, so the
                        // cached values above it are now wrong too.
                        self.sizes.invalidate_subtree(&dir);
                        for anc in dir.ancestors() {
                            self.sizes.invalidate_subtree(anc);
                            break;
                        }
                        self.reload_showing(&dir, false);
                    }
                    for dir in device_touched {
                        self.reload_showing(&dir, true);
                    }
                    if let Some(p) = load {
                        self.load_into_editor(&p);
                    }
                    if let Some((_, note)) = self.finish_note.take_if(|(id, _)| *id == job) {
                        if !cancelled && self.status.starts_with("Done") {
                            self.status = self.status.replacen("Done", &note, 1);
                        }
                    }
                    if let Some((_, kept)) = self.rename_after.take_if(|(id, _)| *id == job) {
                        if !cancelled {
                            self.rename_kept(kept);
                        }
                    }
                }
            }
        }
    }
}
