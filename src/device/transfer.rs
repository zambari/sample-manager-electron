//! Moving files to, from and around the device: uploads, downloads, copies,
//! moves and deletes, one file at a time with retries.

use super::*;

impl Worker {
    /// Look up one device item by listing its folder.
    pub(super) fn stat(&mut self, p: &Path) -> Result<DirEntry, String> {
        let parent = p.parent().unwrap_or(Path::new("/")).to_path_buf();
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.with_conn(|c| c.list(&parent))?
            .into_iter()
            .find(|e| e.name == name)
            .ok_or_else(|| format!("{} no longer exists", dev_str(p)))
    }

    /// Settle where `name` goes in device folder `dir` under `policy`.
    /// `Some((path, existing))`, or `None` to skip. Replacing a file deletes
    /// it first; an existing folder is merged into. On a merge, an existing
    /// file comes back as `existing` for the caller to compare sizes.
    pub(super) fn place_on_device(
        &mut self,
        dir: &Path,
        name: &str,
        is_dir: bool,
        policy: Policy,
    ) -> Result<Option<(PathBuf, Option<DirEntry>)>, String> {
        let listing = self.with_conn(|c| c.list(dir))?;
        let Some(existing) = listing.iter().find(|e| e.name == name) else {
            return Ok(Some((dir.join(name), None)));
        };
        match policy {
            Policy::Skip => Ok(None),
            Policy::Merge if is_dir == existing.is_dir => {
                Ok(Some((dir.join(name), Some(existing.clone()))))
            }
            Policy::Merge => {
                self.left_alone(&dir.join(name));
                Ok(None)
            }
            Policy::KeepBoth => {
                let taken: HashSet<String> = listing.iter().map(|e| e.name.clone()).collect();
                Ok(Some((dir.join(unique_name(&taken, name)), None)))
            }
            Policy::Replace if is_dir && existing.is_dir => {
                Ok(Some((dir.join(name), Some(existing.clone()))))
            }
            Policy::Replace if !is_dir && !existing.is_dir => {
                let p = dir.join(name);
                self.with_conn(|c| c.delete_file(&p))?;
                Ok(Some((p, None)))
            }
            Policy::Replace => Err(format!(
                "{}: a {} of that name is in the way",
                dev_str(&dir.join(name)),
                if existing.is_dir { "folder" } else { "file" }
            )),
        }
    }

    /// Merge: a file already there at the expected size. Counted as done.
    pub(super) fn already_there(&mut self, bytes: u64) {
        self.merged.1 += 1;
        if let Some(p) = self.job.as_mut() {
            p.done += bytes;
        }
        self.file_done();
    }

    /// Merge: same name, different size (or file vs folder): left alone.
    pub(super) fn left_alone(&mut self, at: &Path) {
        self.merged.2 += 1;
        crate::log::line(format!(
            "merge: left in place (same name, different size): {}",
            dev_str(at)
        ));
    }

    pub(super) fn move_item(
        &mut self,
        src: &Path,
        dst_dir: &Path,
        policy: Policy,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        if dst_dir.starts_with(src) {
            return Err(format!(
                "{}: cannot move a folder into itself",
                dev_str(src)
            ));
        }
        if src.parent() == Some(dst_dir) {
            return Ok(());
        }
        let entry = self.stat(src)?;
        let name = entry.name.clone();
        self.progress(&name, 0);
        // A rename can't merge folders: an existing folder is in the way.
        let policy = if entry.is_dir && policy == Policy::Replace {
            Policy::KeepBoth
        } else {
            policy
        };
        let Some((target, _)) = self.place_on_device(dst_dir, &name, entry.is_dir, policy)? else {
            return Ok(());
        };
        self.relocate(src, &target, entry.is_dir, entry.size as u64, cancel)
    }

    /// Delete `p` from the device; a folder is emptied first, since the
    /// device only removes empty ones.
    pub(super) fn remove(
        &mut self,
        p: &Path,
        is_dir: bool,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        self.progress(&Self::short(p), 0);
        if is_dir {
            let kids = self.with_conn(|c| c.list(p))?;
            for k in kids {
                // Stop at the first failure, leaving the rest in place.
                self.remove(&p.join(&k.name), k.is_dir, cancel)?;
            }
            self.with_conn(|c| c.rmdir(p))
        } else {
            self.with_conn(|c| c.delete_file(p))?;
            self.file_done();
            Ok(())
        }
    }

