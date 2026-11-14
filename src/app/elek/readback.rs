//! Project-load completion, push read-back diagnostics and status checks.

use super::*;

impl App {
    /// Complete a streamed or one-shot project read after first-dock handling.
    pub(super) fn elek_loaded(&mut self, result: Result<Box<crate::device::ElekProject>, String>) {
        self.elek.busy = None;
        let streamed = self.elek.reading.take().is_some();
        match result {
            Ok(p) => {
                self.check_elek_readback(&p);
                if streamed {
                    self.elek.finish_read(p);
                } else {
                    self.elek.set_project(p);
                }
                self.refresh_elek_links();
            }
            Err(e) => {
                let st = &mut self.elek;
                st.expect = None;
                st.status = format!("Read failed: {e}");
                // A pool shown early stays, but can't be pushed.
                if let Some(p) = st.project.as_mut().filter(|p| p.partial) {
                    p.read_only = Some(format!("the read didn't finish ({e}); load again to push"));
                }
            }
        }
    }

    /// Keep verification and status precedence explicit: samples, sounds,
    /// patterns, then any incomplete-read warning. Pattern diagnostics are
    /// gathered first, as they were in the Loaded event handler.
    pub(super) fn check_elek_readback(&mut self, p: &crate::device::ElekProject) {
        let st = &mut self.elek;
        log_pool_read(p, st.expect.as_deref());
        let pat_check = st
            .expect_patterns
            .take()
            .map(|want| check_pattern_readback(p, want));
        st.status = sample_readback_status(p, st.expect.take(), &mut self.errors);
        if let Some(status) = sound_readback_status(p, st.expect_sounds.take(), &mut self.errors) {
            st.status = status;
        }
        match pat_check {
            Some(Ok(())) => {
                crate::log::line(
                    "elek-pat: read back after push: every pattern matches what was sent"
                        .to_owned(),
                );
                st.status = "Pushed and read back: the device has the new patterns. Save on the device (PROJECT › SAVE) to keep it.".into();
            }
            Some(Err(e)) => {
                let msg = format!("{e} (details in the log, elek-pat:). Check on the device before saving; if anything is off, reload the project from +Drive.");
                self.errors.push(msg.clone());
                st.status = msg;
            }
            None => {}
        }
        if let Some(status) = stopped_read_status(p) {
            st.status = status;
        }
    }
}

/// Diagnostic details retained independently of the user-facing read-back status.
fn log_pool_read(p: &crate::device::ElekProject, want: Option<&[crate::device::PoolEntry]>) {
    // Entries the device hasn't given an inode yet (new files pushed
    // with 0), on any read: tells whether a save + reload fills it in.
    let unset: Vec<String> = p
        .pool
        .iter()
        .enumerate()
        .filter(|(_, e)| !e.is_empty() && e.inode == 0)
        .map(|(i, e)| format!("{i} ({:08x})", e.hash))
        .collect();
    crate::log::line(format!(
        "elek: {} pool entries with inode 0: {}",
        unset.len(),
        unset.join(", ")
    ));
    // New files pushed with inode/seqnr 0: what did the device make of them?
    if let Some(want) = want {
        for (i, (w, got)) in want.iter().zip(&p.pool).enumerate() {
            if !w.is_empty() && w.inode == 0 {
                crate::log::line(format!(
                    "elek: read back new slot {i}: inode {} hash {:08x} size {} seqnr {} (sent hash {:08x} size {})",
                    got.inode, got.hash, got.size, got.seqnr, w.hash, w.size
                ));
            }
        }
    }
    for (i, e) in p
        .pool
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, e)| !e.is_empty())
        .take(12)
    {
        crate::log::line(format!(
            "elek: pool {i}: inode {} hash {:08x} size {} seqnr {}",
            e.inode, e.hash, e.size, e.seqnr
        ));
    }
}

