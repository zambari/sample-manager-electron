//! Kit slices that stop while still sounding (a trimmed reverb tail, say)
//! click on the sampler. These are found, shown, and on request faded out.
//! Each fade's length and curve can be shaped on the wave before applying.

use super::*;

/// How much of the end is checked for sound, in seconds.
const END_WINDOW_S: f64 = 0.0005;
/// The end must also be at least this loud relative to the stretch before
/// it, so an end that already fades out doesn't count.
const END_RATIO: f32 = 0.1;
/// Default fade length; sounds under 0.5 s get a fifth of their length.
const FADE_S: f64 = 0.1;
/// Shortest fade a drag can make.
const MIN_FADE_S: f64 = 0.01;
/// Sounds shorter than this are left alone.
const MIN_SOUND_S: f64 = 0.1;
/// Default curve exponent, and the range a drag can pick.
const FADE_K: f32 = 3.0;
const FADE_K_RANGE: (f32, f32) = (1.0, 8.0);

/// A fade shaped by dragging on the wave: length in frames and curve.
#[derive(Clone, Copy)]
pub struct FadeShape {
    pub len: usize,
    pub k: f32,
}

/// A kit slice's end and the fade-out that would fix it if it's cut off:
/// `[start, end)`, `end` being where the sound stops, with its curve
/// exponent and how loud the very end is.
#[derive(Clone, Copy)]
pub struct FadeEnd {
    /// Which kit slice (0-based).
    pub slice: usize,
    pub start: usize,
    pub end: usize,
    pub k: f32,
    /// Peak of the last moment before the sound stops, dBFS.
    pub end_db: f32,
    /// The end already dies away on its own, so no threshold flags it.
    pub decays: bool,
}

impl FadeEnd {
    /// Cut off at threshold `db`: still sounding when it stops.
    pub fn abrupt(&self, db: f32) -> bool {
        !self.decays && self.end_db > db
    }
}

/// Gain at fraction `t` (0..=1) through a fade with curve `k`: 1 is a
/// straight line, higher drops away faster, like a natural decay. Always
/// reaches true silence.
pub fn fade_gain(t: f32, k: f32) -> f32 {
    (1.0 - t.clamp(0.0, 1.0)).powf(k)
}

impl Editor {
    /// Reshape slice `slice`'s fade (length in frames, curve). Clamped to
    /// what the slice allows when it's used.
    pub fn shape_fade(&mut self, slice: usize, len: usize, k: f32) {
        let k = k.clamp(FADE_K_RANGE.0, FADE_K_RANGE.1);
        self.fades.insert(slice, FadeShape { len, k });
    }

    /// Every kit slice long enough to fade, with how its sound ends and
    /// the fade that would fix it, in whole-file frames. Doesn't depend on
    /// the threshold, so the UI finds these once and filters.
    pub fn tails(&self) -> Vec<FadeEnd> {
        let Some(full) = self.full.as_ref().filter(|_| self.is_kit) else {
            return Vec::new();
        };
        let s = &full.sample;
        let ch = &s.channels;
        let rate = s.rate.max(1) as f64;
        let frames = |secs: f64| (secs * rate) as usize;
        let win = frames(END_WINDOW_S).max(1);
        let peak = |a: usize, b: usize| {
            ch.iter()
                .flat_map(|c| &c[a..b])
                .fold(0.0f32, |m, &x| m.max(x.abs()))
        };
        let mut bounds = vec![0];
        bounds.extend(&self.markers);
        bounds.push(s.frames());
        bounds
            .windows(2)
            .enumerate()
            .filter_map(|(i, w)| {
                let a = w[0];
                let e = (a..w[1]).rev().find(|&f| ch.iter().any(|c| c[f] != 0.0))? + 1;
                let len = e - a;
                if len < frames(MIN_SOUND_S) {
                    return None;
                }
                let default = frames(FADE_S).min(len / 5);
                let end = peak(e - win, e);
                let before = peak(e - default, e - win);
                let sh = self.fades.get(&i).copied().unwrap_or(FadeShape {
                    len: default,
                    k: FADE_K,
                });
                let fade = sh.len.clamp(frames(MIN_FADE_S).min(len / 2), len * 9 / 10);
                Some(FadeEnd {
                    slice: i,
                    start: e - fade,
                    end: e,
                    k: sh.k,
                    end_db: (20.0 * end.max(1e-6).log10()).max(-120.0),
                    decays: end <= END_RATIO * before,
                })
            })
            .collect()
    }

    /// The cut-off ones among `tails` at the current threshold, in shown
    /// frames, for drawing.
    pub fn abrupt_ends(&self, tails: &[FadeEnd]) -> Vec<FadeEnd> {
        let (off, n) = (self.crop.map_or(0, |c| c.0), self.frames());
        let db = self.prefs.tail_db;
        tails
            .iter()
            .copied()
            .filter(|e| e.abrupt(db) && e.end > off && e.start < off + n)
            .map(|e| FadeEnd {
                start: e.start.saturating_sub(off),
                end: (e.end - off).min(n),
                ..e
            })
            .collect()
    }

    /// Fade out the abrupt slice ends (all, or just slice `only`). One undo
    /// step. Returns how many.
    pub fn fade_ends(&mut self, only: Option<usize>) -> usize {
        let db = self.prefs.tail_db;
        let ends: Vec<FadeEnd> = self
            .tails()
            .into_iter()
            .filter(|e| e.abrupt(db) && only.is_none_or(|i| e.slice == i))
            .collect();
        if ends.is_empty() {
            return 0;
        }
        self.uncrop();
        let Some(full) = &self.full else { return 0 };
        let mut chans = full.sample.channels.clone();
        for e in &ends {
            let len = (e.end - e.start) as f32;
            for c in &mut chans {
                for (i, x) in c[e.start..e.end].iter_mut().enumerate() {
                    *x *= fade_gain((i + 1) as f32 / len, e.k);
                }
            }
            self.fades.remove(&e.slice);
        }
        let (sel, playhead, markers) = (self.sel, self.playhead, self.markers.clone());
        let names = self.slice_names();
        let total = self.frames();
        self.splice(0, total, &chans);
        self.markers = markers;
        self.names = names;
        self.sel = sel;
        self.playhead = playhead;
        ends.len()
    }
}