    /// `parent/name` of a device path, for the status bar.
    pub(super) fn short(p: &Path) -> String {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match p.parent().and_then(|d| d.file_name()) {
            Some(d) => format!("{}/{name}", d.to_string_lossy()),
            None => name,
        }
    }

    /// Move `src` to `target` (a free path). The device refuses to rename a
    /// folder into another folder, so when a rename is refused, rebuild the
    /// item at `target` piece by piece and remove the source. `size` is the
    /// file's size (0 for folders), counted into progress once it has moved.
    pub(super) fn relocate(
        &mut self,
        src: &Path,
        target: &Path,
        is_dir: bool,
        size: u64,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let renamed = self.with_conn(|c| match c.rename(src, target) {
            Ok(()) => Ok(true),
            Err(Fail::Refused(_)) => Ok(false),
            Err(e) => Err(e),
        })?;
        if renamed {
            self.progress("", size);
            if !is_dir {
                self.file_done();
            }
            return Ok(());
        }
        crate::log::line(format!(
            "device: rename refused, moving {} piece by piece",
            dev_str(src)
        ));
        if is_dir {
            self.progress(&format!("{}/…", Self::short(src)), 0);
            self.with_conn(|c| c.mkdir(target))?;
            let kids = self.with_conn(|c| c.list(src))?;
            for k in kids {
                // Each child gets its own line, so a big folder visibly moves.
                self.progress(&Self::short(&src.join(&k.name)), 0);
                // Stop at the first failure so the source keeps what didn't move.
                self.relocate(
                    &src.join(&k.name),
                    &target.join(&k.name),
                    k.is_dir,
                    k.size as u64,
                    cancel,
                )?;
            }
            self.with_conn(|c| c.rmdir(src))?;
        } else {
            self.progress(&Self::short(src), 0);
            // Read and write each count half, so the file adds up to its size once.
            let raw = self.read_raw(src, true, cancel)?;
            self.write_raw(target, &raw, true, None, cancel)?;
            self.with_conn(|c| c.delete_file(src))?;
            self.file_done();
        }
        Ok(())
    }

    pub(super) fn upload(
        &mut self,
        src: &Path,
        dst_dir: &Path,
        policy: Policy,
        mono: Option<crate::audio::stats::Fold>,
        cancel: &AtomicBool,
        errors: &mut Vec<String>,
    ) -> Result<(), String> {
        if src.is_dir() {
            let name = src
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let Some((target, existing)) = self.place_on_device(dst_dir, &name, true, policy)?
            else {
                return Ok(());
            };
            if existing.is_none() {
                self.with_conn(|c| c.mkdir(&target))?;
            }
            let mut kids: Vec<PathBuf> = std::fs::read_dir(src)
                .map_err(|e| format!("{}: {e}", src.display()))?
                .filter_map(|d| d.ok().map(|d| d.path()))
                .filter(|p| {
                    !p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
                })
                .filter(|p| p.is_dir() || crate::fs_model::is_audio_path(p))
                .collect();
            kids.sort();
            for k in kids {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                if let Err(e) = self.upload(&k, &target, policy, mono, cancel, errors) {
                    errors.push(e);
                }
            }
            return Ok(());
        }

        let name = device_name(src);
        self.progress(&name, 0);
        let mut s =
            crate::audio::decode::decode(src).map_err(|e| format!("{}: {e}", src.display()))?;
        let folded = mono.is_some() && s.channels.len() > 1;
        if let Some(fold) = mono.filter(|_| s.channels.len() > 1) {
            let one = (0..s.frames()).map(|i| fold.at(&s.channels, i)).collect();
            s.channels = vec![one];
        }
        let Some((target, existing)) = self.place_on_device(dst_dir, &name, false, policy)? else {
            return Ok(());
        };
        // Known only once connected, which placing the file made sure of.
        let stereo = mono.is_none() && self.conn.as_ref().is_some_and(|c| c.stereo);
        let raw = sample::to_device(&s, stereo);
        // What goes over the wire (converted, maybe folded) isn't the file's
        // size, which is what the bar's total counts: scale it to that, so
        // the bar doesn't jump when the next file starts.
        let worth = std::fs::metadata(src)
            .map(|m| m.len())
            .unwrap_or(raw.len() as u64);
        if let Some(e) = existing {
            // Merging onto a file of that name: compare with what we'd send,
            // with or without the sample header.
            if [raw.len(), raw.len() - sample::HEADER_LEN].contains(&(e.size as usize)) {
                self.already_there(worth);
            } else {
                self.left_alone(&target);
            }
            return Ok(());
        }
        self.write_raw(&target, &raw, false, Some(worth), cancel)?;
        // The device's hash for what landed, read back from its folder.
        if let Ok(e) = self.stat(&target) {
            self.note_link(e.hash, e.size, &target, src, true, folded);
        }
        self.file_done();
        Ok(())
    }

