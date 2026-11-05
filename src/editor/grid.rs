//! Grid kits: the file is split into 4 / 8 / 16 equal slots, each slice at
//! the start of its slot and padded with silence up to the longest one. A
//! sampler that can only cut a file into equal parts (Digitakt 1) then
//! plays every slice from its real start.
//!
//! The grid isn't stored anywhere: a kit whose markers split it evenly into
//! one of [`GRID_SIZES`] parts is read back as a grid kit.

use super::*;

pub const GRID_SIZES: [usize; 3] = [4, 8, 16];

/// One slice, one Vec per channel.
type Slice = Vec<Vec<f32>>;

fn is_empty(s: &Slice) -> bool {
    s.first().is_none_or(Vec::is_empty)
}

impl Editor {
    /// The slices the markers mark out (whole file), trailing silence trimmed,
    /// so a grid slot gives back just its sound.
    fn slices(&self) -> Vec<Slice> {
        let Some(full) = &self.full else { return Vec::new() };
        let ch = &full.sample.channels;
        let mut bounds = vec![0];
        bounds.extend(&self.markers);
        bounds.push(full.sample.frames());
        bounds
            .windows(2)
            .map(|w| {
                let (a, b) = (w[0], w[1]);
                let end = (a..b).rev().find(|&f| ch.iter().any(|c| c[f] != 0.0)).map_or(a, |f| f + 1);
                ch.iter().map(|c| c[a..end].to_vec()).collect()
            })
            .collect()
    }

    /// Rewrite the whole file as `n` equal slots holding `slices` in order.
    /// Empty slots from the end are dropped to make room; with more sounding
    /// slices than slots nothing changes and it returns `None`. Otherwise
    /// returns the slot length in frames.
    fn lay_out(&mut self, mut slices: Vec<Slice>, n: usize) -> Option<usize> {
        let nch = self.full.as_ref()?.sample.channels.len();
        while slices.len() > n {
            let i = slices.iter().rposition(is_empty)?;
            slices.remove(i);
        }
        slices.resize(n, vec![Vec::new(); nch]);
        let len = slices.iter().map(|s| s.first().map_or(0, Vec::len)).max().unwrap_or(0).max(1);
        let chans: Vec<Vec<f32>> = (0..nch)
            .map(|c| {
                let mut v = Vec::with_capacity(len * n);
                for (i, s) in slices.iter().enumerate() {
                    if let Some(x) = s.get(c) {
                        v.extend_from_slice(x);
                    }
                    v.resize((i + 1) * len, 0.0);
                }
                v
            })
            .collect();
        self.uncrop();
        let total = self.frames();
        self.splice(0, total, &chans);
        self.markers = (1..n).map(|i| i * len).collect();
        self.grid = Some(n);
        self.view.fit_pending = true;
        Some(len)
    }

    /// Turn the grid on (re-laying the kit's slices out on `n` slots) or off
    /// (the audio stays as it is).
    pub fn set_grid(&mut self, n: Option<usize>) -> Result<(), String> {
        let Some(n) = n else {
            self.grid = None;
            return Ok(());
        };
        if !self.is_kit || self.full.is_none() {
            return Err("Grid only applies to a loaded slice kit".to_owned());
        }
        let s = self.slices();
        let sounding = s.iter().filter(|s| !is_empty(s)).count();
        self.lay_out(s, n).map(|_| ()).ok_or_else(|| format!("The kit has {sounding} slices — too many for {n} slots"))
    }

    /// Add `clip` in the first slot after the last one that sounds, then
    /// select that slot.
    pub(super) fn append_on_grid(&mut self, clip: &AudioClip, n: usize) -> Result<(), String> {
        let Some(full) = &self.full else { return Err("No kit loaded".to_owned()) };
        let ins = clip.adapt(full.sample.rate, full.sample.channels.len());
        let mut s = self.slices();
        let at = s.iter().rposition(|x| !is_empty(x)).map_or(0, |i| i + 1);
        if at >= n {
            return Err(format!("All {n} slots are full — remove a slice or pick a bigger grid"));
        }
        s.resize(n.max(s.len()), Vec::new());
        s[at] = ins;
        let len = self.lay_out(s, n).ok_or("Slice didn't fit the grid")?;
        self.sel = (at * len, (at + 1) * len);
        self.playhead = at * len;
        Ok(())
    }

    /// After a cut / paste / insert in a grid kit: put everything back on
    /// the grid. When it no longer fits, the grid is turned off instead.
    pub(super) fn settle_grid(&mut self) {
        let Some(n) = self.grid else { return };
        let (sel, playhead) = (self.sel, self.playhead);
        if self.lay_out(self.slices(), n).is_none() {
            self.grid = None;
            self.notice = Some(format!("The kit has more than {n} slices now — grid turned off (pick a bigger one to put it back)"));
            return;
        }
        let total = self.frames();
        self.sel = (sel.0.min(total), sel.1.min(total));
        if self.sel.0 >= self.sel.1 {
            self.sel = (0, total);
        }
        self.playhead = playhead.min(total);
    }

    /// The grid this kit's markers describe, if they split it evenly.
    pub(super) fn infer_grid(&self) -> Option<usize> {
        let total = self.full.as_ref()?.sample.frames();
        let n = self.markers.len() + 1;
        if !self.is_kit || !GRID_SIZES.contains(&n) || total % n != 0 {
            return None;
        }
        let len = total / n;
        self.markers.iter().enumerate().all(|(i, &m)| m == (i + 1) * len).then_some(n)
    }
}
