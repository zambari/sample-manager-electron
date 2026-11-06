//! Trim kit tails: whatever follows the last moment a slice is louder than
//! the threshold goes (after a short fade), and the kit closes up, so it
//! takes less sample memory. A grid kit gets shorter slots instead.

use super::*;

/// Threshold steps the decay map is kept at, and its range.
const DB_STEP: f32 = 0.5;
const DB_FLOOR: f32 = -96.0;
const BINS: usize = 193; // DB_FLOOR ..= 0 dB

/// Per slice, where its sound last rises above each threshold step. Found
/// once per audio / marker change so a threshold drag is just lookups.
pub(super) struct DecayMap {
    key: (u64, Vec<usize>),
    slices: Vec<Decay>,
}

struct Decay {
    /// Slice bounds, whole-file frames.
    a: usize,
    b: usize,
    /// One past the last non-zero frame.
    end: usize,
    /// Per threshold step: one past the last frame louder than it (`a`
    /// when nothing is).
    cut: Vec<usize>,
}

/// One slice as the trim would leave it.
#[derive(Clone, Copy)]
pub struct TrimSlice {
    /// Slice bounds now, whole-file frames.
    pub a: usize,
    pub b: usize,
    /// Frames kept from `a`.
    pub keep: usize,
    /// Where the fade-out starts (the cut point), whole-file frames, if
    /// any sound goes.
    pub fade_at: Option<usize>,
}

pub struct TrimPlan {
    pub slices: Vec<TrimSlice>,
    /// Kit length now and after the trim, frames.
    pub total: usize,
    pub new_total: usize,
}

impl TrimPlan {
    pub fn saved(&self) -> usize {
        self.total.saturating_sub(self.new_total)
    }
}

impl Editor {
    fn decay_map(&self) -> Option<std::sync::Arc<DecayMap>> {
        let full = self.full.as_ref().filter(|_| self.is_kit)?;
        let key = (self.rev, self.markers.clone());
        if let Some(m) = self.decay.borrow().as_ref().filter(|m| m.key == key) {
            return Some(m.clone());
        }
        let ch = &full.sample.channels;
        let thr: Vec<f32> = (0..BINS)
            .map(|i| 10f32.powf((DB_FLOOR + i as f32 * DB_STEP) / 20.0))
            .collect();
        let mut bounds = vec![0];
        bounds.extend(&self.markers);
        bounds.push(full.sample.frames());
        let slices = bounds
            .windows(2)
            .map(|w| {
                let (a, b) = (w[0], w[1]);
                let mut cut = vec![a; BINS];
                let mut end = a;
                let (mut idx, mut peak) = (0, 0.0f32);
                // From the end back: the running peak only grows, so each
                // step is passed once, at the last frame louder than it.
                for f in (a..b).rev() {
                    let v = ch.iter().fold(0.0f32, |m, c| m.max(c[f].abs()));
                    if v > 0.0 && end == a {
                        end = f + 1;
                    }
                    if v > peak {
                        peak = v;
                        while idx < BINS && peak > thr[idx] {
                            cut[idx] = f + 1;
                            idx += 1;
                        }
                        if idx == BINS {
                            break;
                        }
                    }
                }
                Decay { a, b, end, cut }
            })
            .collect();
        let m = std::sync::Arc::new(DecayMap { key, slices });
        *self.decay.borrow_mut() = Some(m.clone());
        Some(m)
    }

    /// What trimming at `db` (with `fade_ms` of fade after each cut) would
    /// leave.
    pub fn trim_plan(&self, db: f32) -> Option<TrimPlan> {
        let map = self.decay_map()?;
        let total = self.full.as_ref()?.sample.frames();
        let rate = self.full.as_ref()?.sample.rate.max(1) as f32;
        let fade = (self.prefs.trim_fade_ms.max(0.0) / 1000.0 * rate) as usize;
        // Round down: keep a hair more rather than less.
        let bin = (((db - DB_FLOOR) / DB_STEP).floor().max(0.0) as usize).min(BINS - 1);
        let slices: Vec<TrimSlice> = map
            .slices
            .iter()
            .map(|d| {
                let cut = d.cut[bin];
                let sound = d.end - d.a;
                // Nothing louder than the threshold: a quiet slice, not a
                // tail. Leave its sound alone, drop only the silence.
                let (keep, fade_at) = if cut == d.a || cut + fade >= d.end {
                    (sound, None)
                } else {
                    (cut + fade - d.a, Some(cut))
                };
                // A free kit can't hold an empty slice.
                let keep = if self.grid.is_none() { keep.max(1) } else { keep };
                TrimSlice { a: d.a, b: d.b, keep, fade_at }
            })
            .collect();
        let new_total = match self.grid {
            Some(n) => slices.iter().map(|s| s.keep).max().unwrap_or(0).max(1) * n,
            None => slices.iter().map(|s| s.keep).sum(),
        };
        Some(TrimPlan { slices, total, new_total })
    }

    /// Trim every slice's tail at the current threshold. One undo step.
    /// Returns the kit length before and after, in frames.
    pub fn trim_tails(&mut self) -> Option<(usize, usize)> {
        let plan = self.trim_plan(self.prefs.tail_db)?;
        if plan.saved() == 0 {
            return None;
        }
        self.uncrop();
        let full = self.full.as_ref()?;
        let ch = &full.sample.channels;
        let slices: Vec<Vec<Vec<f32>>> = plan
            .slices
            .iter()
            .map(|s| {
                ch.iter()
                    .map(|c| {
                        let mut v = c[s.a..s.a + s.keep].to_vec();
                        if let Some(at) = s.fade_at {
                            let from = at - s.a;
                            let len = (v.len() - from) as f32;
                            for (i, x) in v[from..].iter_mut().enumerate() {
                                *x *= fade_gain((i + 1) as f32 / len, 2.0);
                            }
                        }
                        v
                    })
                    .collect()
            })
            .collect();
        match self.grid {
            Some(n) => {
                self.lay_out(slices, self.slice_names(), n)?;
            }
            None => {
                let nch = ch.len();
                let chans: Vec<Vec<f32>> = (0..nch)
                    .map(|c| slices.iter().flat_map(|s| s[c].iter().copied()).collect())
                    .collect();
                let total = self.frames();
                let names = self.slice_names();
                self.splice(0, total, &chans);
                self.names = names;
                let mut at = 0;
                self.markers = slices[..slices.len() - 1]
                    .iter()
                    .map(|s| {
                        at += s[0].len();
                        at
                    })
                    .collect();
                self.view.fit_pending = true;
            }
        }
        let total = self.frames();
        self.sel = (0, total);
        self.playhead = self.playhead.min(total);
        Some((plan.total, total))
    }
}
