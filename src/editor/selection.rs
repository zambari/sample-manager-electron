//! Moving the in / out points, playhead snapping, mono, gain and normalize.

use super::*;

impl Editor {
    pub fn set_in(&mut self, frame: usize) {
        let f = self.snap(frame);
        self.sel.0 = f.min(self.sel.1.saturating_sub(1));
    }

    pub fn set_out(&mut self, frame: usize) {
        let f = self.snap(frame);
        self.sel.1 = f.max(self.sel.0 + 1).min(self.frames());
    }

    /// Set both ends at once from an unordered pair (rubber-band select).
    pub fn set_region(&mut self, a: usize, b: usize) {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let lo = self.snap(lo);
        let hi = self.snap(hi).min(self.frames());
        self.sel = (lo, hi.max(lo + 1).min(self.frames().max(1)));
    }

    /// Slide region `orig` by `delta` frames, keeping its length and
    /// staying inside the clip. Snaps the in point when snapping is on.
    pub fn move_region(&mut self, orig: (usize, usize), delta: i64) {
        let n = self.frames();
        let len = orig.1.saturating_sub(orig.0).min(n);
        let max_in = (n - len) as i64;
        let a = (orig.0 as i64 + delta).clamp(0, max_in) as usize;
        let mut a2 = self.snap(a);
        if a2 + len > n {
            a2 = a;
        }
        self.sel = (a2, a2 + len);
    }

    /// Where a keyboard step of a marker at `frame` lands: the next zero
    /// crossing when snapping is on, otherwise one screen pixel over.
    pub(super) fn step_marker(&self, frame: usize, dir: i8) -> Option<usize> {
        if self.prefs.snap_zero {
            return self.next_zero(frame, dir);
        }
        let px = self.view.fpp.round().max(1.0) as isize;
        Some((frame as isize + px * dir as isize).clamp(0, self.frames() as isize) as usize)
    }

    /// Step the in point in `dir`, staying left of the out point.
    pub fn step_in(&mut self, dir: i8) {
        if let Some(f) = self.step_marker(self.sel.0, dir) {
            self.sel.0 = f.min(self.sel.1.saturating_sub(1));
        }
    }

    /// Step the out point in `dir`, staying right of the in point.
    pub fn step_out(&mut self, dir: i8) {
        if let Some(f) = self.step_marker(self.sel.1, dir) {
            self.sel.1 = f.max(self.sel.0 + 1).min(self.frames());
        }
    }

    /// Snap both ends of the selection to their nearest zero crossings.
    pub fn snap_selection(&mut self) {
        let (a, b) = (self.snap(self.sel.0), self.snap(self.sel.1));
        if a < b {
            self.sel = (a, b);
        }
    }

    /// Whether the selection can be doubled without running past the end.
    pub fn can_double_selection(&self) -> bool {
        let (a, b) = self.sel;
        b > a && a + 2 * (b - a) <= self.frames() + Self::double_slack(b - a)
    }

    /// How far a doubled loop may overshoot the file end and still count as
    /// fitting: halving rounds odd lengths and snaps to zero crossings, so
    /// repeated ½ then ×2 drifts a little. 1% of the loop, at least 64 frames.
    pub(super) fn double_slack(len: usize) -> usize {
        (len / 50).max(64)
    }

    /// Halve (`factor` 0.5) or double (2.0) the selection length, keeping the
    /// in point where it is. The out point snaps to zero if snapping is on.
    pub fn scale_selection(&mut self, factor: f64) {
        let (a, b) = self.sel;
        let len = ((b - a) as f64 * factor).round().max(1.0) as usize;
        let n = self.frames();
        // Doubling to (nearly) the file end lands exactly on the end.
        let out = if factor > 1.0 && (a + len).abs_diff(n) <= Self::double_slack(b - a) {
            n
        } else {
            self.snap((a + len).min(n))
        };
        if out > a {
            self.sel.1 = out;
        }
    }

    /// Whether the selection can move half its length left (-1) or right (+1)
    /// without running past either end of the file.
    pub fn can_shift_selection(&self, dir: i32) -> bool {
        let (a, b) = self.sel;
        let half = (b - a) / 2;
        half > 0 && if dir < 0 { a >= half } else { b + half <= self.frames() }
    }

    /// Move the selection half its length left (-1) or right (+1), keeping its
    /// length exactly (no zero snapping, so loops stay the same size).
    pub fn shift_selection(&mut self, dir: i32) {
        if !self.can_shift_selection(dir) {
            return;
        }
        let (a, b) = self.sel;
        let half = (b - a) / 2;
        self.sel = if dir < 0 { (a - half, b - half) } else { (a + half, b + half) };
    }

    /// Off, or fold this way (listening and export alike).
    pub fn set_mono(&mut self, mono: Option<Fold>) {
        self.prefs.mono = mono;
        if let Some(f) = mono {
            self.prefs.fold = f;
        }
    }

    /// M: mono off, or back on with the last fold picked.
    pub fn toggle_mono(&mut self) {
        let m = if self.prefs.mono.is_some() { None } else { Some(self.prefs.fold) };
        self.set_mono(m);
    }

    pub fn select_all(&mut self) {
        self.sel = (0, self.frames());
    }

    pub fn reset(&mut self) {
        self.select_all();
        self.gain_db = 0.0;
    }

    pub fn normalize(&mut self) {
        let peak = self.region_stats().peak;
        if peak > 0.0 {
            self.gain_db = (self.prefs.normalize_db - stats::to_db(peak)).clamp(-60.0, 60.0);
        }
    }

    /// Nearest zero crossing (summed channels) within ~3 ms, if snapping is on.
    pub(super) fn snap(&self, frame: usize) -> usize {
        let Some(s) = self.sample() else { return frame };
        let n = s.frames();
        let frame = frame.min(n);
        if !self.prefs.snap_zero || n < 2 {
            return frame;
        }
        let mono = |i: usize| -> f32 { s.channels.iter().map(|c| c[i]).sum() };
        let window = (s.rate as usize * 3 / 1000).max(8);
        for d in 0..window {
            for cand in [frame.checked_sub(d), Some(frame + d)].into_iter().flatten() {
                if cand >= 1 && cand < n && (mono(cand - 1) <= 0.0) != (mono(cand) <= 0.0) {
                    return cand;
                }
            }
        }
        frame
    }

    /// First zero crossing strictly past `frame` in `dir`. A crossing at `f`
    /// sits between frames `f - 1` and `f`, the same convention as a cut point.
    pub(super) fn next_zero(&self, frame: usize, dir: i8) -> Option<usize> {
        let s = self.sample()?;
        let n = s.frames();
        let mono = |i: usize| -> f32 { s.channels.iter().map(|c| c[i]).sum() };
        let is_cross = |f: usize| f >= 1 && f < n && (mono(f - 1) <= 0.0) != (mono(f) <= 0.0);
        if dir > 0 {
            (frame + 1..n).find(|&f| is_cross(f))
        } else {
            (1..frame.min(n)).rev().find(|&f| is_cross(f))
        }
    }
}
