//! Project/pattern JSON files, sample bundles and bundled-sample imports.

use super::*;

impl App {
    /// Where project and pattern files go by default.
    pub(super) fn elek_files_dir(sub: &str) -> Option<PathBuf> {
        let d = PathBuf::from(std::env::var_os("HOME")?).join("Documents/sample_manager").join(sub);
        let _ = std::fs::create_dir_all(&d);
        Some(d)
    }

    pub(super) fn json_dialog(sub: &str, what: &str) -> rfd::FileDialog {
        let mut d = rfd::FileDialog::new().add_filter(what, &["json"]);
        if let Some(dir) = Self::elek_files_dir(sub) {
            d = d.set_directory(dir);
        }
        d
    }

    /// Asks where to save: a `.json` file, or for a bundle a folder of that
    /// name (the JSON goes inside it, beside `samples/`).
    pub(super) fn ask_save_path(sub: &str, what: &str, base: &str, bundle: bool) -> Option<PathBuf> {
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

    pub(super) fn save_project_file(&mut self, bundle: bool) {
        let Some(p) = &self.elek.project else { return };
        let base = format!("{} project", p.model.name());
        let Some(path) = Self::ask_save_path("projects", "Project", &base, bundle) else { return };
        let st = &self.elek;
        let file = p.project_file(&st.order, &st.fills, &st.proj.cells, &st.lib.sounds, &st.new_pool(), &st.names);
        self.save_elek_file(file, path, bundle, "Project".into());
    }

    pub(super) fn save_pattern_file(&mut self, slot: usize, name: &str, bundle: bool) {
        let Some(p) = &self.elek.project else { return };
        let label = crate::device::patterns::label(slot);
        let base = if name.trim().is_empty() { label.clone() } else { format!("{label} {}", name.trim()) };
        let base: String = base.chars().map(|c| if "/\\:".contains(c) { '_' } else { c }).collect();
        let Some(path) = Self::ask_save_path("patterns", "Pattern", &base, bundle) else { return };
        let st = &self.elek;
        let file = p.pattern_file(slot, name, &st.order, &st.fills, &st.proj.cells, &st.lib.sounds, &st.new_pool(), &st.names);
        self.save_elek_file(file, path, bundle, label);
    }

    /// Write a built file, or for a bundle gather its samples first:
    /// local copies where we have them, the rest fetched from the device
    /// into the preview cache (`finish_bundle` runs once they're in).
    pub(super) fn save_elek_file(&mut self, file: Result<file::SaveFile, String>, path: PathBuf, bundle: bool, what: String) {
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
    pub(super) fn bundle_progress(&mut self) {
        let f = &self.pool_fetch;
        let pending: HashSet<Key> = f.queue.iter().copied().chain(f.running.map(|(_, k)| k)).collect();
        let Some(b) = self.pool_fetch.bundle.as_mut() else { return };
        b.waiting.retain(|k| pending.contains(k));
        if b.waiting.is_empty() {
            let b = self.pool_fetch.bundle.take().expect("checked above");
            self.finish_bundle(b);
        }
    }

    pub(super) fn finish_bundle(&mut self, mut b: BundleSave) {
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
    pub(super) fn importing(&self) -> bool {
        self.pool_fetch.import.as_ref().is_some_and(|i| i.slots.keys().any(|s| self.elek.filling.contains_key(s)))
    }

    /// +Drive folder for samples a bundle brings that the device lacks.
    pub(super) fn import_dir(json: &Path) -> PathBuf {
        let stem = json.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "bundle".into());
        let stem: String = stem.chars().map(|c| if "/\\".contains(c) { '_' } else { c }).collect();
        PathBuf::from("/imported").join(stem.trim())
    }

    /// Start placing `slots` (`(slot as read, its sample)`): each is looked
    /// up on the device by hash, and uploaded from the bundle if missing.
    /// `verify`: the slot already points at the file (a loaded project);
    /// otherwise it's an empty slot to fill.
    pub(super) fn start_import(&mut self, what: String, json: &Path, slots: Vec<(u16, file::SampleRef)>, verify: bool) {
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

    pub(super) fn import_resolved(&mut self, slot: u16, hash: u32, found: Result<Option<(u32, String)>, String>) {
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

    pub(super) fn import_upload(&mut self, slot: u16, file: PathBuf, dir: PathBuf) {
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
    pub(super) fn import_done(&mut self, slot: u16, err: Option<String>) {
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

    pub(super) fn import_summary(&mut self) {
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

    pub(super) fn load_project_file(&mut self) {
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
    pub(super) fn load_pattern_file(&mut self, slot: usize) {
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
}