    /// Remember that device file `device` matches local file `local`.
    fn note_link(&mut self, hash: u32, size: u32, device: &Path, local: &Path, uploaded: bool, folded: bool) {
        if let Some(l) = crate::links::Link::new(hash, size, dev_str(device), local, uploaded, folded) {
            crate::links::append(&l);
            self.em.send(Ev::Linked(l));
        }
    }

    /// `half`: this write follows a read of the same bytes (a device-to-device
    /// copy), and each counts for half. `worth`: the bar bytes the whole
    /// write counts for, when not its own length.
    pub(super) fn write_raw(
        &mut self,
        target: &Path,
        raw: &[u8],
        half: bool,
        worth: Option<u64>,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        let len = raw.len().max(1) as u64;
        let mut sent = 0u64;
        if let Some(p) = self.job.as_mut() {
            p.file = None;
        }
        // `progress` borrows self, so track bytes locally while the
        // connection is borrowed and fold them back in after.
        let mut prog = self.job;
        let (em, rx, later, busy) = (&self.em, &self.rx, &mut self.later, &self.busy);
        let mut on = |c: &mut Conn, n: u64, file_total: u64| {
            serve_lists(c, rx, later, busy, em);
            if let Some(p) = prog.as_mut() {
                p.done += match worth {
                    Some(w) => w * (sent + n) / len - w * sent / len,
                    None if half => n - n / 2,
                    None => n,
                };
                sent += n;
                let file_done = p.file.filter(|f| f.0 == "sending").map_or(0, |f| f.1) + n;
                p.file = Some(("sending", file_done, file_total));
                // Every chunk: they are slow enough that this is cheap, and
                // small samples would otherwise never show movement.
                p.last = p.done;
                em.send(p.ev(""));
            }
        };
        let Some(c) = self.conn.as_mut() else {
            return Err("not connected".into());
        };
        let res = c.write(target, raw, cancel, &mut on);
        let res = match res {
            Ok(()) => Ok(()),
            Err(Fail::Refused(e)) => Err(e),
            Err(Fail::Link(e)) => Err(self.link_failed(e)),
        };
        if prog.is_some() {
            self.job = prog;
        }
        res
    }

