//! The Elk Sample Order mode: device requests and their answers.

use super::*;
use crate::device::{ElkCmd, ElkEv};
use crate::ui::elk_view::{self, ElkIntent};
use std::collections::VecDeque;

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
    uploads: std::collections::HashMap<u64, Vec<(u16, PathBuf)>>,
    /// Slots being looked up from an earlier upload's link: if the device
    /// no longer has that file, upload it again.
    prior: std::collections::HashMap<u16, PathBuf>,
}

impl App {
    /// A full-window page. The docked pool stays docked for the way back,
    /// but the right column shows the files meanwhile.
    pub(super) fn enter_full_elk(&mut self) {
        self.dock_pool = false;
        self.pool_focused = false;
        self.enter_elk();
    }

    pub(super) fn enter_elk(&mut self) {
        self.elk_mode = true;
        self.editor_focused = false;
        self.measure_cache();
        if !self.elk.asked_projects {
            self.elk.asked_projects = true;
            self.device.elk(ElkCmd::ListProjects);
        }
    }

    /// Optional home of the pool: docked in the right column, beside the
    /// files and the editor. (The Device button opens the center view.)
    pub(super) fn dock_elk(&mut self) {
        self.elk_mode = false;
        self.elk_docked = true;
        self.dock_pool = true;
        self.pool_focused = true;
        self.editor_focused = false;
        self.measure_cache();
        if !self.elk.asked_projects {
            self.elk.asked_projects = true;
            self.device.elk(ElkCmd::ListProjects);
        }
    }

    pub(super) fn show_elk(&mut self, ui: &mut egui::Ui) {
        self.elk.docked = false;
        self.elk.keys_on = true;
        let intents = match self.elk.page {
            elk_view::ElkPage::Order => elk_view::show(ui, &mut self.elk),
            elk_view::ElkPage::Project => crate::ui::elk_project_view::show(ui, &mut self.elk),
        };
        self.elk_intents(intents);
    }

