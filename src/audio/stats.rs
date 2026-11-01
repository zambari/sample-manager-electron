//! Level statistics for a region. Computed on the raw samples; gain is applied
//! afterwards by the caller, except for the clip count which depends on it.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    /// Absolute peak, linear, before gain.
    pub peak: f32,
    pub rms: f32,
    /// Mean sample value; non-zero means a DC offset.
    pub dc: f32,
    /// Samples that would hit or exceed full scale at the current gain.
    pub clipped: usize,
    pub frames: usize,
}

pub fn compute(channels: &[Vec<f32>], start: usize, end: usize, gain: f32) -> Stats {
    let mut peak = 0f32;
    let mut sum = 0f64;
    let mut sum_sq = 0f64;
    let mut n = 0usize;
    let mut clipped = 0usize;
    // A sample clips after gain when |s * gain| >= 1.
    let clip_at = if gain > 0.0 { 1.0 / gain } else { f32::INFINITY };
    for ch in channels {
        let end = end.min(ch.len());
        if start >= end {
            continue;
        }
        for &s in &ch[start..end] {
            let a = s.abs();
            peak = peak.max(a);
            sum += s as f64;
            sum_sq += (s as f64) * (s as f64);
            if a >= clip_at {
                clipped += 1;
            }
        }
        n += end - start;
    }
    if n == 0 {
        return Stats::default();
    }
    Stats {
        peak,
        rms: (sum_sq / n as f64).sqrt() as f32,
        dc: (sum / n as f64) as f32,
        clipped,
        frames: end.saturating_sub(start),
    }
}

pub fn to_db(lin: f32) -> f32 {
    if lin <= 0.0 { f32::NEG_INFINITY } else { 20.0 * lin.log10() }
}

pub fn from_db(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn fmt_db(db: f32) -> String {
    if db.is_finite() { format!("{db:+.1} dB") } else { "−∞ dB".to_owned() }
}
