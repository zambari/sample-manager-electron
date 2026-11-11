//! The Elek Sample Order mode: device requests and their answers.

use super::*;
use crate::device::{ElekCmd, ElekEv};
use crate::ui::elek_view::{self, ElekIntent};
use crate::device::file;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

type Key = (u32, u32);

/// Pool files being copied into the preview cache, one device job at a time
/// so a clicked slot (put at the front) never waits behind the whole pool.
#[derive(Default)]
pub(super) struct PoolFetch {
    queue: VecDeque<Key>,
    /// The download job on its way, and which file.
    running: Option<(u64, Key)>,
    /// Play (`false`) or open (`true`) this file once it lands.
    then: Option<(Key, bool)>,
    /// Background run: files done, and total asked for.
    done: usize,
    total: usize,
    /// Failed in a row: a moved file is skipped, but a second failure
    /// (likely unplugged) drops the queue.
    fails: usize,
    /// Uploads of files dropped onto the pool: job → `(slot as read, file)`.
    pub(super) uploads: std::collections::HashMap<u64, Vec<(u16, PathBuf)>>,
    /// Slots being looked up from an earlier upload's link: if the device
    /// no longer has that file, upload it again.
    prior: std::collections::HashMap<u16, PathBuf>,
    /// A bundle waiting for its samples to land in the preview cache.
    bundle: Option<BundleSave>,
    /// A loaded file's samples being placed into the pool.
    import: Option<Import>,
}

/// A project or pattern being saved with its samples.
pub(super) struct BundleSave {
    file: file::SaveFile,
    /// The bundle's folder, and the JSON's name in it.
    dir: PathBuf,
    json: String,
    /// "Project", or the pattern's slot label, for messages.
    what: String,
    /// Samples still being fetched from the device.
    waiting: HashSet<Key>,
}

/// Samples from a loaded file being placed: each is looked up on the device
/// by hash, else its bundled WAV is uploaded and looked up again.
#[derive(Default)]
pub(super) struct Import {
    /// The load's own message, summed up with how placing went.
    what: String,
    /// By slot as read.
    slots: HashMap<u16, ImportSlot>,
    found: usize,
    uploaded: usize,
    missing: Vec<String>,
    failed: Vec<String>,
}

struct ImportSlot {
    name: String,
    /// The file's key in the file.
    key: Key,
    /// Its bundled WAV.
    file: Option<PathBuf>,
    /// Where on the +Drive an upload goes.
    dir: PathBuf,
    stage: Stage,
    /// The slot already points at the file (a loaded project): only check.
    verify: bool,
}

#[derive(PartialEq)]
enum Stage {
    /// Looking up the file's own hash.
    Hash,
    /// Looking up the hash an earlier upload of the bundled WAV got.
    Link,
    /// Uploaded, looking up what landed.
    Upload,
}

impl App {
    /// A device page in the centre. Samples is the pool itself, so the
    /// right column shows the files meanwhile; Patterns keeps the docked
    /// pool beside it.
    pub(super) fn enter_full_elek(&mut self) {
        if self.elek.page == elek_view::ElekPage::Order {
            self.dock_pool = false;
        }
        self.pool_focused = false;
        self.enter_elek();
    }

    pub(super) fn enter_elek(&mut self) {
        // Leaving the editor: don't keep its loop running behind the device view.
        if !self.elek_mode {
            self.engine.stop();
        }
        self.elek_mode = true;
        self.editor_focused = false;
        self.measure_cache();
        if !self.elek.asked_projects {
            self.elek.asked_projects = true;
            self.device.elek(ElekCmd::ListProjects);
        }
        self.greet_device();
    }

    /// First visit to the device view this run: read the project in the
    /// device's memory without waiting for a click, and once its sample
    /// names are in, show the docked layout (see [`Self::first_dock_now`]).
    fn greet_device(&mut self) {
        if self.elek_greeted {
            return;
        }
        let present = matches!(crate::device::presence(), crate::device::Presence::One(_));
        if !present {
            return;
        }
        self.elek_greeted = true;
        if self.elek.project.is_some() || self.elek.busy.is_some() || self.elek.reading.is_some() {
            return;
        }
        self.first_dock = true;
        self.elek_read();
    }

    /// The first read has its sample names: Patterns in the centre (if
    /// the user is still on a device page), the pool docked on the right.
    fn first_dock_now(&mut self) {
        self.first_dock = false;
        self.elek_docked = true;
        self.dock_pool = true;
        self.pool_focused = false;
        // The docked pool lives in the right column, which this hides.
        self.elek.hide_files = false;
        if self.elek_mode {
            self.elek.page = elek_view::ElekPage::Project;
        }
        self.status = "Project loaded: patterns in the middle, the sample pool docked on the right. \
            Switch the right column with Files / Sample pool, or × to undock."
            .into();
    }

    /// Whether the right column shows the dock (Files / Sample pool switch).
    pub(super) fn dock_col_shown(&self) -> bool {
        self.elek_docked && (!self.elek_mode || self.elek.page == elek_view::ElekPage::Project)
    }

    /// Optional home of the pool: docked in the right column, beside the
    /// files and the editor. (The Device button opens the center view.)
    pub(super) fn dock_elek(&mut self) {
        self.elek_mode = false;
        self.elek_docked = true;
        self.dock_pool = true;
        self.pool_focused = true;
        self.editor_focused = false;
        self.measure_cache();
        if !self.elek.asked_projects {
            self.elek.asked_projects = true;
            self.device.elek(ElekCmd::ListProjects);
        }
    }

    pub(super) fn show_elek(&mut self, ui: &mut egui::Ui) {
        self.elek.docked = false;
        // The pool docked beside the Patterns page takes the keys once clicked.
        self.elek.keys_on = !(self.dock_col_shown() && self.dock_pool && self.pool_focused);
        let intents = match self.elek.page {
            elek_view::ElekPage::Order => elek_view::show(ui, &mut self.elek),
            elek_view::ElekPage::Project => crate::ui::elek_project_view::show(ui, &mut self.elek),
        };
        self.elek_intents(intents);
    }

    /// The pool as a pane in the right column.
    pub(super) fn show_elek_docked(&mut self, ui: &mut egui::Ui) {
        self.elek.docked = true;
        self.elek.keys_on = self.pool_focused;
        let intents = elek_view::show_docked(ui, &mut self.elek);
        self.elek_intents(intents);
    }

