//! Kit gain: in a kit the gain slider scales only the selected audio, baked
//! into the samples (one undo step per gesture), instead of the whole file.

use super::*;

/// The gain being dialed in on one selection, against the audio it started from.
pub(super) struct SelGain {
    /// The whole file before this gain, and the selection it applies to
    /// (whole-file frames).
    base: Loaded,
    range: (usize, usize),
    /// The revision this gain produced; any other edit starts a fresh one.
    rev: u64,
    db: f32,
}

impl Editor {
    /// The selection in whole-file frames.
    fn sel_whole(&self) -> (usize, usize) {
        let off = self.crop_offset();
        (self.sel.0 + off, self.sel.1 + off)
    }

    /// The gain on the current selection, while it's still being dialed in;
    /// 0 once anything else changed (another selection, an edit, undo).
    pub fn sel_gain_db(&self) -> f32 {
        self.sel_gain
            .as_ref()
            .filter(|g| g.rev == self.rev && g.range == self.sel_whole())
            .map_or(0.0, |g| g.db)
    }

    /// The base audio for a gain on the current selection.
    fn sel_gain_base(&self) -> Option<Loaded> {
        match &self.sel_gain {
            Some(g) if g.rev == self.rev && g.range == self.sel_whole() => Some(g.base.clone()),
            _ => self.full.clone(),
        }
    }

    /// Scale the selected audio by `db` against how it was before the gain.
    pub fn set_sel_gain(&mut self, db: f32) {
        let Some(base) = self.sel_gain_base() else { return };
        let (a, b) = self.sel_whole();
        let fs = &base.sample;
        let b = b.min(fs.frames());
        if b <= a {
            return;
        }
        let g = stats::from_db(db);
        let channels = fs
            .channels
            .iter()
            .map(|c| {
                let mut v = c.clone();
                v[a..b].iter_mut().for_each(|x| *x *= g);
                v
            })
            .collect();
        let sample = Sample {
            path: fs.path.clone(),
            rate: fs.rate,
            channels,
            bits: fs.bits,
            format: fs.format.clone(),
            file_len: fs.file_len,
        };
        self.replace_audio(Loaded::new(sample));
        self.sel_gain = Some(SelGain {
            base,
            range: (a, b),
            rev: self.rev,
            db,
        });
    }

    /// Kit normalize: gain the selection so it peaks at the target.
    pub fn normalize_sel(&mut self) {
        let Some(base) = self.sel_gain_base() else { return };
        let (a, b) = self.sel_whole();
        let fs = &base.sample;
        let peak = stats::compute(&fs.channels, a, b.min(fs.frames()), 1.0).peak;
        if peak > 0.0 {
            let db = (self.prefs.normalize_db - stats::to_db(peak)).clamp(-60.0, 60.0);
            self.set_sel_gain(db);
        }
    }

    /// New whole-file audio of the same length: the selection, crop and
    /// markers stay where they are.
    fn replace_audio(&mut self, new_full: Loaded) {
        self.full = Some(new_full);
        self.next_rev += 1;
        self.rev = self.next_rev;
        self.refresh_file_stats();
        self.stats_key = None;
        match self.crop {
            Some((ca, cb)) => {
                let (sel, playhead, view) = (self.sel, self.playhead, (self.view.start, self.view.fpp));
                self.apply_crop(ca, cb);
                self.sel = sel;
                self.playhead = playhead;
                (self.view.start, self.view.fpp) = view;
                self.view.fit_pending = false;
            }
            None => self.current = self.full.clone(),
        }
    }
}
