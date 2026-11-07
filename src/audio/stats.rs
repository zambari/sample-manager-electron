//! Level statistics for a region. Computed on the raw samples; gain is applied
//! afterwards by the caller, except for the clip count which depends on it.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    /// Absolute peak, linear, before gain.
    pub peak: f32,
    pub rms: f32,
    /// Mean sample value; non-zero means a DC offset.
    pub dc: f32,
    /// Samples that would go through the ceiling (hard-clip) at the current gain.
    pub clipped: usize,
    /// Samples the soft limiter bends at the current gain, short of clipping.
    pub limited: usize,
    pub frames: usize,
}

pub fn compute(channels: &[Vec<f32>], start: usize, end: usize, gain: f32) -> Stats {
    let mut peak = 0f32;
    let mut sum = 0f64;
    let mut sum_sq = 0f64;
    let mut n = 0usize;
    let mut clipped = 0usize;
    let mut limited = 0usize;
    let lim = super::limit::Limiter::new(gain);
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
            match lim.level(a * gain) {
                super::limit::Level::Clipped => clipped += 1,
                super::limit::Level::Limited => limited += 1,
                super::limit::Level::Clean => {}
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
        limited,
        frames: end.saturating_sub(start),
    }
}

pub fn to_db(lin: f32) -> f32 {
    if lin <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * lin.log10()
    }
}

pub fn from_db(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn fmt_db(db: f32) -> String {
    if db.is_finite() {
        format!("{db:+.1} dB")
    } else {
        "−∞ dB".to_owned()
    }
}

/// How the first two channels relate, and what folding to mono costs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Phase {
    /// L/R correlation, −1 (out of phase) … +1 (mono). `None` on silence.
    pub corr: Option<f32>,
    /// Mono sum RMS relative to the stereo RMS, in dB. 0 for identical
    /// channels, about −3 for unrelated ones, very negative when they cancel.
    pub mono_loss_db: f32,
}

/// Mono fold used everywhere: plain average of all channels.
pub fn mono_at(channels: &[Vec<f32>], i: usize) -> f32 {
    channels.iter().map(|c| c[i]).sum::<f32>() / channels.len().max(1) as f32
}

/// How multichannel audio is folded down to one channel.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum Fold {
    /// Average of all channels.
    #[default]
    Mix,
    /// Keep only the left channel.
    Left,
    /// Keep only the right channel.
    Right,
}

impl Fold {
    pub const ALL: [Fold; 3] = [Fold::Mix, Fold::Left, Fold::Right];

    pub fn short(self) -> &'static str {
        match self {
            Fold::Mix => "Mix",
            Fold::Left => "L",
            Fold::Right => "R",
        }
    }

    pub fn at(self, channels: &[Vec<f32>], i: usize) -> f32 {
        match self {
            Fold::Mix => mono_at(channels, i),
            Fold::Left => channels[0][i],
            Fold::Right => channels[1.min(channels.len() - 1)][i],
        }
    }
}

pub fn fold_region(channels: &[Vec<f32>], start: usize, end: usize, fold: Fold) -> Vec<f32> {
    let end = end.min(channels.first().map_or(0, Vec::len));
    (start.min(end)..end)
        .map(|i| fold.at(channels, i))
        .collect()
}

pub fn phase(channels: &[Vec<f32>], start: usize, end: usize) -> Phase {
    if channels.len() < 2 {
        return Phase {
            corr: Some(1.0),
            mono_loss_db: 0.0,
        };
    }
    let (l, r) = (&channels[0], &channels[1]);
    let end = end.min(l.len()).min(r.len());
    let (mut lr, mut ll, mut rr, mut mm) = (0f64, 0f64, 0f64, 0f64);
    for i in start.min(end)..end {
        let (a, b) = (l[i] as f64, r[i] as f64);
        lr += a * b;
        ll += a * a;
        rr += b * b;
        let m = mono_at(channels, i) as f64;
        mm += m * m;
    }
    let denom = (ll * rr).sqrt();
    let corr = (denom > 1e-12).then(|| (lr / denom).clamp(-1.0, 1.0) as f32);
    // Stereo power per channel (average of L and R) vs the mono sum's power.
    let st = (ll + rr) / 2.0;
    let mono_loss_db = if st > 1e-12 {
        (10.0 * (mm.max(1e-20) / st).log10()) as f32
    } else {
        0.0
    };
    Phase { corr, mono_loss_db }
}