    /// `half`: see [`Self::write_raw`].
    pub(super) fn read_raw(
        &mut self,
        src: &Path,
        half: bool,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, String> {
        if let Some(p) = self.job.as_mut() {
            p.file = None;
        }
        // A file that still fails after its chunk retries is read again from
        // the start (reopened), with its progress rolled back first.
        let start = self.job;
        let mut attempt = 1;
        loop {
            let res = self.read_once(src, half, cancel);
            match res {
                Err(e)
                    if attempt < FILE_TRIES
                        && self.conn.is_some()
                        && !cancel.load(Ordering::Relaxed)
                        && e.starts_with("read failed") =>
                {
                    crate::log::line(format!("device: {e} (attempt {attempt}), reading it again"));
                    attempt += 1;
                    if start.is_some() {
                        self.job = start;
                        if let Some(p) = self.job.as_mut() {
                            p.file = None;
                            self.em.send(p.ev(""));
                        }
                    }
                    std::thread::sleep(Duration::from_millis(300));
                }
                res => {
                    if res.is_ok() && attempt > 1 {
                        crate::log::line(format!(
                            "device: {} read fine on attempt {attempt}",
                            dev_str(src)
                        ));
                    }
                    return res;
                }
            }
        }
    }

    pub(super) fn read_once(
        &mut self,
        src: &Path,
        half: bool,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, String> {
        // `progress` borrows self, so track bytes locally while the
        // connection is borrowed and fold them back in after.
        let mut prog = self.job;
        let (em, rx, later, busy) = (&self.em, &self.rx, &mut self.later, &self.busy);
        let mut on = |c: &mut Conn, n: u64, file_total: u64| {
            serve_lists(c, rx, later, busy, em);
            if let Some(p) = prog.as_mut() {
                p.done += if half { n / 2 } else { n };
                let file_done = p.file.filter(|f| f.0 == "reading").map_or(0, |f| f.1) + n;
                p.file = Some(("reading", file_done, file_total));
                // Every chunk: they are slow enough that this is cheap, and
                // small samples would otherwise never show movement.
                p.last = p.done;
                em.send(p.ev(""));
            }
        };
        let Some(c) = self.conn.as_mut() else {
            return Err("not connected".into());
        };
        let res = c.read(src, cancel, &mut on);
        let res = match res {
            Ok(v) => Ok(v),
            Err(Fail::Refused(e)) => Err(e),
            Err(Fail::Link(e)) => Err(self.link_failed(e)),
        };
        if prog.is_some() {
            self.job = prog;
        }
        res
    }

    pub(super) fn download(
        &mut self,
        src: &Path,
        dst_dir: &Path,
        policy: Policy,
        cancel: &AtomicBool,
        errors: &mut Vec<String>,
    ) -> Result<(), String> {
        let entry = self.stat(src)?;
        if entry.is_dir {
            let target = dst_dir.join(&entry.name);
            let target = match (target.exists(), policy) {
                (false, _) | (true, Policy::Replace | Policy::Merge) => target,
                (true, Policy::Skip) => return Ok(()),
                (true, Policy::KeepBoth) => crate::fs_model::unique_sibling(&target),
            };
            if policy == Policy::Merge && target.exists() && !target.is_dir() {
                self.left_alone(&target);
                return Ok(());
            }
            std::fs::create_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
            let kids = self.with_conn(|c| c.list(src))?;
            for k in kids {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                if let Err(e) = self.download(&src.join(&k.name), &target, policy, cancel, errors) {
                    errors.push(e);
                }
            }
            return Ok(());
        }

        let target = dst_dir.join(wav_name(&entry.name));
        let target = match (target.exists(), policy) {
            (false, _) | (true, Policy::Replace) => target,
            (true, Policy::Skip) => return Ok(()),
            (true, Policy::KeepBoth) => crate::fs_model::unique_sibling(&target),
            (true, Policy::Merge) => {
                // The WAV we'd write: a 44-byte header plus the sample data,
                // whether or not the device's size counts its own header.
                let have = std::fs::metadata(&target).map_or(0, |m| m.len());
                let data = entry.size as u64;
                if [
                    data + 44,
                    (data + 44).saturating_sub(sample::HEADER_LEN as u64),
                ]
                .contains(&have)
                {
                    self.already_there(data);
                } else {
                    self.left_alone(&target);
                }
                return Ok(());
            }
        };
        self.progress(&entry.name, 0);
        let raw = self.read_raw(src, false, cancel)?;
        std::fs::create_dir_all(dst_dir).map_err(|e| format!("{}: {e}", dst_dir.display()))?;
        sample::write_wav(&raw, &target).map_err(|e| format!("{}: {e}", target.display()))?;
        self.note_link(entry.hash, entry.size, src, &target, false, false);
        self.file_done();
        Ok(())
    }

    pub(super) fn copy(
        &mut self,
        src: &Path,
        dst_dir: &Path,
        policy: Policy,
        cancel: &AtomicBool,
        errors: &mut Vec<String>,
    ) -> Result<(), String> {
        if dst_dir.starts_with(src) {
            return Err(format!(
                "{}: cannot copy a folder into itself",
                dev_str(src)
            ));
        }
        let entry = self.stat(src)?;
        // Copying into the same folder always makes a second item.
        let policy = if src.parent() == Some(dst_dir) {
            Policy::KeepBoth
        } else {
            policy
        };
        let Some((target, existing)) =
            self.place_on_device(dst_dir, &entry.name, entry.is_dir, policy)?
        else {
            return Ok(());
        };
        if entry.is_dir {
            if existing.is_none() {
                self.with_conn(|c| c.mkdir(&target))?;
            }
            let kids = self.with_conn(|c| c.list(src))?;
            for k in kids {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                if let Err(e) = self.copy(&src.join(&k.name), &target, policy, cancel, errors) {
                    errors.push(e);
                }
            }
            return Ok(());
        }
        if let Some(e) = existing {
            if e.size == entry.size {
                self.already_there(entry.size as u64);
            } else {
                self.left_alone(&target);
            }
            return Ok(());
        }
        self.progress(&entry.name, 0);
        let raw = self.read_raw(src, true, cancel)?;
        self.write_raw(&target, &raw, true, None, cancel)?;
        self.file_done();
        Ok(())
    }
}