fn check_pattern_readback(
    p: &crate::device::ElekProject,
    want: Vec<Option<Vec<u8>>>,
) -> Result<(), String> {
    let got = p.pattern_data();
    let Ok(pats) = &p.patterns else {
        return Err("patterns couldn't be parsed after the push".to_owned());
    };
    // Slots not loaded on either side (a stopped read) aren't compared.
    let pairs: Vec<(usize, &[u8], &[u8])> = (0..want.len())
        .filter_map(|i| Some((i, want[i].as_deref()?, got.get(i).copied().flatten()?)))
        .collect();
    let off: Vec<usize> = pairs
        .iter()
        .filter(|(_, w, g)| w != g)
        .map(|(i, _, _)| *i)
        .collect();
    for (i, w, g) in pairs.iter().filter(|(_, w, g)| w != g).take(8) {
        crate::log::line(format!(
            "elek-pat: read back {} differs from what was sent: {}",
            crate::device::patterns::label(*i),
            pats.describe_diff(w, g)
        ));
    }
    if off.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} pattern(s) read back different from what was sent",
            off.len()
        ))
    }
}

fn sample_readback_status(
    p: &crate::device::ElekProject,
    want: Option<Vec<crate::device::PoolEntry>>,
    errors: &mut Vec<String>,
) -> String {
    let used = p.pool.iter().skip(1).filter(|e| !e.is_empty()).count();
    // The device may renumber inodes; compare the files.
    let differ = |want: &[crate::device::PoolEntry]| {
        want.iter()
            .zip(&p.pool)
            .filter(|(a, b)| a.is_empty() != b.is_empty() || a.key() != b.key())
            .count()
    };
    match want {
        Some(want) if differ(&want) == 0 => {
            "Pushed and read back: the device has the new order. Save on the device (PROJECT › SAVE) to keep it.".to_owned()
        }
        Some(want) => {
            let off = differ(&want);
            let msg = format!("Read back after push: {off} slots differ from what was sent. Don't save; reload the project from +Drive.");
            errors.push(msg.clone());
            msg
        }
        None => format!("Read {used} samples"),
    }
}

fn sound_readback_status(
    p: &crate::device::ElekProject,
    want: Option<Vec<String>>,
    errors: &mut Vec<String>,
) -> Option<String> {
    if let (Some(want), Some(got)) = (want, p.pool_sounds()) {
        let off: Vec<String> = want
            .iter()
            .zip(&got)
            .enumerate()
            .filter(|(_, (w, g))| {
                **w != if g.blank {
                    String::new()
                } else {
                    g.name.clone()
                }
            })
            .map(|(i, (w, g))| format!("{:03} sent {w:?} read {:?}", i + 1, g.name))
            .collect();
        if off.is_empty() {
            crate::log::line(
                "elek-lib: read back after push: the sound pool matches what was sent",
            );
            return Some("Pushed and read back: the device has the new sound pool. Save on the device (PROJECT › SAVE) to keep it.".into());
        } else {
            crate::log::line(format!(
                "elek-lib: read back after push, sound pool differs: {}",
                off.join("; ")
            ));
            let msg = format!("{} pool sound(s) read back different from what was sent (details in the log, elek-lib:). Check on the device before saving.", off.len());
            errors.push(msg.clone());
            return Some(msg);
        }
    }
    None
}

fn stopped_read_status(p: &crate::device::ElekProject) -> Option<String> {
    if p.partial {
        let (n, banks) = match &p.patterns {
            Ok(pats) => (
                pats.loaded_count(),
                (0..8)
                    .filter(|&b| pats.bank_loaded(b))
                    .map(|b| ((b'A' + b as u8) as char).to_string())
                    .collect::<Vec<_>>(),
            ),
            Err(_) => (0, Vec::new()),
        };
        return Some(if banks.is_empty() {
            format!("Read stopped after {n} of 128 patterns: no bank loaded completely, so nothing can be pushed. Reload to read the rest.")
        } else {
            format!("Read stopped after {n} of 128 patterns. Bank {} can be edited and pushed; reload to read the rest.", banks.join(", "))
        });
    } else if !p.sounds_read {
        return Some("Read stopped before the sound pool was in: patterns can be pushed, sample pool changes can't (they move the samples pool sounds play). Reload to read it.".into());
    }
    None
}
