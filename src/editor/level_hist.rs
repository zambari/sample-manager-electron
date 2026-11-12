//! How many frames of the region sit at each level, before gain, so the
//! histogram panel can show how much of the audio a gain pushes into the
//! limiter; plus the region's silent lead-in and tail.
//!
//! Binned straight off the f32 bits (exponent + top 5 mantissa bits), which
//! makes each bin 1/32 octave (~0.19 dB) wide and costs no log per sample.
//!
//! Both are cached by audio + range, so playback repaints never rescan;
//! only moving the in/out points, an edit or a crop does.

use super::*;

/// Mantissa bits kept below the exponent: 5 → 32 bins per octave.
const SUB: u32 = 5;
const SHIFT: u32 = 23 - SUB;
pub const BINS: usize = 1 << (8 + SUB);

/// Audio identity (pointer), range, and (samples only) revision + crop.
pub type Key = (usize, usize, usize, u64, usize);

pub struct LevelHist {
    /// Frames by bin of their loudest channel's |sample|. Bin 0 holds
    /// silence (and denormals).
    pub counts: Vec<u64>,
    pub total: u64,
}

impl LevelHist {
    fn build(ch: &[Vec<f32>], a: usize, b: usize) -> Self {
        let mut counts = vec![0u64; BINS];
        let b = ch.iter().map(|c| c.len()).min().unwrap_or(0).min(b);
        for i in a..b {
            let m = ch.iter().fold(0f32, |m, c| m.max(c[i].abs()));
            let k = (m.to_bits() >> SHIFT) as usize;
            counts[k.min(BINS - 1)] += 1;
        }
        Self {
            counts,
            total: b.saturating_sub(a) as u64,
        }
    }

    /// Linear bounds of bin `k` (pre-gain).
    pub fn bounds(k: usize) -> (f32, f32) {
        let lo = f32::from_bits((k as u32) << SHIFT);
        let hi = f32::from_bits(((k as u32) + 1) << SHIFT);
        (lo, hi)
    }

    /// Linear level at the middle of bin `k` (pre-gain).
    pub fn center(k: usize) -> f32 {
        let (lo, hi) = Self::bounds(k);
        (lo * hi).sqrt()
    }
}

/// Near-silence at the ends of the region, in frames.
#[derive(Clone, Copy, Default)]
pub struct Edges {
    /// Frames under the "reasonably zero" threshold at each end.
    pub lead_quiet: usize,
    pub tail_quiet: usize,
}

fn edges(ch: &[Vec<f32>], a: usize, b: usize, quiet: f32) -> Edges {
    let b = ch.iter().map(|c| c.len()).min().unwrap_or(0).min(b);
    if b <= a {
        return Edges::default();
    }
    let peak = |i: usize| ch.iter().fold(0f32, |m, c| m.max(c[i].abs()));
    Edges {
        lead_quiet: (a..b).position(|i| peak(i) >= quiet).unwrap_or(b - a),
        tail_quiet: (a..b).rev().position(|i| peak(i) >= quiet).unwrap_or(b - a),
    }
}

impl Editor {
    /// What the gain applies to, before the gain: the in/out region and the
    /// gain slider for a sample; the selection and its gain (against the
    /// audio before it) in a kit.
    fn hist_source(&self) -> Option<(Arc<Sample>, usize, usize, f32, Key)> {
        let (src, a, b, db) = if self.is_kit {
            let db = self.sel_gain_db();
            let (a, b) = self.sel_whole();
            (self.sel_gain_base()?.sample, a, b, db)
        } else {
            let c = self.current.as_ref()?;
            (c.sample.clone(), self.sel.0, self.sel.1, self.gain_db)
        };
        // The kit's base stays alive in `sel_gain` while the gain is dialed,
        // so its pointer is enough; a sample's audio changes with each edit
        // or crop, and a freed buffer's address can come back.
        let edit = if self.is_kit { (0, 0) } else { (self.rev, self.crop_offset()) };
        let key = (Arc::as_ptr(&src) as usize, a, b, edit.0, edit.1);
        Some((src, a, b, db, key))
    }

    /// Level histogram of the gain's target before the gain, and the gain
    /// (dB) to shift it by.
    pub fn level_hist(&mut self) -> Option<(Arc<LevelHist>, f32)> {
        let (src, a, b, db, key) = self.hist_source()?;
        if let Some((k, h)) = &self.level_hist {
            if *k == key {
                return Some((h.clone(), db));
            }
        }
        let h = Arc::new(LevelHist::build(&src.channels, a, b));
        self.level_hist = Some((key, h.clone()));
        Some((h, db))
    }

    /// Silence at the region's ends; "quiet" is under `zero_db` after gain.
    pub fn edges(&mut self, zero_db: f32) -> Edges {
        let Some((src, a, b, db, key)) = self.hist_source() else {
            return Edges::default();
        };
        let lim = stats::from_db(zero_db - db);
        let k = (key, lim.to_bits());
        // A few entries, so the stats box's two thresholds don't evict
        // each other every frame.
        if let Some((_, e)) = self.edges.iter().find(|(kk, _)| *kk == k) {
            return *e;
        }
        let e = edges(&src.channels, a, b, lim);
        self.edges.retain(|((kk, _), _)| *kk == key);
        if self.edges.len() >= 4 {
            self.edges.remove(0);
        }
        self.edges.push((k, e));
        e
    }
}
