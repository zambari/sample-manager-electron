//! Pool uploads, preview fetch queue, cache and edited local sample updates.

use super::*;

impl App {
    /// Play or open pool file `k`: from a local copy when there is one,
    /// else fetch it into the preview cache first (ahead of the queue).
    pub(super) fn elek_local(&mut self, k: Key, open: bool) {
        if let Some(l) = self.links.local(k.0, k.1) {
            let (local, note) = (l.local.clone(), super::editing::link_note(l));
            if open {
                self.elek_mode = false;
                self.note_pool_edit(k, &local);
                self.load_into_editor(&local);
            } else {
                self.preview_device_sample(&local);
            }
            self.status = note;
            return;
        }
        // Silence the editor immediately, including while the preview downloads.
        self.engine.stop();
        self.editor.play_when_loaded = false;
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
    pub(in crate::app) fn pool_upload_dir(&self) -> Option<PathBuf> {
        let active = [Role::Source, Role::Dest].into_iter().map(|r| self.pane(r).active_tab());
        let any = [Role::Source, Role::Dest].into_iter().flat_map(|r| self.pane(r).tabs.iter());
        active.chain(any).find(|t| t.device).map(|t| t.cwd.clone())
    }

    /// Files dropped onto empty pool slots: ones already on the +Drive
    /// (uploaded earlier and unchanged) are looked up right away, the rest
    /// are uploaded first.
    pub(super) fn add_pool_files(&mut self, pairs: Vec<(u16, PathBuf)>) {
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

    pub(super) fn add_pool_files_upload(&mut self, upload: Vec<(u16, PathBuf)>) {
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
    pub(super) fn resolve_fill(&mut self, slot: u16, hash: u32, size: u32) {
        let mut lens = vec![size, size.saturating_sub(64), size + 64];
        lens.retain(|&l| l > 0);
        lens.dedup();
        self.device.elek(ElekCmd::Resolve { slot, hash, lens });
    }

    /// Queue every pool file without a usable local copy.
    pub(super) fn fetch_all(&mut self) {
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

    pub(super) fn stop_fetch(&mut self) {
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
    pub(super) fn pump_fetch(&mut self) {
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
    pub(in crate::app) fn pool_fetch_finished(&mut self, job: u64, ok: bool) {
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
    pub(super) fn sync_pool_fetch(&mut self) {
        let f = &self.pool_fetch;
        self.elek.fetching = f.queue.iter().copied().chain(f.running.map(|(_, k)| k)).collect();
        self.elek.fetch = (f.total > 1 && (f.running.is_some() || !f.queue.is_empty())).then_some((f.done, f.total));
        self.elek.cache = crate::links::pool_cache_dir().and_then(|c| match self.sizes.get(&c) {
            Some(crate::sizes::Size::Known { bytes, files }) => Some((bytes, files)),
            _ => None,
        });
    }

    /// Ask for the cache folder's size again.
    pub(in crate::app) fn measure_cache(&mut self) {
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

    pub(super) fn purge_cache(&mut self) {
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

    /// Every couple of seconds while a project is loaded: which pool
    /// samples' local files were edited since they went to the device.
    /// A stat per linked pool file, so it's cheap.
    pub(in crate::app) fn check_changed(&mut self, ctx: &egui::Context) {
        const EVERY: std::time::Duration = std::time::Duration::from_secs(2);
        let Some(p) = &self.elek.project else {
            if !self.changed_paths.is_empty() {
                self.changed_paths.clear();
                self.elek.changed_local.clear();
            }
            return;
        };
        if self.changed_at.is_some_and(|t| t.elapsed() < EVERY) {
            return;
        }
        self.changed_at = Some(std::time::Instant::now());
        ctx.request_repaint_after(EVERY);
        let changed: HashMap<(u32, u32), PathBuf> = (1..p.pool.len())
            .map(|i| self.elek.entry(i))
            .filter(|e| !e.is_empty())
            .filter_map(|e| {
                let k = e.key();
                Some((k, self.links.changed_copy(k.0, k.1)?.local.clone()))
            })
            .collect();
        self.changed_paths = changed.values().cloned().collect();
        self.elek.changed_local = changed;
    }

    /// Send the edited local file of each of these pool samples to its
    /// slot: uploaded next to the +Drive original (kept: other projects may
    /// use it), filled in, then one push once they've all landed.
    pub(super) fn update_changed(&mut self, keys: Vec<(u32, u32)>) {
        if !self.elek.pool_editable() {
            self.elek.status = "The pool can't take new files right now".into();
            return;
        }
        // Earlier ones that failed or were left out aren't coming.
        let filling = &self.elek.filling;
        self.update_push.retain(|s| filling.contains_key(s));
        let mut by_dir: HashMap<PathBuf, Vec<(u16, PathBuf)>> = HashMap::new();
        let mut started = 0;
        for k in keys {
            let (Some(old), Some(local)) = (self.elek.slot_of(k), self.elek.changed_local.get(&k).cloned()) else {
                continue;
            };
            if self.elek.filling.contains_key(&old) {
                continue;
            }
            let name = local.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            self.elek.filling.insert(old, name);
            self.update_push.insert(old);
            started += 1;
            self.pool_fetch.prior.remove(&old);
            // Already on the +Drive as it is now (sent earlier by hand).
            if let Some((hash, size)) = self.links.local_link(&local).map(|l| (l.hash, l.size)) {
                self.pool_fetch.prior.insert(old, local);
                self.resolve_fill(old, hash, size);
                continue;
            }
            let dir = self
                .elek
                .names
                .get(&k)
                .and_then(|p| Path::new(p).parent().map(Path::to_path_buf))
                .or_else(|| self.pool_upload_dir());
            match dir {
                Some(d) => by_dir.entry(d).or_default().push((old, local)),
                None => {
                    self.elek.filling.remove(&old);
                    self.update_push.remove(&old);
                    started -= 1;
                    self.elek.status = "Open the device's +Drive in a file tab first, at the folder new samples should go to".into();
                }
            }
        }
        for (dir, upload) in by_dir {
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
            let id = self.run_device(task, &format!("Updating {n} edited sample(s) on the device"), n, bytes);
            self.pool_fetch.uploads.insert(id, upload);
        }
        if started > 0 {
            self.elek.status = format!("Sending {started} edited sample(s) to their slots…");
        }
    }

    /// A slot taking an edited file has its new sample (or gave up): once
    /// none is left on the way, push.
    pub(super) fn update_landed(&mut self, old: u16) {
        if !self.update_push.remove(&old) {
            return;
        }
        // Failed ones drop out of `filling`: stop waiting for them.
        let filling = &self.elek.filling;
        self.update_push.retain(|s| filling.contains_key(s));
        if !self.update_push.is_empty() {
            return;
        }
        self.changed_at = None;
        let st = &mut self.elek;
        if st.live {
            st.status = "Edited samples are in their slots; going to the device".into();
        } else if st.busy.is_some() || !st.pool_pushable() {
            st.status = "Edited samples are in their slots; push when the device is free".into();
        } else {
            st.status = "Pushing the edited samples to the device…".into();
            self.elek_push(false, false);
        }
    }

    /// Which pool files we have a local copy of, for the view.
    pub(in crate::app) fn refresh_elek_links(&mut self) {
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
        self.changed_at = None;
    }
}