    /// The pool as a pane in the right column.
    pub(super) fn show_elk_docked(&mut self, ui: &mut egui::Ui) {
        self.elk.docked = true;
        self.elk.keys_on = self.pool_focused;
        let intents = elk_view::show_docked(ui, &mut self.elk);
        self.elk_intents(intents);
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
                    self.elk_docked = false;
                    self.pool_focused = false;
                }
            });
        });
        if self.dock_pool {
            let r = ui.max_rect();
            self.show_elk_docked(ui);
            self.pool_rect = Some(r);
        } else {
            self.pool_rect = None;
            self.show_pane(ui, Role::Dest);
        }
    }

    fn elk_intents(&mut self, intents: Vec<ElkIntent>) {
        self.sync_pool_fetch();
        for i in intents {
            match i {
                ElkIntent::BackToEditor => {
                    self.elk_mode = false;
                    // Back to where the pool lives by default.
                    if self.elk_docked {
                        self.dock_pool = true;
                    }
                }
                ElkIntent::Dock => self.dock_elk(),
                ElkIntent::Undock => {
                    self.elk.page = elk_view::ElkPage::Order;
                    self.enter_full_elk();
                }
                ElkIntent::OpenPatterns => {
                    self.elk.page = elk_view::ElkPage::Project;
                    self.enter_full_elk();
                }
                ElkIntent::FetchAll => self.fetch_all(),
                ElkIntent::StopFetch => self.stop_fetch(),
                ElkIntent::PurgeCache => self.purge_cache(),
                ElkIntent::AddFiles(pairs) => self.add_pool_files(pairs),
                ElkIntent::ListProjects => {
                    self.elk.projects = None;
                    self.device.elk(ElkCmd::ListProjects);
                }
                ElkIntent::Read => self.elk_read(),
                ElkIntent::SaveDump => {
                    let Some(p) = &self.elk.project else { continue };
                    let Some(home) = std::env::var_os("HOME") else { continue };
                    let root = PathBuf::from(home).join("Documents/sample_manager/project_dumps");
                    match p.save_dump(&root) {
                        Ok(dir) => {
                            crate::log::line(format!("elk: dump saved to {}", dir.display()));
                            self.elk.status = format!("Dump saved to {}", dir.display());
                            reveal(&dir);
                        }
                        Err(e) => {
                            self.elk.status = format!("Couldn't save the dump: {e}");
                            self.errors.push(format!("save dump: {e}"));
                        }
                    }
                }
                ElkIntent::Local(k, open) => self.elk_local(k, open),
                ElkIntent::Push { force } => {
                    let Some(p) = &self.elk.project else { continue };
                    let cells = &self.elk.proj.cells;
                    let msgs = p.build_push(&self.elk.order, &self.elk.fills, cells);
                    let pat_changed = self.elk.proj.changed();
                    if pat_changed > 0 {
                        for (i, c) in cells.iter().enumerate().filter(|(i, c)| **c != crate::device::Cell::as_read(*i)) {
                            crate::log::line(format!("elk-pat: pushing {}: {:?}", crate::device::patterns::label(i), c));
                        }
                    }
                    self.elk.expect_patterns = (pat_changed > 0).then(|| p.expected_patterns(&self.elk.order, &self.elk.fills, cells));
                    for (slot, e) in &self.elk.fills {
                        if e.is_empty() {
                            crate::log::line(format!("elk: pushing slot {slot} (as read) cleared"));
                        } else {
                            crate::log::line(format!("elk: pushing new file in slot {slot} (as read): hash {:08x} size {} inode {} seqnr {}", e.hash, e.size, e.inode, e.seqnr));
                        }
                    }
                    if msgs.is_empty() {
                        self.elk.status = "Nothing to send: no pattern, kit or sound changes with this order.".into();
                        continue;
                    }
                    crate::log::line(format!(
                        "elk: partial push, {} of {} dump messages changed",
                        msgs.len(),
                        p.msg_count()
                    ));
                    let settings = p.settings_data().to_vec();
                    self.elk.expect = Some(self.elk.new_pool());
                    self.elk.busy = Some(("Sending to device".into(), 0, msgs.len()));
                    self.device.elk(ElkCmd::Push { msgs, settings, force });
                }
            }
        }
    }

    /// Play or open pool file `k`: from a local copy when there is one,
    /// else fetch it into the preview cache first (ahead of the queue).
    fn elk_local(&mut self, k: Key, open: bool) {
        if let Some(l) = self.links.local(k.0, k.1) {
            let (local, note) = (l.local.clone(), super::editing::link_note(l));
            if open {
                self.elk_mode = false;
                self.load_into_editor(&local);
            } else {
                self.preview_toggle(&local);
            }
            self.status = note;
            return;
        }
        let Some(name) = self.elk.names.get(&k).cloned() else {
            self.elk.status = if self.elk.busy.is_some() {
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
        self.elk.status = format!("Fetching {short} from the device to {}…", if open { "open" } else { "play" });
        self.pump_fetch();
    }

    /// The +Drive folder dropped files go to: the device tab on screen
    /// (source first), else any device tab.
    fn pool_upload_dir(&self) -> Option<PathBuf> {
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
                self.elk.filling.remove(slot);
            }
            self.elk.status = "Open the device's +Drive in a file tab first, at the folder new samples should go to".into();
            return;
        };
        let bytes: u64 = upload.iter().filter_map(|(_, p)| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
        let task = device::Task::Transfer {
            dir: device::Dir::Upload,
            items: upload.iter().map(|(_, p)| (p.clone(), dir.clone())).collect(),
            policy: Policy::KeepBoth,
            mono: None,
            total_bytes: bytes,
        };
        let n = upload.len();
        let id = self.run_device(task, &format!("Uploading {n} file(s) for the pool"), n, bytes);
        self.elk.status = format!("Uploading {n} file(s) to {} for the pool…", device::dev_str(&dir));
        self.pool_fetch.uploads.insert(id, upload);
    }

    /// Ask the device which length it knows `hash` by. The folder listing's
    /// size may or may not count the 64-byte sample header, so try both.
    fn resolve_fill(&mut self, slot: u16, hash: u32, size: u32) {
        let mut lens = vec![size, size.saturating_sub(64), size + 64];
        lens.retain(|&l| l > 0);
        lens.dedup();
        self.device.elk(ElkCmd::Resolve { slot, hash, lens });
    }

    /// Queue every pool file without a usable local copy.
    fn fetch_all(&mut self) {
        let Some(p) = &self.elk.project else { return };
        let mut keys: Vec<Key> = p.pool.iter().skip(1).filter(|e| !e.is_empty()).map(|e| e.key()).collect();
        keys.sort();
        keys.dedup();
        let (mut nameless, mut added) = (0, 0);
        for k in keys {
            if self.links.local(k.0, k.1).is_some() {
                continue;
            }
            if !self.elk.names.contains_key(&k) {
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
        self.elk.status = match (added, nameless) {
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
        self.elk.status = "Fetching stopped".into();
    }

    /// Start the next download if none is on its way.
    fn pump_fetch(&mut self) {
        if self.pool_fetch.running.is_some() {
            return;
        }
        let Some(cache) = crate::links::pool_cache_dir() else { return };
        while let Some(k) = self.pool_fetch.queue.pop_front() {
            let Some(name) = self.elk.names.get(&k).cloned() else {
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
    }

    /// A device job finished: if it was a preview fetch, play what was
    /// waiting on it and start the next. Called before the status bar
    /// forgets the queue, so a run of fetches shows as one batch.
    pub(super) fn pool_fetch_finished(&mut self, job: u64, ok: bool) {
        if let Some(files) = self.pool_fetch.uploads.remove(&job) {
            for (slot, path) in files {
                match self.links.local_link(&path).map(|l| (l.hash, l.size)) {
                    Some((hash, size)) if self.elk.filling.contains_key(&slot) => self.resolve_fill(slot, hash, size),
                    _ => {
                        self.elk.filling.remove(&slot);
                        self.elk.status = format!("{} didn't make it to the device", path.display());
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
                self.elk.status = "Couldn't fetch that one (moved on the device since the read?)".into();
            }
        } else if !ok {
            // Unplugged or refused: don't run down the rest of the queue
            // into the same wall.
            let left = self.pool_fetch.queue.len();
            self.pool_fetch.queue.clear();
            self.pool_fetch.then = None;
            if left > 0 {
                self.elk.status = format!("Fetch failed; {left} queued fetch(es) dropped");
            }
        } else if let Some((want, open)) = self.pool_fetch.then.take_if(|(w, _)| *w == k) {
            self.elk_local(want, open);
        }
        if self.pool_fetch.done % 10 == 0 {
            self.measure_cache();
        }
        self.pump_fetch();
    }

    /// Mirror the fetch queue into the view.
    fn sync_pool_fetch(&mut self) {
        let f = &self.pool_fetch;
        self.elk.fetching = f.queue.iter().copied().chain(f.running.map(|(_, k)| k)).collect();
        self.elk.fetch = (f.total > 1 && (f.running.is_some() || !f.queue.is_empty())).then_some((f.done, f.total));
        self.elk.cache = crate::links::pool_cache_dir().and_then(|c| match self.sizes.get(&c) {
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
        self.refresh_elk_links();
        if c.is_dir() {
            self.trash_local(vec![c.clone()]);
        }
        self.sizes.set(c, 0, 0);
        self.elk.status = "Preview cache moved to the Trash".into();
    }

    /// Which pool files we have a local copy of, for the view.
    pub(super) fn refresh_elk_links(&mut self) {
        let Some(p) = &self.elk.project else { return };
        let linked: std::collections::HashMap<_, _> = (0..p.pool.len())
            .map(|i| self.elk.entry(i))
            .filter(|e| !e.is_empty())
            .filter_map(|e| {
                let k = e.key();
                let l = self.links.known(k.0, k.1)?;
                Some((k, (l.device_name().to_owned(), l.local.clone(), l.uploaded)))
            })
            .collect();
        if linked.len() != self.elk.linked.len() {
            crate::log::line(format!("elk: {} pool files have a local copy", linked.len()));
        }
        self.elk.linked = linked;
    }

    fn elk_read(&mut self) {
        self.elk.busy = Some(("Reading project".into(), 0, 129));
        let known = self.elk.names.keys().copied().collect();
        self.device.elk(ElkCmd::Read { known });
    }

    pub(super) fn elk_event(&mut self, e: ElkEv) {
        let st = &mut self.elk;
        match e {
            ElkEv::Projects(r) => st.projects = Some(r),
            ElkEv::Partial(p) => {
                st.set_project(p);
                self.refresh_elk_links();
            }
            ElkEv::Uses(v) => {
                if let Some(p) = st.project.as_mut().filter(|p| p.partial && p.uses.len() == v.len()) {
                    p.uses = v;
                }
            }
            ElkEv::Progress { label, done, total } => st.busy = Some((label, done, total)),
            ElkEv::Loaded(r) => {
                st.busy = None;
                match r {
                    Ok(p) => {
                        let used = p.pool.iter().skip(1).filter(|e| !e.is_empty()).count();
                        // Entries the device hasn't given an inode yet (new files pushed
                        // with 0), on any read: tells whether a save + reload fills it in.
                        let unset: Vec<String> = p.pool.iter().enumerate().filter(|(_, e)| !e.is_empty() && e.inode == 0).map(|(i, e)| format!("{i} ({:08x})", e.hash)).collect();
                        crate::log::line(format!("elk: {} pool entries with inode 0: {}", unset.len(), unset.join(", ")));
                        // New files pushed with inode/seqnr 0: what did the device make of them?
                        if let Some(want) = &st.expect {
                            for (i, (w, got)) in want.iter().zip(&p.pool).enumerate() {
                                if !w.is_empty() && w.inode == 0 {
                                    crate::log::line(format!(
                                        "elk: read back new slot {i}: inode {} hash {:08x} size {} seqnr {} (sent hash {:08x} size {})",
                                        got.inode, got.hash, got.size, got.seqnr, w.hash, w.size
                                    ));
                                }
                            }
                        }
                        for (i, e) in p.pool.iter().enumerate().skip(1).filter(|(_, e)| !e.is_empty()).take(12) {
                            crate::log::line(format!(
                                "elk: pool {i}: inode {} hash {:08x} size {} seqnr {}",
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
                            let off: Vec<usize> = (0..want.len()).filter(|&i| got.get(i).is_none_or(|g| *g != &want[i][..])).collect();
                            for &i in off.iter().take(8) {
                                crate::log::line(format!(
                                    "elk-pat: read back {} differs from what was sent: {}",
                                    crate::device::patterns::label(i),
                                    got.get(i).map_or("missing".into(), |g| pats.describe_diff(&want[i], g))
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
                                crate::log::line("elk-pat: read back after push: every pattern matches what was sent".to_owned());
                                st.status = "Pushed and read back: the device has the new patterns. Save on the device (PROJECT › SAVE) to keep it.".into();
                            }
                            Some(Err(e)) => {
                                let msg = format!("{e} (details in the log, elk-pat:). Check on the device before saving; if anything is off, reload the project from +Drive.");
                                self.errors.push(msg.clone());
                                st.status = msg;
                            }
                            None => {}
                        }
                        st.set_project(p);
                        self.refresh_elk_links();
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
            ElkEv::Names(v) => st.names.extend(v),
            ElkEv::Resolved { slot, hash, found } => {
                // Dropped from the slot meanwhile (project reloaded or closed).
                let Some(name) = st.filling.get(&slot).cloned() else { return };
                let prior = self.pool_fetch.prior.remove(&slot);
                if let (Ok(None), Some(path)) = (&found, prior) {
                    // Uploaded once, gone from the device since: send it again.
                    crate::log::line(format!("elk: {} no longer on the device; uploading again", path.display()));
                    self.add_pool_files_upload(vec![(slot, path)]);
                    return;
                }
                match found {
                    Ok(Some((len, path))) => {
                        let e = crate::device::PoolEntry::new_file(hash, len);
                        let n = st.order.len();
                        let dup = (1..n).find(|&s| {
                            let x = st.entry(st.order[s] as usize);
                            !x.is_empty() && x.key() == e.key()
                        });
                        if let Some(at) = dup {
                            st.filling.remove(&slot);
                            st.status = format!("{name} is already in the pool (slot {at:03}); left out");
                        } else {
                            st.fill(slot, e);
                            st.names.insert((hash, len), path.clone());
                            st.status = format!("{name} is ready in its slot; push to send it to the device");
                            self.refresh_elk_links();
                        }
                    }
                    Ok(None) => {
                        st.filling.remove(&slot);
                        st.status = format!("The device can't find {name} by its hash; left out");
                        self.errors.push(format!("pool: the device has no file with hash {hash:08x} after uploading {name}"));
                    }
                    Err(e) => {
                        st.filling.remove(&slot);
                        st.status = format!("Couldn't check {name} on the device: {e}");
                    }
                }
            }
            ElkEv::PushStale => {
                st.busy = None;
                st.expect = None;
                st.expect_patterns = None;
                st.status = "Not pushed: the project on the device changed since it was read".into();
                st.confirm_stale = true;
            }
            ElkEv::Pushed(r) => match r {
                Ok(n) => {
                    st.status = format!("Sent {n} messages; reading back to check…");
                    self.elk_read();
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
