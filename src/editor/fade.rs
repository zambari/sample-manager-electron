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
/// Default cut-off fade; sounds under 0.5 s get a fifth of their length.
pub(super) const FADE_S: f64 = 0.1;
/// Shortest cut-off fade a drag can make.
pub(super) const MIN_FADE_S: f64 = 0.01;
/// Cut-off sounds shorter than this are left alone.
pub(super) const MIN_SOUND_S: f64 = 0.1;
/// Default curves: cut-off ends drop away like a natural decay, tails
/// (already below the threshold) more gently. And the range a drag picks.
pub(super) const CUT_K: f32 = 3.0;
pub(super) const TAIL_K: f32 = 2.0;
const FADE_K_RANGE: (f32, f32) = (1.0, 8.0);

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
    /// Starts where the sound drops below the threshold and runs on over
    /// the quieter rest (dragging moves `end`). Otherwise the slice is cut
    /// off and the fade ends where its sound stops (dragging moves `start`).
    pub tail: bool,
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

    /// Back to the default fade for slice `slice`.
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
