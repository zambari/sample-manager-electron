//! Trim kit tails: each slice keeps its sound up to the last moment it's
//! louder than the threshold, then fades (see [`super::fade`]), and the
//! rest goes. The kit closes up, or a grid kit gets shorter slots, so it
//! takes less sample memory.

use super::fade::*;
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
    /// Peak of the last moment before the sound stops, dBFS.
    end_db: f32,
    /// That end already dies away on its own.
    decays: bool,
}

/// How a slice's sound ends, for the threshold strip.
#[derive(Clone, Copy)]
pub struct SliceEnd {
    pub slice: usize,
    pub end_db: f32,
    pub decays: bool,
}

impl SliceEnd {
    /// Still louder than `db` when it stops: gets a fade over its end.
    pub fn cut_off(&self, db: f32) -> bool {
        !self.decays && self.end_db > db
    }
}

/// One slice as the trim would leave it.
#[derive(Clone, Copy)]
pub struct TrimSlice {
    /// Slice bounds now, whole-file frames.
    pub a: usize,
    pub b: usize,
    /// Frames kept from `a`.
    pub keep: usize,
    pub fade: Option<FadeEnd>,
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

    /// The fades it applies, whole-file frames.
    pub fn fades(&self) -> Vec<FadeEnd> {
        self.slices.iter().filter_map(|s| s.fade).collect()
    }
}

impl Editor {
    fn decay_map(&self) -> Option<Arc<DecayMap>> {
        let full = self.full.as_ref().filter(|_| self.is_kit)?;
        let key = (self.rev, self.markers.clone());
        if let Some(m) = self.decay.borrow().as_ref().filter(|m| m.key == key) {
            return Some(m.clone());
        }
        let ch = &full.sample.channels;
        let rate = full.sample.rate.max(1) as f64;
        let win = ((END_WINDOW_S * rate) as usize).max(1);
        let thr: Vec<f32> = (0..BINS)
            .map(|i| 10f32.powf((DB_FLOOR + i as f32 * DB_STEP) / 20.0))
            .collect();
        let peak = |a: usize, b: usize| {
            ch.iter()
                .flat_map(|c| &c[a..b])
                .fold(0.0f32, |m, &x| m.max(x.abs()))
        };
        let mut bounds = vec![0];
        bounds.extend(&self.markers);
        bounds.push(full.sample.frames());
        let slices = bounds
            .windows(2)
            .map(|w| {
                let (a, b) = (w[0], w[1]);
                let mut cut = vec![a; BINS];
                let mut end = a;
                let (mut idx, mut pk) = (0, 0.0f32);
                // From the end back: the running peak only grows, so each
                // step is passed once, at the last frame louder than it.
                for f in (a..b).rev() {
                    let v = ch.iter().fold(0.0f32, |m, c| m.max(c[f].abs()));
                    if v > 0.0 && end == a {
                        end = f + 1;
                    }
                    if v > pk {
                        pk = v;
                        while idx < BINS && pk > thr[idx] {
                            cut[idx] = f + 1;
                            idx += 1;
                        }
                        if idx == BINS {
                            break;
                        }
                    }
                }
                let len = end - a;
                let last = peak(end - win.min(len), end);
                let before_len = ((FADE_S * rate) as usize).min(len / 5).max(win);
                let before = peak(end - before_len.min(len), end - win.min(len));
                Decay {
                    a,
                    b,
                    end,
                    cut,
                    end_db: (20.0 * last.max(1e-6).log10()).max(-120.0),
                    decays: last <= END_RATIO * before,
                }
            })
            .collect();
        let m = Arc::new(DecayMap { key, slices });
        *self.decay.borrow_mut() = Some(m.clone());
        Some(m)
    }

    /// How each slice's sound ends, for the threshold strip.
    pub fn slice_ends(&self) -> Vec<SliceEnd> {
        self.decay_map().map_or_else(Vec::new, |m| {
            m.slices
                .iter()
                .enumerate()
                .filter(|(_, d)| d.end > d.a)
                .map(|(slice, d)| SliceEnd { slice, end_db: d.end_db, decays: d.decays })
                .collect()
        })
    }

    /// Grid kit trimmed by slot length (rather than by level).
    pub fn trims_by_length(&self) -> bool {
        self.is_kit && self.grid.is_some() && !self.prefs.grid_trim_by_level
    }

    /// Grid kit: the slot length now and each slot's sound length (up to
    /// its last non-zero frame), in frames.
    pub fn slot_sounds(&self) -> Option<(usize, Vec<usize>)> {
        let n = self.grid?;
        let map = self.decay_map()?;
        let len = self.full.as_ref()?.sample.frames() / n;
        Some((len, map.slices.iter().map(|d| d.end - d.a).collect()))
    }

    /// The slot length a length trim cuts to, in frames.
    pub fn slot_cut_len(&self) -> Option<usize> {
        let (len, _) = self.slot_sounds()?;
        Some(self.slot_cut.map_or(len, |c| c.clamp(1, len)))
    }

