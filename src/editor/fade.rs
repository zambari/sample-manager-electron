//! Fade-outs for the tail trim (see [`super::trim`]). Each kit slice whose
//! sound runs past the threshold gets one: right where it drops below the
//! threshold, or, when it's still loud at its very end (cut off), over its
//! last stretch. Each fade's length and curve can be shaped on the wave.

use super::*;

/// How much of the end is checked for sound, in seconds.
pub(super) const END_WINDOW_S: f64 = 0.0005;
/// An end at most this loud relative to the stretch before it already
/// fades out on its own.
pub(super) const END_RATIO: f32 = 0.1;
/// The stretch before the end that a cut-off end is compared against.
pub(super) const FADE_S: f64 = 0.1;
/// Shortest cut-off fade a drag can make.
pub(super) const MIN_FADE_S: f64 = 0.01;
/// Cut-off sounds shorter than this are left alone.
pub(super) const MIN_SOUND_S: f64 = 0.1;
/// The curve range a drag picks: from holding the level till a sharp drop
/// at the end, through linear, to a steep early decay.
const FADE_K_RANGE: (f32, f32) = (1.0 / 8.0, 8.0);

/// A fade shaped by dragging on the wave: length in frames and curve.
#[derive(Clone, Copy)]
pub struct FadeShape {
    pub len: usize,
    pub k: f32,
}

/// A fade-out over `[start, end)` with curve exponent `k`.
#[derive(Clone, Copy)]
pub struct FadeEnd {
    /// Which kit slice (0-based).
    pub slice: usize,
    pub start: usize,
    pub end: usize,
    pub k: f32,
    /// Shaped on its own (⌥-drag) rather than following the shared shape.
    pub own: bool,
    /// Starts where the sound drops below the threshold and runs on over
    /// the quieter rest (dragging moves `end`). Otherwise the slice is cut
    /// off and the fade ends where its sound stops (dragging moves `start`).
    pub tail: bool,
}

/// Gain at fraction `t` (0..=1) through a fade with curve `k`: 1 is a
/// straight line, higher drops away early like a natural decay, lower
/// holds the level and drops away near the end. Always reaches true
/// silence. The two sides mirror: `k` and `1/k` are the same curve flipped.
pub fn fade_gain(t: f32, k: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if k >= 1.0 {
        (1.0 - t).powf(k)
    } else {
        1.0 - t.powf(1.0 / k)
    }
}

/// The curve whose gain halfway through is `g` (the knob's height).
pub fn fade_k_for_mid(g: f32) -> f32 {
    let lo = 0.5f32.powf(FADE_K_RANGE.1);
    let g = g.clamp(lo, 1.0 - lo);
    let k = if g <= 0.5 {
        g.ln() / 0.5f32.ln()
    } else {
        0.5f32.ln() / (1.0 - g).ln()
    };
    k.clamp(FADE_K_RANGE.0, FADE_K_RANGE.1)
}

impl Editor {
    /// Reshape every fade that follows the shared shape (length in frames,
    /// curve). Saved with the prefs.
    pub fn shape_all_fades(&mut self, len: usize, k: f32) {
        let rate = self.full.as_ref().map_or(0, |f| f.sample.rate).max(1) as f64;
        self.prefs.trim_fade_ms = ((len as f64 * 1000.0 / rate) as f32).clamp(1.0, 1000.0);
        self.prefs.trim_fade_k = k.clamp(FADE_K_RANGE.0, FADE_K_RANGE.1);
    }

    /// Back to the default shared shape.
    pub fn reset_all_fades(&mut self) {
        let d = Prefs::default();
        self.prefs.trim_fade_ms = d.trim_fade_ms;
        self.prefs.trim_fade_k = d.trim_fade_k;
    }

    /// Reshape only slice `slice`'s fade (length in frames, curve). Clamped
    /// to what the slice allows when it's used.
    pub fn shape_fade(&mut self, slice: usize, len: usize, k: f32) {
        let k = k.clamp(FADE_K_RANGE.0, FADE_K_RANGE.1);
        self.fades.insert(slice, FadeShape { len, k });
    }

    /// Slice `slice`'s fade back to the shared shape.
    pub fn reset_fade(&mut self, slice: usize) {
        self.fades.remove(&slice);
    }

    pub(super) fn fade_shape(&self, slice: usize) -> Option<FadeShape> {
        self.fades.get(&slice).copied()
    }

    /// `fades` (whole-file frames) in shown frames, for drawing.
    pub fn shown_fades(&self, fades: &[FadeEnd]) -> Vec<FadeEnd> {
        let (off, n) = (self.crop.map_or(0, |c| c.0), self.frames());
        fades
            .iter()
            .filter(|e| e.end > off && e.start < off + n)
            .map(|e| FadeEnd {
                start: e.start.saturating_sub(off),
                end: (e.end - off).min(n),
                ..*e
            })
            .collect()
    }
}

/// Fade `v` (one channel of a slice starting at whole-file frame `a`).
pub(super) fn apply(v: &mut [f32], a: usize, f: &FadeEnd) {
    let (s, e) = (f.start - a, (f.end - a).min(v.len()));
    let len = (f.end - f.start) as f32;
    for (i, x) in v[s..e].iter_mut().enumerate() {
        *x *= fade_gain((i + 1) as f32 / len, f.k);
    }
}