    /// The right column while docked: a Files / Sample pool switch above
    /// either the destination pane or the pool.
    pub(super) fn show_dock_col(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.add(theme::mode_title("Files", !self.dock_pool, false)).clicked() {
                self.dock_pool = false;
                self.pool_focused = false;
            }
            let lit = self.dock_pool && self.pool_focused;
            if ui.add(theme::mode_title("Sample pool", self.dock_pool, lit)).clicked() {
                self.dock_pool = true;
                self.pool_focused = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(theme::close_button(None, true)).on_hover_text("Close the docked pool (the project stays loaded; reopen it from the Device view's Dock button)").clicked() {
                    self.elek_docked = false;
                    self.pool_focused = false;
                }
            });
        });
        if self.dock_pool {
            let r = ui.max_rect();
            self.show_elek_docked(ui);
            self.pool_rect = Some(r);
        } else {
            self.pool_rect = None;
            self.show_pane(ui, Role::Dest);
        }
    }

    fn elek_intents(&mut self, intents: Vec<ElekIntent>) {
        self.sync_pool_fetch();
        for i in intents {
            match i {
                ElekIntent::BackToEditor => {
                    self.elek_mode = false;
                    // Back to where the pool lives by default.
                    if self.elek_docked {
                        self.dock_pool = true;
                    }
                }
                ElekIntent::Dock => self.dock_elek(),
                ElekIntent::Undock => {
                    self.elek.page = elek_view::ElekPage::Order;
                    self.enter_full_elek();
                }
                ElekIntent::OpenPatterns => {
                    self.elek.page = elek_view::ElekPage::Project;
                    self.enter_full_elek();
                }
                ElekIntent::FetchAll => self.fetch_all(),
                ElekIntent::StopFetch => self.stop_fetch(),
                ElekIntent::PurgeCache => self.purge_cache(),
                ElekIntent::AddFiles(pairs) => self.add_pool_files(pairs),
                ElekIntent::ListProjects => {
                    self.elek.projects = None;
                    self.device.elek(ElekCmd::ListProjects);
                }
                ElekIntent::Read => self.elek_read(),
                ElekIntent::SaveDump => {
                    let Some(p) = &self.elek.project else { continue };
                    let Some(home) = std::env::var_os("HOME") else { continue };
                    let root = PathBuf::from(home).join("Documents/sample_manager/project_dumps");
                    match p.save_dump(&root) {
                        Ok(dir) => {
                            crate::log::line(format!("elek: dump saved to {}", dir.display()));
                            self.elek.status = format!("Dump saved to {}", dir.display());
                            reveal(&dir);
                        }
                        Err(e) => {
                            self.elek.status = format!("Couldn't save the dump: {e}");
                            self.errors.push(format!("save dump: {e}"));
                        }
                    }
                }
                ElekIntent::SaveProject(bundle) => self.save_project_file(bundle),
                ElekIntent::LoadProject => self.load_project_file(),
                ElekIntent::SavePattern(slot, name, bundle) => self.save_pattern_file(slot, &name, bundle),
                ElekIntent::LoadPattern(slot) => self.load_pattern_file(slot),
                ElekIntent::Local(k, open) => self.elek_local(k, open),
                ElekIntent::ShowOriginal(k) => match self.links.original(k.0, k.1).map(|l| (l.local.clone(), l.fresh())) {
                    Some((p, fresh)) if p.exists() => {
                        self.show_in_source(&p);
                        self.elek.status = if fresh {
                            format!("{}: the original, shown in the source pane", p.display())
                        } else {
                            format!("{}: changed since upload, so the device plays an older version", p.display())
                        };
                    }
                    Some((p, _)) => self.elek.status = format!("{} was moved or deleted since the upload", p.display()),
                    None => self.elek.status = "No local original known for this sample".into(),
                },
                ElekIntent::RenameFile { key, from, to } => {
                    let to = to.trim();
                    let from = PathBuf::from(from);
                    let Some(parent) = from.parent() else { continue };
                    let target = parent.join(to);
                    if to.is_empty() || to.contains('/') || target == from {
                        continue;
                    }
                    let job = self.run_device(
                        crate::device::Task::Rename { from, to: target.clone() },
                        "Renaming",
                        1,
                        0,
                    );
                    self.pool_rename = Some((job, key, target.to_string_lossy().into_owned()));
                    self.relink_after = Some((job, key, target));
                }
                ElekIntent::Push { force } => self.elek_push(force, false),
            }
        }
    }

    /// Send the edits to the device. `live`: an automatic push from Live
    /// mode (always forced, quieter log).
    pub(super) fn elek_push(&mut self, force: bool, live: bool) {
        let Some(p) = &self.elek.project else { return };
        // A project from a file: the device holds something else
        // entirely, so there's nothing to compare against.
        let whole = p.from_file.is_some();
        let force = force || whole || live;
        let cells = &self.elek.proj.cells;
        let msgs = p.build_push(&self.elek.order, &self.elek.fills, cells);
        let pat_changed = self.elek.proj.changed();
        if pat_changed > 0 {
            for (i, c) in cells.iter().enumerate().filter(|(i, c)| **c != crate::device::Cell::as_read(*i)) {
                crate::log::line(format!("elek-pat: pushing {}: {:?}", crate::device::patterns::label(i), c));
            }
        }
        self.elek.expect_patterns = (pat_changed > 0 || whole).then(|| p.expected_patterns(&self.elek.order, &self.elek.fills, cells));
        for (slot, e) in &self.elek.fills {
            if e.is_empty() {
                crate::log::line(format!("elek: pushing slot {slot} (as read) cleared"));
            } else {
                crate::log::line(format!("elek: pushing new file in slot {slot} (as read): hash {:08x} size {} inode {} seqnr {}", e.hash, e.size, e.inode, e.seqnr));
            }
        }
        if msgs.is_empty() {
            self.elek.status = "Nothing to send: no pattern, kit or sound changes with this order.".into();
            return;
        }
        crate::log::line(format!(
            "elek: {} push, {} of {} dump messages",
            if whole { "whole-project" } else if live { "live" } else { "partial" },
            msgs.len(),
            p.msg_count()
        ));
        let model = p.model;
        let settings = p.settings_data().to_vec();
        self.elek.expect = Some(self.elek.new_pool());
        self.elek.busy = Some((if live { "Live push" } else { "Sending to device" }.into(), 0, msgs.len()));
        self.device.elek(ElekCmd::Push { msgs, settings, force, model });
    }

    /// Live mode: once the pool edits pause and the last push has been read
    /// back in full, push them.
    pub(super) fn live_push_tick(&mut self, ctx: &egui::Context) {
        const SETTLE: std::time::Duration = std::time::Duration::from_millis(400);
        let st = &mut self.elek;
        if !st.live {
            return;
        }
        let Some(p) = &st.project else {
            st.live = false;
            return;
        };
        if p.read_only.is_some() || p.from_file.is_some() {
            st.live = false;
            return;
        }
        if st.pool_changed() == 0 || !st.filling.is_empty() || st.confirm_push || st.confirm_stale {
            return;
        }
        if let Some(t) = st.edited_at {
            let since = t.elapsed();
            if since < SETTLE {
                ctx.request_repaint_after(SETTLE - since);
                return;
            }
        }
        // A read stopped early: some patterns would be left on the wrong
        // samples, so live pushes stop until a full reload.
        if st.reading.is_none() && st.project.as_ref().is_some_and(|p| p.partial) {
            st.live = false;
            st.status = "Live mode off: the read didn't finish, so not every pattern could follow its samples. Reload to turn it on again.".into();
            return;
        }
        if st.busy.is_some() || !st.pool_pushable() {
            return;
        }
        self.elek_push(true, true);
    }

    /// Play or open pool file `k`: from a local copy when there is one,
    /// else fetch it into the preview cache first (ahead of the queue).
    fn elek_local(&mut self, k: Key, open: bool) {
        if let Some(l) = self.links.local(k.0, k.1) {
            let (local, note) = (l.local.clone(), super::editing::link_note(l));
            if open {
                self.elek_mode = false;
                self.note_pool_edit(k, &local);
                self.load_into_editor(&local);
            } else {
                self.preview_toggle(&local);
            }
            self.status = note;
            return;
        }
        let Some(name) = self.elek.names.get(&k).cloned() else {
            self.elek.status = if self.elek.busy.is_some() {
                "Its +Drive path isn't known yet; try again once the names are in.".into()
            } else {
                "Not found on +Drive, so there's nothing to fetch.".into()
            };
            return;
        };
        let short = name.rsplit('/').next().unwrap_or(&name).to_owned();
        self.pool_fetch.then = Some((k, open));
        let f = &mut self.pool_fetch;
        if f.running.is_none_or(|(_, r)| r != k) {
            if let Some(i) = f.queue.iter().position(|&q| q == k) {
                f.queue.remove(i);
            } else if f.total > 0 {
                f.total += 1;
            }
            f.queue.push_front(k);
        }
        self.elek.status = format!("Fetching {short} from the device to {}…", if open { "open" } else { "play" });
        self.pump_fetch();
    }

    /// The +Drive folder dropped files go to: the device tab on screen
    /// (source first), else any device tab.
    pub(super) fn pool_upload_dir(&self) -> Option<PathBuf> {
        let active = [Role::Source, Role::Dest].into_iter().map(|r| self.pane(r).active_tab());
        let any = [Role::Source, Role::Dest].into_iter().flat_map(|r| self.pane(r).tabs.iter());
        active.chain(any).find(|t| t.device).map(|t| t.cwd.clone())
    }

    /// Files dropped onto empty pool slots: ones already on the +Drive
    /// (uploaded earlier and unchanged) are looked up right away, the rest
    /// are uploaded first.
    fn add_pool_files(&mut self, pairs: Vec<(u16, PathBuf)>) {
        let mut upload = Vec::new();
        for (slot, path) in pairs {
            self.pool_fetch.prior.remove(&slot);
            match self.links.local_link(&path).map(|l| (l.hash, l.size)) {
                Some((hash, size)) => {
                    self.pool_fetch.prior.insert(slot, path);
                    self.resolve_fill(slot, hash, size);
                }
                None => upload.push((slot, path)),
            }
        }
        self.add_pool_files_upload(upload);
    }

    fn add_pool_files_upload(&mut self, upload: Vec<(u16, PathBuf)>) {
        if upload.is_empty() {
            return;
        }
        let Some(dir) = self.pool_upload_dir() else {
            for (slot, _) in &upload {
                self.elek.filling.remove(slot);
            }
            self.elek.status = "Open the device's +Drive in a file tab first, at the folder new samples should go to".into();
            return;
        };
        let bytes: u64 = upload.iter().filter_map(|(_, p)| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
        let task = device::Task::Transfer {
            dir: device::Dir::Upload,
            items: upload.iter().map(|(_, p)| (p.clone(), dir.clone())).collect(),
            policy: Policy::KeepBoth,
            mono: None,
            total_bytes: bytes,
            local_names: Default::default(),
        };
        let n = upload.len();
        let id = self.run_device(task, &format!("Uploading {n} file(s) for the pool"), n, bytes);
        self.elek.status = format!("Uploading {n} file(s) to {} for the pool…", device::dev_str(&dir));
        self.pool_fetch.uploads.insert(id, upload);
    }

    /// Ask the device which length it knows `hash` by. The folder listing's
    /// size may or may not count the 64-byte sample header, so try both.
    fn resolve_fill(&mut self, slot: u16, hash: u32, size: u32) {
        let mut lens = vec![size, size.saturating_sub(64), size + 64];
        lens.retain(|&l| l > 0);
        lens.dedup();
        self.device.elek(ElekCmd::Resolve { slot, hash, lens });
    }

    /// Queue every pool file without a usable local copy.
    fn fetch_all(&mut self) {
        let Some(p) = &self.elek.project else { return };
        let mut keys: Vec<Key> = p.pool.iter().skip(1).filter(|e| !e.is_empty()).map(|e| e.key()).collect();
        keys.sort();
        keys.dedup();
        let (mut nameless, mut added) = (0, 0);
        for k in keys {
            if self.links.local(k.0, k.1).is_some() {
                continue;
            }
            if !self.elek.names.contains_key(&k) {
                nameless += 1;
                continue;
            }
            let f = &mut self.pool_fetch;
            if f.running.is_some_and(|(_, r)| r == k) || f.queue.contains(&k) {
                continue;
            }
            f.queue.push_back(k);
            added += 1;
        }
        let f = &mut self.pool_fetch;
        f.fails = 0;
        if f.total == 0 {
            f.done = 0;
            f.total = f.queue.len() + usize::from(f.running.is_some());
        } else {
            f.total += added;
        }
        self.elek.status = match (added, nameless) {
            (0, 0) => "Every pool sample already has a local copy".into(),
            (n, 0) => format!("Fetching {n} sample(s) in the background"),
            (n, m) => format!("Fetching {n} sample(s); {m} skipped (no +Drive path)"),
        };
        self.pump_fetch();
    }

    fn stop_fetch(&mut self) {
        let f = &mut self.pool_fetch;
        f.queue.clear();
        f.then = None;
        f.total = 0;
        f.done = 0;
        f.fails = 0;
        if let Some((id, _)) = f.running {
            self.jobs.cancel(id);
        }
        self.elek.status = "Fetching stopped".into();
        // A bundle waiting on the dropped fetches saves what it has.
        self.bundle_progress();
    }

    /// Start the next download if none is on its way.
    fn pump_fetch(&mut self) {
        if self.pool_fetch.running.is_some() {
            return;
        }
        let Some(cache) = crate::links::pool_cache_dir() else { return };
        while let Some(k) = self.pool_fetch.queue.pop_front() {
            let Some(name) = self.elek.names.get(&k).cloned() else {
                self.pool_fetch.done += 1;
                continue;
            };
            let dir = cache.join(format!("{:08x}_{}", k.0, k.1));
            let short = name.rsplit('/').next().unwrap_or(&name).to_owned();
            let task = device::Task::Transfer {
                dir: device::Dir::Download,
                items: vec![(PathBuf::from(&name), dir)],
                policy: Policy::Replace,
                mono: None,
                total_bytes: k.1 as u64,
                local_names: Default::default(),
            };
            let id = self.run_device(task, &format!("Fetching {short} for preview"), 1, k.1 as u64);
            self.pool_fetch.running = Some((id, k));
            break;
        }
        if self.pool_fetch.running.is_none() {
            self.pool_fetch.total = 0;
            self.pool_fetch.done = 0;
            self.measure_cache();
        }
        self.sync_pool_fetch();
        self.bundle_progress();
    }

    /// A device job finished: if it was a preview fetch, play what was
    /// waiting on it and start the next. Called before the status bar
    /// forgets the queue, so a run of fetches shows as one batch.
    pub(super) fn pool_fetch_finished(&mut self, job: u64, ok: bool) {
        if let Some(files) = self.pool_fetch.uploads.remove(&job) {
            for (slot, path) in files {
                match self.links.local_link(&path).map(|l| (l.hash, l.size)) {
                    Some((hash, size)) if self.elek.filling.contains_key(&slot) => self.resolve_fill(slot, hash, size),
                    _ if self.pool_fetch.import.as_ref().is_some_and(|i| i.slots.contains_key(&slot)) => {
                        self.import_done(slot, Some(format!("{} didn't make it to the device", path.display())));
                    }
                    _ => {
                        self.elek.filling.remove(&slot);
                        self.elek.status = format!("{} didn't make it to the device", path.display());
                    }
                }
            }
            return;
        }
        let Some((_, k)) = self.pool_fetch.running.take_if(|(id, _)| *id == job) else {
            return;
        };
        self.pool_fetch.done += 1;
        self.pool_fetch.fails = if ok { 0 } else { self.pool_fetch.fails + 1 };
        if self.pool_fetch.fails == 1 {
            // Likely moved on the device since the names were looked up.
            if self.pool_fetch.then.is_some_and(|(w, _)| w == k) {
                self.pool_fetch.then = None;
                self.elek.status = "Couldn't fetch that one (moved on the device since the read?)".into();
            }
        } else if !ok {
            // Unplugged or refused: don't run down the rest of the queue
            // into the same wall.
            let left = self.pool_fetch.queue.len();
            self.pool_fetch.queue.clear();
            self.pool_fetch.then = None;
            if left > 0 {
                self.elek.status = format!("Fetch failed; {left} queued fetch(es) dropped");
            }
        } else if let Some((want, open)) = self.pool_fetch.then.take_if(|(w, _)| *w == k) {
            self.elek_local(want, open);
        }
        if self.pool_fetch.done % 10 == 0 {
            self.measure_cache();
        }
        self.pump_fetch();
    }

    /// Mirror the fetch queue into the view.
    fn sync_pool_fetch(&mut self) {
        let f = &self.pool_fetch;
        self.elek.fetching = f.queue.iter().copied().chain(f.running.map(|(_, k)| k)).collect();
        self.elek.fetch = (f.total > 1 && (f.running.is_some() || !f.queue.is_empty())).then_some((f.done, f.total));
        self.elek.cache = crate::links::pool_cache_dir().and_then(|c| match self.sizes.get(&c) {
            Some(crate::sizes::Size::Known { bytes, files }) => Some((bytes, files)),
            _ => None,
        });
    }

    /// Ask for the cache folder's size again.
    pub(super) fn measure_cache(&mut self) {
        let Some(c) = crate::links::pool_cache_dir() else { return };
        if !c.is_dir() {
            self.sizes.set(c, 0, 0);
            return;
        }
        // Keep showing the old figure until the new one lands.
        if !matches!(self.sizes.get(&c), Some(crate::sizes::Size::Pending)) {
            self.jobs.dir_size(c);
        }
    }

    fn purge_cache(&mut self) {
        let Some(c) = crate::links::pool_cache_dir() else { return };
        if self.voice_path.as_ref().is_some_and(|p| p.starts_with(&c)) {
            self.engine.stop();
        }
        self.links.forget_under(&c);
        self.refresh_elek_links();
        if c.is_dir() {
            self.trash_local(vec![c.clone()]);
        }
        self.sizes.set(c, 0, 0);
        self.elek.status = "Preview cache moved to the Trash".into();
    }

    /// Which pool files we have a local copy of, for the view.
    pub(super) fn refresh_elek_links(&mut self) {
        let Some(p) = &self.elek.project else { return };
        let linked: std::collections::HashMap<_, _> = (0..p.pool.len())
            .map(|i| self.elek.entry(i))
            .filter(|e| !e.is_empty())
            .filter_map(|e| {
                let k = e.key();
                let l = self.links.known(k.0, k.1)?;
                Some((k, (l.device_name().to_owned(), l.local.clone(), l.uploaded)))
            })
            .collect();
        if linked.len() != self.elek.linked.len() {
            crate::log::line(format!("elek: {} pool files have a local copy", linked.len()));
        }
        self.elek.linked = linked;
    }

    /// Where project and pattern files go by default.
    fn elek_files_dir(sub: &str) -> Option<PathBuf> {
        let d = PathBuf::from(std::env::var_os("HOME")?).join("Documents/sample_manager").join(sub);
        let _ = std::fs::create_dir_all(&d);
        Some(d)
    }

    fn json_dialog(sub: &str, what: &str) -> rfd::FileDialog {
        let mut d = rfd::FileDialog::new().add_filter(what, &["json"]);
        if let Some(dir) = Self::elek_files_dir(sub) {
            d = d.set_directory(dir);
        }
        d
    }

    /// Asks where to save: a `.json` file, or for a bundle a folder of that
    /// name (the JSON goes inside it, beside `samples/`).
    fn ask_save_path(sub: &str, what: &str, base: &str, bundle: bool) -> Option<PathBuf> {
        if bundle {
            let mut d = rfd::FileDialog::new().set_title(format!("Save {what} with its samples (a folder)")).set_file_name(base);
            if let Some(dir) = Self::elek_files_dir(sub) {
                d = d.set_directory(dir);
            }
            let p = d.save_file()?;
            Some(if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")) { p.with_extension("") } else { p })
        } else {
            Self::json_dialog(sub, what).set_file_name(format!("{base}.json")).save_file()
        }
    }

    fn save_project_file(&mut self, bundle: bool) {
        let Some(p) = &self.elek.project else { return };
        let base = format!("{} project", p.model.name());
        let Some(path) = Self::ask_save_path("projects", "Project", &base, bundle) else { return };
        let st = &self.elek;
        let file = p.project_file(&st.order, &st.fills, &st.proj.cells, &st.new_pool(), &st.names);
        self.save_elek_file(file, path, bundle, "Project".into());
    }

    fn save_pattern_file(&mut self, slot: usize, name: &str, bundle: bool) {
        let Some(p) = &self.elek.project else { return };
        let label = crate::device::patterns::label(slot);
        let base = if name.trim().is_empty() { label.clone() } else { format!("{label} {}", name.trim()) };
        let base: String = base.chars().map(|c| if "/\\:".contains(c) { '_' } else { c }).collect();
        let Some(path) = Self::ask_save_path("patterns", "Pattern", &base, bundle) else { return };
        let st = &self.elek;
        let file = p.pattern_file(slot, name, &st.order, &st.fills, &st.proj.cells, &st.new_pool(), &st.names);
        self.save_elek_file(file, path, bundle, label);
    }

    /// Write a built file, or for a bundle gather its samples first:
    /// local copies where we have them, the rest fetched from the device
    /// into the preview cache (`finish_bundle` runs once they're in).
    fn save_elek_file(&mut self, file: Result<file::SaveFile, String>, path: PathBuf, bundle: bool, what: String) {
        let mut file = match file {
            Ok(f) => f,
            Err(e) => {
                self.elek.status = format!("Couldn't save {}: {e}", what.to_lowercase());
                self.errors.push(format!("save {}: {e}", what.to_lowercase()));
                return;
            }
        };
        if !bundle {
            match file.write(&path) {
                Ok(()) => {
                    crate::log::line(format!("elek: {what} saved to {}", path.display()));
                    self.elek.status = format!("{what} saved to {}", path.display());
                }
                Err(e) => {
                    self.elek.status = format!("Couldn't save {}: {e}", what.to_lowercase());
                    self.errors.push(format!("save {}: {e}", what.to_lowercase()));
                }
            }
            return;
        }
        if self.pool_fetch.bundle.is_some() {
            self.elek.status = "Another bundle is still gathering its samples; try again once it's saved".into();
            return;
        }
        if path.is_file() {
            self.elek.status = format!("{} is a file; a bundle needs a folder there", path.display());
            return;
        }
        let stem = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "bundle".into());
        let mut waiting = HashSet::new();
        for s in file.samples_mut().iter() {
            let Some(k) = s.key() else { continue };
            if self.links.local(k.0, k.1).is_none() && self.elek.names.contains_key(&k) {
                waiting.insert(k);
            }
        }
        let n = waiting.len();
        let f = &mut self.pool_fetch;
        f.fails = 0;
        for &k in &waiting {
            if f.running.is_some_and(|(_, r)| r == k) || f.queue.contains(&k) {
                continue;
            }
            f.queue.push_back(k);
            if f.total > 0 {
                f.total += 1;
            }
        }
        if f.total == 0 {
            f.done = 0;
            f.total = f.queue.len() + usize::from(f.running.is_some());
        }
        f.bundle = Some(BundleSave { file, json: format!("{stem}.json"), dir: path, what, waiting });
        if n > 0 {
            self.elek.status = format!("Fetching {n} sample(s) from the device for the bundle…");
        }
        // Finishes right away when nothing needs fetching.
        self.pump_fetch();
    }

    /// Drop fetches that are done (or dropped) from the bundle's wait list,
    /// and write it once nothing is left.
    fn bundle_progress(&mut self) {
        let f = &self.pool_fetch;
        let pending: HashSet<Key> = f.queue.iter().copied().chain(f.running.map(|(_, k)| k)).collect();
        let Some(b) = self.pool_fetch.bundle.as_mut() else { return };
        b.waiting.retain(|k| pending.contains(k));
        if b.waiting.is_empty() {
            let b = self.pool_fetch.bundle.take().expect("checked above");
            self.finish_bundle(b);
        }
    }

    fn finish_bundle(&mut self, mut b: BundleSave) {
        let samples = b.dir.join("samples");
        let what = b.what.clone();
        let fail = |app: &mut Self, e: String| {
            app.elek.status = format!("Couldn't save {}: {e}", what.to_lowercase());
            app.errors.push(format!("save bundle {}: {e}", b.dir.display()));
        };
        if let Err(e) = std::fs::create_dir_all(&samples) {
            return fail(self, format!("{}: {e}", samples.display()));
        }
        let mut taken = HashSet::new();
        let mut done: HashMap<Key, String> = HashMap::new();
        let (mut copied, mut missing, mut failed) = (0, Vec::new(), Vec::new());
        for s in b.file.samples_mut() {
            let Some(k) = s.key() else { continue };
            if let Some(f) = done.get(&k) {
                s.file = Some(f.clone());
                continue;
            }
            let Some(l) = self.links.local(k.0, k.1) else {
                missing.push(s.short());
                continue;
            };
            let src = l.local.clone();
            let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("wav").to_lowercase();
            let name = file::bundle_name(s, l.device_name(), &ext, &mut taken);
            match std::fs::copy(&src, samples.join(&name)) {
                Ok(_) => {
                    let rel = format!("samples/{name}");
                    s.file = Some(rel.clone());
                    done.insert(k, rel);
                    copied += 1;
                }
                Err(e) => failed.push(format!("{}: {e}", src.display())),
            }
        }
        let json = b.dir.join(&b.json);
        if let Err(e) = b.file.write(&json) {
            return fail(self, e);
        }
        crate::log::line(format!(
            "elek: {what} bundle saved to {}: {copied} sample(s), missing {missing:?}, failed {failed:?}",
            b.dir.display()
        ));
        let mut msg = format!("{what} saved with {copied} sample(s) to {}", b.dir.display());
        if !missing.is_empty() {
            msg.push_str(&format!(". No copy of {} (not found on the device): noted by hash only", missing.join(", ")));
        }
        if !failed.is_empty() {
            msg.push_str(&format!(". {} couldn't be copied (see errors)", failed.len()));
            self.errors.extend(failed);
        }
        self.elek.status = msg;
        reveal(&json);
    }

    /// A load is still placing samples from its file.
    fn importing(&self) -> bool {
        self.pool_fetch.import.as_ref().is_some_and(|i| i.slots.keys().any(|s| self.elek.filling.contains_key(s)))
    }

    /// +Drive folder for samples a bundle brings that the device lacks.
    fn import_dir(json: &Path) -> PathBuf {
        let stem = json.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "bundle".into());
        let stem: String = stem.chars().map(|c| if "/\\".contains(c) { '_' } else { c }).collect();
        PathBuf::from("/imported").join(stem.trim())
    }

    /// Start placing `slots` (`(slot as read, its sample)`): each is looked
    /// up on the device by hash, and uploaded from the bundle if missing.
    /// `verify`: the slot already points at the file (a loaded project);
    /// otherwise it's an empty slot to fill.
    fn start_import(&mut self, what: String, json: &Path, slots: Vec<(u16, file::SampleRef)>, verify: bool) {
        let dir = Self::import_dir(json);
        let mut imp = Import { what, ..Default::default() };
        for (slot, s) in slots {
            let Some(key) = s.key() else { continue };
            let name = s.short();
            self.elek.filling.insert(slot, name.clone());
            imp.slots.insert(slot, ImportSlot { name, key, file: s.bundled(json), dir: dir.clone(), stage: Stage::Hash, verify });
            self.device.elek(ElekCmd::Resolve { slot, hash: key.0, lens: vec![key.1] });
        }
        let empty = imp.slots.is_empty();
        self.pool_fetch.import = Some(imp);
        if empty {
            self.import_summary();
        }
    }

    fn import_resolved(&mut self, slot: u16, hash: u32, found: Result<Option<(u32, String)>, String>) {
        let Some(is) = self.pool_fetch.import.as_mut().and_then(|i| i.slots.get_mut(&slot)) else { return };
        if !self.elek.filling.contains_key(&slot) {
            // The project was closed or replaced meanwhile.
            self.pool_fetch.import = None;
            return;
        }
        let name = is.name.clone();
        match found {
            Ok(Some((len, path))) => {
                let e = crate::device::PoolEntry::new_file(hash, len);
                let up = is.stage != Stage::Hash;
                if up && e.key() != is.key {
                    crate::log::line(format!(
                        "elek: {name} came back as {:08x}/{} (bundle had {:08x}/{})",
                        e.hash, len, is.key.0, is.key.1
                    ));
                }
                let same = is.verify && self.elek.entry(slot as usize).key() == e.key();
                self.elek.names.insert(e.key(), path);
                if !same {
                    self.elek.fill(slot, e);
                }
                let imp = self.pool_fetch.import.as_mut().expect("checked above");
                if up { imp.uploaded += 1 } else { imp.found += 1 }
                self.import_done(slot, None);
                self.refresh_elek_links();
            }
            Ok(None) => match is.stage {
                Stage::Hash => {
                    let Some(file) = is.file.clone() else {
                        if let Some(i) = self.pool_fetch.import.as_mut() {
                            i.missing.push(name);
                        }
                        return self.import_done(slot, None);
                    };
                    // Uploaded from this very file before, under another hash?
                    let link = self.links.local_link(&file).map(|l| (l.hash, l.size)).filter(|&(h, _)| h != is.key.0);
                    if let Some((h, size)) = link {
                        is.stage = Stage::Link;
                        self.resolve_fill(slot, h, size);
                    } else {
                        is.stage = Stage::Upload;
                        let dir = is.dir.clone();
                        self.import_upload(slot, file, dir);
                    }
                }
                Stage::Link => {
                    is.stage = Stage::Upload;
                    let (file, dir) = (is.file.clone().expect("linked from it"), is.dir.clone());
                    self.import_upload(slot, file, dir);
                }
                Stage::Upload => self.import_done(slot, Some(format!("{name}: uploaded, but the device can't find it by its hash"))),
            },
            Err(e) => self.import_done(slot, Some(format!("{name}: {e}"))),
        }
    }

    fn import_upload(&mut self, slot: u16, file: PathBuf, dir: PathBuf) {
        let bytes = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
        let short = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let task = device::Task::Transfer {
            dir: device::Dir::Upload,
            items: vec![(file.clone(), dir.clone())],
            policy: Policy::KeepBoth,
            mono: None,
            total_bytes: bytes,
            local_names: Default::default(),
        };
        let id = self.run_device(task, &format!("Uploading {short} for the pool"), 1, bytes);
        crate::log::line(format!("elek: {short} not on the device; uploading to {}", device::dev_str(&dir)));
        self.pool_fetch.uploads.insert(id, vec![(slot, file)]);
    }

    /// Slot `slot` is placed (or failed with `err`); sums up once all are.
    fn import_done(&mut self, slot: u16, err: Option<String>) {
        self.elek.filling.remove(&slot);
        let Some(imp) = self.pool_fetch.import.as_mut() else { return };
        imp.slots.remove(&slot);
        if let Some(e) = err {
            imp.failed.push(e);
        }
        if imp.slots.is_empty() {
            self.import_summary();
        } else {
            let left = imp.slots.len();
            self.elek.status = format!("{}: placing samples, {left} to go…", imp.what);
        }
    }

    fn import_summary(&mut self) {
        let Some(imp) = self.pool_fetch.import.take() else { return };
        let mut msg = imp.what.clone();
        let mut parts = Vec::new();
        if imp.found > 0 {
            parts.push(format!("{} sample(s) found on the device", imp.found));
        }
        if imp.uploaded > 0 {
            parts.push(format!("{} uploaded from the bundle", imp.uploaded));
        }
        if !parts.is_empty() {
            msg.push_str(&format!(": {}", parts.join(", ")));
        }
        if !imp.missing.is_empty() {
            msg.push_str(&format!(". Not on the device and not bundled: {}", imp.missing.join(", ")));
        }
        if !imp.failed.is_empty() {
            msg.push_str(&format!(". {} sample(s) failed (see errors)", imp.failed.len()));
        }
        crate::log::line(format!("elek: import done: {msg}; failed {:?}", imp.failed));
        self.errors.extend(imp.failed);
        self.elek.status = msg;
    }

    fn load_project_file(&mut self) {
        if self.elek.changed() > 0 {
            self.elek.status = "Push or close the current project first: it has unpushed changes".into();
            return;
        }
        if self.importing() {
            self.elek.status = "Still placing samples from the last load; try again once it's done".into();
            return;
        }
        let Some(path) = Self::json_dialog("projects", "Project").pick_file() else { return };
        match crate::device::ElekProject::load_file(&path) {
            Ok((p, samples)) => {
                crate::log::line(format!(
                    "elek: project loaded from {}: {}{}",
                    path.display(),
                    p.versions,
                    p.read_only.as_ref().map(|w| format!(", READ-ONLY: {w}")).unwrap_or_default()
                ));
                let n = p.pool.len();
                let st = &mut self.elek;
                st.names.extend(samples.iter().filter_map(|s| Some((s.key()?, s.path.clone()?))));
                st.set_project(p);
                self.refresh_elek_links();
                let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let bundled = samples.iter().filter(|s| s.bundled(&path).is_some()).count();
                let slots: Vec<_> = samples.into_iter().filter(|s| s.slot > 0 && s.slot < n).map(|s| (s.slot as u16, s)).collect();
                let what = format!(
                    "Opened {file_name}{}. It's only here for now: Push sends the whole project into the device's memory",
                    if bundled > 0 { format!(" ({bundled} bundled sample(s))") } else { String::new() }
                );
                self.elek.status = format!("{what}. Checking its samples on the device…");
                self.start_import(what, &path, slots, true);
            }
            Err(e) => {
                self.elek.status = format!("Couldn't open the project: {e}");
                self.errors.push(format!("load project {}: {e}", path.display()));
            }
        }
    }

    /// Load a pattern file into `slot`. Its samples are matched to this pool
    /// by +Drive hash; ones not in the pool go into empty slots, each found
    /// on the device by hash or, from a bundle, uploaded (push waits).
    fn load_pattern_file(&mut self, slot: usize) {
        if self.importing() {
            self.elek.status = "Still placing samples from the last load; try again once it's done".into();
            return;
        }
        let Some(path) = Self::json_dialog("patterns", "Pattern").pick_file() else { return };
        let Some(p) = &self.elek.project else { return };
        let lp = match p.load_pattern(&path) {
            Ok(lp) => lp,
            Err(e) => {
                self.elek.status = format!("Couldn't load the pattern: {e}");
                return;
            }
        };
        let st = &self.elek;
        let n = p.pool.len();
        let mut map = HashMap::new();
        let mut taken = HashSet::new();
        let (mut found, mut lookup, mut no_room) = (0, Vec::<(u16, file::SampleRef)>::new(), Vec::new());
        for s in &lp.samples {
            let Some(k) = s.key() else { continue };
            // Already in the pool.
            if let Some(o) = (1..n).find(|&o| {
                let e = st.entry(o);
                !e.is_empty() && e.key() == k
            }) {
                map.insert(s.slot, o);
                found += 1;
                continue;
            }
            // On its way in from this same load.
            if let Some((o, _)) = lookup.iter().find(|(_, x)| x.key() == Some(k)) {
                map.insert(s.slot, *o as usize);
                continue;
            }
            // The first empty slot in the order as shown.
            let free = st.order.iter().skip(1).copied().find(|&o| st.free(o) && !taken.contains(&o));
            match free {
                Some(o) => {
                    taken.insert(o);
                    map.insert(s.slot, o as usize);
                    lookup.push((o, s.clone()));
                }
                None => no_room.push(s.short()),
            }
        }
        let Some(p) = self.elek.project.as_mut() else { return };
        let idx = match p.import_pattern(lp.data, &map) {
            Ok(i) => i,
            Err(e) => {
                self.elek.status = format!("Couldn't load the pattern: {e}");
                return;
            }
        };
        let label = crate::device::patterns::label(slot);
        crate::log::line(format!(
            "elek-pat: {} loaded into {label}: {found} sample(s) in the pool, {} to place, {} without room; map {map:?}",
            path.display(),
            lookup.len(),
            no_room.len()
        ));
        self.elek.proj.place_import(slot, idx);
        let mut msg = format!("Loaded \"{}\" into {label}", lp.name);
        if found > 0 {
            msg.push_str(&format!(" ({found} sample(s) already in the pool)"));
        }
        if !no_room.is_empty() {
            msg.push_str(&format!(". No empty pool slot for {}: those tracks play nothing", no_room.join(", ")));
        }
        if !lookup.is_empty() {
            self.elek.status = format!("{msg}. Placing {} sample(s)…", lookup.len());
        } else {
            self.elek.status = msg.clone();
        }
        self.start_import(msg, &path, lookup, false);
    }

    fn elek_read(&mut self) {
        self.elek.busy = Some(("Reading project".into(), 0, 129));
        let known = self.elek.names.keys().copied().collect();
        let ctl = std::sync::Arc::new(crate::device::ReadCtl::default());
        ctl.open.store(self.elek.proj.bank_mask(), std::sync::atomic::Ordering::Relaxed);
        self.elek.reading = Some(ctl.clone());
        self.elek.read_shown = false;
        self.device.elek(ElekCmd::Read { known, ctl });
    }

    pub(super) fn elek_event(&mut self, e: ElekEv) {
        // The names come right after the pool, before the patterns stream
        // in; a read without that early stage ends in `Loaded`.
        if self.first_dock {
            match &e {
                ElekEv::Pattern { .. } | ElekEv::Loaded(Ok(_)) => self.first_dock_now(),
                ElekEv::Loaded(Err(_)) if self.elek.project.is_some() => self.first_dock_now(),
                ElekEv::Loaded(Err(_)) => self.first_dock = false,
                _ => {}
            }
        }
        let st = &mut self.elek;
        match e {
            ElekEv::Projects(r) => st.projects = Some(r),
            ElekEv::Partial(p) => {
                st.set_project(p);
                st.read_shown = true;
                self.refresh_elek_links();
            }
            ElekEv::Pattern { msg, pats } => {
                if st.reading.is_none() {
                    return;
                }
                if let Some(p) = st.project.as_mut().filter(|p| p.partial) {
                    p.add_streamed(msg, pats);
                    st.proj.refresh(p);
                }
            }
            ElekEv::Uses(v) => {
                if let Some(p) = st.project.as_mut().filter(|p| p.partial && p.uses.len() == v.len()) {
                    p.uses = v;
                }
            }
            ElekEv::Progress { label, done, total } => st.busy = Some((label, done, total)),
            ElekEv::Loaded(r) => {
                st.busy = None;
                let streamed = st.reading.take().is_some();
                match r {
                    Ok(p) => {
                        let used = p.pool.iter().skip(1).filter(|e| !e.is_empty()).count();
                        // Entries the device hasn't given an inode yet (new files pushed
                        // with 0), on any read: tells whether a save + reload fills it in.
                        let unset: Vec<String> = p.pool.iter().enumerate().filter(|(_, e)| !e.is_empty() && e.inode == 0).map(|(i, e)| format!("{i} ({:08x})", e.hash)).collect();
                        crate::log::line(format!("elek: {} pool entries with inode 0: {}", unset.len(), unset.join(", ")));
                        // New files pushed with inode/seqnr 0: what did the device make of them?
                        if let Some(want) = &st.expect {
                            for (i, (w, got)) in want.iter().zip(&p.pool).enumerate() {
                                if !w.is_empty() && w.inode == 0 {
                                    crate::log::line(format!(
                                        "elek: read back new slot {i}: inode {} hash {:08x} size {} seqnr {} (sent hash {:08x} size {})",
                                        got.inode, got.hash, got.size, got.seqnr, w.hash, w.size
                                    ));
                                }
                            }
                        }
                        for (i, e) in p.pool.iter().enumerate().skip(1).filter(|(_, e)| !e.is_empty()).take(12) {
                            crate::log::line(format!(
                                "elek: pool {i}: inode {} hash {:08x} size {} seqnr {}",
                                e.inode, e.hash, e.size, e.seqnr
                            ));
                        }
                        // The device may renumber inodes; compare the files.
                        let differ = |want: &[crate::device::PoolEntry]| {
                            want.iter()
                                .zip(&p.pool)
                                .filter(|(a, b)| a.is_empty() != b.is_empty() || a.key() != b.key())
                                .count()
                        };
                        let pat_check = st.expect_patterns.take().map(|want| {
                            let got = p.pattern_data();
                            let Ok(pats) = &p.patterns else {
                                return Err("patterns couldn't be parsed after the push".to_owned());
                            };
                            // Slots not loaded on either side (a stopped read) aren't compared.
                            let pairs: Vec<(usize, &[u8], &[u8])> = (0..want.len())
                                .filter_map(|i| Some((i, want[i].as_deref()?, got.get(i).copied().flatten()?)))
                                .collect();
                            let off: Vec<usize> = pairs.iter().filter(|(_, w, g)| w != g).map(|(i, _, _)| *i).collect();
                            for (i, w, g) in pairs.iter().filter(|(_, w, g)| w != g).take(8) {
                                crate::log::line(format!(
                                    "elek-pat: read back {} differs from what was sent: {}",
                                    crate::device::patterns::label(*i),
                                    pats.describe_diff(w, g)
                                ));
                            }
                            if off.is_empty() { Ok(()) } else { Err(format!("{} pattern(s) read back different from what was sent", off.len())) }
                        });
                        st.status = match st.expect.take() {
                            Some(want) if differ(&want) == 0 => {
                                "Pushed and read back: the device has the new order. Save on the device (PROJECT › SAVE) to keep it.".to_owned()
                            }
                            Some(want) => {
                                let off = differ(&want);
                                let msg = format!("Read back after push: {off} slots differ from what was sent. Don't save; reload the project from +Drive.");
                                self.errors.push(msg.clone());
                                msg
                            }
                            None => format!("Read {used} samples"),
                        };
                        match pat_check {
                            Some(Ok(())) => {
                                crate::log::line("elek-pat: read back after push: every pattern matches what was sent".to_owned());
                                st.status = "Pushed and read back: the device has the new patterns. Save on the device (PROJECT › SAVE) to keep it.".into();
                            }
                            Some(Err(e)) => {
                                let msg = format!("{e} (details in the log, elek-pat:). Check on the device before saving; if anything is off, reload the project from +Drive.");
                                self.errors.push(msg.clone());
                                st.status = msg;
                            }
                            None => {}
                        }
                        if p.partial {
                            let (n, banks) = match &p.patterns {
                                Ok(pats) => (
                                    pats.loaded_count(),
                                    (0..8).filter(|&b| pats.bank_loaded(b)).map(|b| ((b'A' + b as u8) as char).to_string()).collect::<Vec<_>>(),
                                ),
                                Err(_) => (0, Vec::new()),
                            };
                            st.status = if banks.is_empty() {
                                format!("Read stopped after {n} of 128 patterns: no bank loaded completely, so nothing can be pushed. Reload to read the rest.")
                            } else {
                                format!("Read stopped after {n} of 128 patterns. Bank {} can be edited and pushed; reload to read the rest.", banks.join(", "))
                            };
                        }
                        if streamed {
                            st.finish_read(p);
                        } else {
                            st.set_project(p);
                        }
                        self.refresh_elek_links();
                    }
                    Err(e) => {
                        st.expect = None;
                        st.status = format!("Read failed: {e}");
                        // A pool shown early stays, but can't be pushed.
                        if let Some(p) = st.project.as_mut().filter(|p| p.partial) {
                            p.read_only = Some(format!("the read didn't finish ({e}); load again to push"));
                        }
                    }
                }
            }
            ElekEv::Names(v) => st.names.extend(v),
            ElekEv::Resolved { slot, hash, found } => {
                if self.pool_fetch.import.as_ref().is_some_and(|i| i.slots.contains_key(&slot)) {
                    self.import_resolved(slot, hash, found);
                    return;
                }
                // Dropped from the slot meanwhile (project reloaded or closed).
                let Some(name) = st.filling.get(&slot).cloned() else { return };
                let prior = self.pool_fetch.prior.remove(&slot);
                if let (Ok(None), Some(path)) = (&found, prior) {
                    // Uploaded once, gone from the device since: send it again.
                    crate::log::line(format!("elek: {} no longer on the device; uploading again", path.display()));
                    self.add_pool_files_upload(vec![(slot, path)]);
                    return;
                }
                match found {
                    Ok(Some((len, path))) => {
                        let e = crate::device::PoolEntry::new_file(hash, len);
                        let n = st.order.len();
                        let dup = (1..n).find(|&s| {
                            // Replacing a slot's own sample isn't a duplicate of it.
                            let x = st.entry(st.order[s] as usize);
                            st.order[s] != slot && !x.is_empty() && x.key() == e.key()
                        });
                        if let Some(at) = dup {
                            st.filling.remove(&slot);
                            st.status = format!("{name} is already in the pool (slot {at:03}); left out");
                        } else {
                            st.fill(slot, e);
                            st.names.insert((hash, len), path.clone());
                            st.status = format!("{name} is ready in its slot; push to send it to the device");
                            self.refresh_elek_links();
                            self.pool_edit_filled(slot, (hash, len), &path);
                        }
                    }
                    Ok(None) => {
                        st.filling.remove(&slot);
                        st.status = format!("The device can't find {name} by its hash; left out");
                        self.errors.push(format!("pool: the device has no file with hash {hash:08x} ({name})"));
                    }
                    Err(e) => {
                        st.filling.remove(&slot);
                        st.status = format!("Couldn't check {name} on the device: {e}");
                    }
                }
            }
            ElekEv::PushStale => {
                st.busy = None;
                st.expect = None;
                st.expect_patterns = None;
                st.status = "Not pushed: the project on the device changed since it was read".into();
                st.confirm_stale = true;
            }
            ElekEv::Pushed(r) => match r {
                Ok(n) => {
                    st.status = format!("Sent {n} messages; reading back to check…");
                    self.elek_read();
                }
                Err(e) => {
                    st.busy = None;
                    st.expect = None;
                    st.expect_patterns = None;
                    st.status = format!("Push failed: {e}");
                    self.errors.push(format!("push to device: {e}"));
                }
            },
        }
    }
}