    /// The plan the Trim button applies: by slot length on a grid (unless
    /// level is picked), else by level.
    pub fn active_trim_plan(&self) -> Option<TrimPlan> {
        if self.trims_by_length() {
            self.length_plan(self.slot_cut_len()?)
        } else {
            self.trim_plan(self.prefs.tail_db)
        }
    }

    /// Grid kit: every slot cut to `cut` frames. A sound that runs past
    /// it fades out over its last stretch before the cut.
    pub fn length_plan(&self, cut: usize) -> Option<TrimPlan> {
        let n = self.grid?;
        let map = self.decay_map()?;
        let full = self.full.as_ref()?;
        let total = full.sample.frames();
        let rate = full.sample.rate.max(1) as f64;
        let cut = cut.max(1);
        let def = ((self.prefs.trim_fade_ms.max(0.0) as f64 / 1000.0 * rate) as usize).max(1);
        let slices: Vec<TrimSlice> = map
            .slices
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let len = d.end - d.a;
                let fade = (len > cut).then(|| {
                    let shape = self.fade_shape(i);
                    let l = shape.map_or(def, |sh| sh.len).clamp(1, (cut * 9 / 10).max(1));
                    FadeEnd {
                        slice: i,
                        start: d.a + cut - l,
                        end: d.a + cut,
                        k: shape.map_or(CUT_K, |sh| sh.k),
                        tail: false,
                    }
                });
                TrimSlice { a: d.a, b: d.b, keep: len.min(cut), fade }
            })
            .collect();
        let new_total = slices.iter().map(|s| s.keep).max().unwrap_or(0).max(1) * n;
        Some(TrimPlan { slices, total, new_total })
    }

    /// What trimming at `db` would leave, with the fade each slice gets.
    pub fn trim_plan(&self, db: f32) -> Option<TrimPlan> {
        let map = self.decay_map()?;
        let full = self.full.as_ref()?;
        let total = full.sample.frames();
        let rate = full.sample.rate.max(1) as f64;
        let frames = |secs: f64| (secs * rate) as usize;
        let win = frames(END_WINDOW_S).max(1);
        let tail_len = frames(self.prefs.trim_fade_ms.max(0.0) as f64 / 1000.0).max(1);
        // Round down: keep a hair more rather than less.
        let bin = (((db - DB_FLOOR) / DB_STEP).floor().max(0.0) as usize).min(BINS - 1);
        let slices: Vec<TrimSlice> = map
            .slices
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let (a, e, s) = (d.a, d.end, d.cut[bin]);
                let len = e - a;
                let shape = self.fade_shape(i);
                let fade = if s == a {
                    // Nothing louder than the threshold: a quiet slice,
                    // not a tail. Its sound stays.
                    None
                } else if s + win >= e {
                    // Loud right up to where it stops: fade over its end.
                    (!d.decays && len >= frames(MIN_SOUND_S)).then(|| {
                        let def = frames(FADE_S).min(len / 5);
                        let l = shape.map_or(def, |sh| sh.len);
                        let l = l.clamp(frames(MIN_FADE_S).min(len / 2), len * 9 / 10);
                        FadeEnd {
                            slice: i,
                            start: e - l,
                            end: e,
                            k: shape.map_or(CUT_K, |sh| sh.k),
                            tail: false,
                        }
                    })
                } else {
                    // Decays below the threshold before it stops: fade the
                    // quiet rest from there, then cut.
                    let l = shape.map_or(tail_len, |sh| sh.len).clamp(1, e - s);
                    Some(FadeEnd {
                        slice: i,
                        start: s,
                        end: s + l,
                        k: shape.map_or(TAIL_K, |sh| sh.k),
                        tail: true,
                    })
                };
                let keep = fade.map_or(len, |f| f.end - a);
                // A free kit can't hold an empty slice.
                let keep = if self.grid.is_none() { keep.max(1) } else { keep };
                TrimSlice { a, b: d.b, keep, fade }
            })
            .collect();
        let new_total = match self.grid {
            Some(n) => slices.iter().map(|s| s.keep).max().unwrap_or(0).max(1) * n,
            None => slices.iter().map(|s| s.keep).sum(),
        };
        Some(TrimPlan { slices, total, new_total })
    }

    /// Fade and trim every slice at the current threshold. One undo step.
    /// Returns the kit length before and after, in frames, and how many
    /// fades went in.
    pub fn trim_tails(&mut self) -> Option<(usize, usize, usize)> {
        let plan = self.active_trim_plan()?;
        let nfades = plan.fades().len();
        if plan.saved() == 0 && nfades == 0 {
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
                        if let Some(f) = &s.fade {
                            apply(&mut v, s.a, f);
                        }
                        v
                    })
                    .collect()
            })
            .collect();
        let names = self.slice_names();
        match self.grid {
            Some(n) => {
                self.lay_out(slices, names, n)?;
            }
            None => {
                let nch = ch.len();
                let chans: Vec<Vec<f32>> = (0..nch)
                    .map(|c| slices.iter().flat_map(|s| s[c].iter().copied()).collect())
                    .collect();
                let total = self.frames();
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
        self.fades.clear();
        let total = self.frames();
        self.sel = (0, total);
        self.playhead = self.playhead.min(total);
        Some((plan.total, total, nfades))
    }
}
