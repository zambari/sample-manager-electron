//! Worker device events landing in cached application state.

use super::*;

impl App {
    pub(in crate::app) fn elek_event(&mut self, e: ElekEv) {
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
            ElekEv::PresetBanks(result) => {
                st.presets.end_load();
                st.presets.banks = Some(result);
            }
            ElekEv::Presets { bank, result } => {
                st.presets.end_load();
                st.presets.listings.insert(bank, Some(result));
            }
            ElekEv::Preset { bank, slot, result } => {
                st.presets.end_load();
                if let Err(e) = &result {
                    st.presets.status = format!("Could not load preset {bank}:{slot:03}: {e}. Reload presets to retry.");
                }
                if let Ok(preset) = &result {
                    if let Some((key, Some(path))) = &preset.sample {
                        st.names.insert(*key, path.clone());
                    }
                }
                st.presets.sounds.insert((bank, slot), Some(result));
            }
            ElekEv::PresetReadProgress(p) => st.presets.read_progress = p,
            ElekEv::PresetTags { bank, slot, result } => {
                // A reload can discard a background read while it is in flight.
                if let Some(tags) = st.presets.tag_masks.get_mut(&(bank, slot)) {
                    *tags = Some(result);
                }
            }
            ElekEv::PresetDumpProgress { label, done, total } => {
                st.presets.dumping = Some((label, done, total));
            }
            ElekEv::PresetsDumped(result) => {
                st.presets.dumping = None;
                match result {
                    Ok(dir) => {
                        crate::log::line(format!("presets: debug dump saved to {}", dir.display()));
                        st.presets.status = format!("Preset debug dump saved to {}", dir.display());
                        reveal(&dir);
                    }
                    Err(e) => {
                        st.presets.status = format!("Couldn't save preset debug dump: {e}");
                        self.errors.push(format!("save preset dump: {e}"));
                    }
                }
            }
            ElekEv::PresetFileSaved(result) => {
                st.presets.dumping = None;
                st.presets.status = match result {
                    Ok(path) => {
                        crate::log::line(format!("presets: backup saved to {}", path.display()));
                        format!("Importable preset backup saved to {}", path.display())
                    }
                    Err(e) => {
                        self.errors.push(format!("save presets: {e}"));
                        format!("Couldn't save preset backup: {e}")
                    }
                };
            }
            ElekEv::PresetFileLoaded(result) => {
                st.presets.dumping = None;
                st.presets.importing = false;
                match result {
                    Ok(loaded) => st.presets.load_file(loaded),
                    Err(e) => {
                        st.presets.status = format!("Couldn't load preset backup: {e}");
                        self.errors.push(format!("load presets: {e}"));
                    }
                }
            }
            ElekEv::PresetPushProgress { label, done, total } => {
                st.presets.pushing = Some((label, done, total));
            }
            ElekEv::PresetsPushed(result) => {
                st.presets.pushing = None;
                match result {
                    Ok(()) => {
                        st.presets.reload();
                        st.presets.status = "Presets pushed to +Drive and verified.".into();
                    }
                    Err(e) => {
                        st.presets.push_failed = true;
                        st.presets.status = format!("Preset push failed: {e} Some edits may already be on the device. Reload presets before trying again.");
                    }
                }
            }
            ElekEv::Projects(r) => st.projects = Some(r),
            ElekEv::SoundProgress { done, total } => st.lib.sound_progress = Some((done, total)),
            ElekEv::Sounds(r) => {
                st.lib.sound_progress = None;
                st.status = match &r {
                    Ok(s) => format!("Sound pool read: {} sounds in use", s.iter().filter(|s| !s.blank).count()),
                    Err(e) => format!("Sound pool read failed: {e}"),
                };
                st.lib.pool_sounds = Some(r);
            }
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
            ElekEv::Loaded(r) => self.elek_loaded(r),
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
                            self.update_landed(slot);
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
                st.keep_blank.clear();
                st.expect = None;
                st.expect_patterns = None;
                st.status = "Not pushed: the project on the device changed since it was read".into();
                st.confirm_stale = true;
            }
            ElekEv::Pushed(r) => match r {
                Ok(n) => {
                    st.status = format!("Sent {n} messages; reading back to check…");
                    let cached = std::mem::take(&mut st.keep_blank);
                    self.elek_read(cached);
                }
                Err(e) => {
                    st.busy = None;
                    st.expect = None;
                    st.expect_patterns = None;
                    st.expect_sounds = None;
                    st.keep_blank.clear();
                    st.status = format!("Push failed: {e}");
                    self.errors.push(format!("push to device: {e}"));
                }
            },
        }
    }
}
