//! Soft limiter for gain pushed past unity. Below [`KNEE_DB`] the audio is
//! untouched; from there up to full scale the top is bent over smoothly, so
//! transients give a little instead of being chopped flat. Input more than
//! [`RANGE_DB`] over full scale is more than it can absorb and hard-clips.
//!
//! It's a static curve (per sample, no attack or release), so playback,
//! export and the waveform all agree on exactly what comes out. It only
//! engages while the gain is above 0 dB, fading in over the first
//! [`RAMP_DB`], so a file played as-is is never altered.

use super::stats::{from_db, to_db};

/// Output level where the bend starts.
pub const KNEE_DB: f32 = -0.5;
/// How far over full scale (after gain) input can go before it hard-clips.
pub const RANGE_DB: f32 = 6.0;
/// Gain over which the limiter fades in, so 0 dB → +0.1 dB doesn't jump.
const RAMP_DB: f32 = 1.0;

/// What a sample after gain does at the limiter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Clean,
    /// Bent over by the limiter, but still under the ceiling.
    Limited,
    /// Through the ceiling: hard-clipped.
    Clipped,
}

#[derive(Clone, Copy, Debug)]
pub struct Limiter {
    gain: f32,
    knee: f32,
    top: f32,
    /// Curve exponent; chosen so the slope is 1 at the knee and 0 at `top`.
    r: f32,
}

impl Limiter {
    pub fn new(gain: f32) -> Self {
        let amt = (to_db(gain) / RAMP_DB).clamp(0.0, 1.0);
        let amt = if amt.is_nan() { 0.0 } else { amt };
        let knee = 1.0 - (1.0 - from_db(KNEE_DB)) * amt;
        let top = from_db(RANGE_DB);
        let r = if 1.0 - knee > 1e-6 {
            (top - knee) / (1.0 - knee)
        } else {
            f32::INFINITY
        };
        Self { gain, knee, top, r }
    }

    /// Whether gain is pushing up, so the curve is in play.
    pub fn active(&self) -> bool {
        self.r.is_finite()
    }

    /// Gain a raw sample, then limit it.
    #[inline]
    pub fn apply(&self, s: f32) -> f32 {
        self.shape(s * self.gain)
    }

    /// Limit an already-gained sample. Inactive: passed through unchanged
    /// (callers that need it clamp, as before).
    #[inline]
    pub fn shape(&self, x: f32) -> f32 {
        let a = x.abs();
        if a <= self.knee || !self.active() {
            return x;
        }
        let y = if a >= self.top {
            1.0
        } else {
            let u = (a - self.knee) / (self.top - self.knee);
            self.knee + (1.0 - self.knee) * (1.0 - (1.0 - u).powf(self.r))
        };
        y.copysign(x)
    }

    /// Where an already-gained sample (any sign) lands.
    #[inline]
    pub fn level(&self, x: f32) -> Level {
        let a = x.abs();
        if !self.active() {
            // Gain isn't pushing: a file that already peaks at full scale is
            // fine (16-bit's lowest value decodes to exactly −1.0). Only real
            // overs, as a float file can hold, clip.
            if a > 1.0 { Level::Clipped } else { Level::Clean }
        } else if a >= self.top {
            Level::Clipped
        } else if a > self.knee {
            Level::Limited
        } else {
            Level::Clean
        }
    }

    /// For audio this limiter already wrote (a kit's baked gain): where an
    /// output sample sits.
    #[inline]
    pub fn level_out(&self, y: f32) -> Level {
        let a = y.abs();
        if (self.active() && a >= 0.9999) || a > 1.0 {
            Level::Clipped
        } else if self.active() && a > self.knee {
            Level::Limited
        } else {
            Level::Clean
        }
    }
}

/// Gain (dB) that brings a raw `peak` out of the limiter at `target_db`.
/// Below the knee that's plain arithmetic; above it the gain goes a little
/// further, since the limiter takes some back.
pub fn gain_db_for_peak(peak: f32, target_db: f32) -> f32 {
    let lin = target_db - to_db(peak);
    let target = from_db(target_db).min(0.9999);
    let out = |db: f32| Limiter::new(from_db(db)).apply(peak);
    if out(lin) >= target - 1e-6 {
        return lin.clamp(-60.0, 60.0);
    }
    let (mut lo, mut hi) = (lin, lin + RANGE_DB + 1.0);
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if out(mid) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    hi.clamp(-60.0, 60.0)
}
