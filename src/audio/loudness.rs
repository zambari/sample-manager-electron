//! Loudness after ITU-R BS.1770 / EBU R128: K-weighted LUFS (integrated,
//! momentary and short-term max, loudness range), plus a true-peak estimate
//! from 4× oversampling. Plain RMS and sample peak live in [`super::stats`].
//!
//! All channels weigh 1.0 (no surround weighting), so a mono file reads
//! about 3 LU below the same signal duplicated to stereo, as the standard
//! says it should.

/// Readings for a stretch of audio, before any gain.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Loudness {
    /// Gated integrated loudness, LUFS. `None` on silence.
    pub integrated: Option<f32>,
    /// The stretch was shorter than one 400 ms block, so `integrated` and
    /// `momentary_max` come from a single block over all of it.
    pub short: bool,
    /// Loudest 400 ms block, LUFS.
    pub momentary_max: Option<f32>,
    /// Loudest 3 s window, LUFS. `None` when shorter than 3 s.
    pub short_term_max: Option<f32>,
    /// Loudness range (10th–95th percentile of short-term), LU. `None`
    /// without at least two 3 s windows.
    pub lra: Option<f32>,
    /// Inter-sample peak estimate, linear.
    pub true_peak: f32,
}

/// Measure `channels` (all the same length) at `rate`. Returns `None` if
/// `cancelled` turns true part way, so a stale request can stop early.
pub fn measure(channels: &[&[f32]], rate: u32, cancelled: &dyn Fn() -> bool) -> Option<Loudness> {
    let n = channels.iter().map(|c| c.len()).min().unwrap_or(0);
    if n == 0 || rate == 0 {
        return Some(Loudness::default());
    }
    // K-weighted power summed over channels, per 100 ms step.
    let step = (rate as usize / 10).max(1);
    let steps = n.div_ceil(step);
    let mut seg = vec![0f64; steps];
    let mut true_peak = 0f32;
    for ch in channels {
        let ch = &ch[..n];
        let mut k = KWeight::new(rate);
        for (i, chunk) in ch.chunks(step).enumerate() {
            let mut acc = 0f64;
            for &s in chunk {
                let y = k.run(s as f64);
                acc += y * y;
            }
            seg[i] += acc;
            if i % 64 == 0 && cancelled() {
                return None;
            }
        }
        true_peak = true_peak.max(true_peak_of(ch, cancelled)?);
    }

    let lufs = |power: f64| -0.691 + 10.0 * power.max(1e-20).log10();
    // Mean power of a window of `len` steps starting at `i`.
    let window = |i: usize, len: usize| {
        let frames = (len * step).min(n - i * step);
        seg[i..i + len].iter().sum::<f64>() / frames as f64
    };

    let full_blocks = |len: usize| -> Vec<f64> {
        // Only windows made of whole steps (the last step may be short).
        let whole = n / step;
        if whole < len {
            return Vec::new();
        }
        (0..=whole - len).map(|i| window(i, len)).collect()
    };

    let mut short = false;
    let mut blocks = full_blocks(4);
    if blocks.is_empty() {
        // Shorter than one block: measure it as a single one.
        short = true;
        blocks = vec![seg.iter().sum::<f64>() / n as f64];
    }

    let abs_gate = |p: &&f64| lufs(**p) > -70.0;
    let above_abs: Vec<f64> = blocks.iter().filter(abs_gate).copied().collect();
    let integrated = (!above_abs.is_empty()).then(|| {
        let mean = above_abs.iter().sum::<f64>() / above_abs.len() as f64;
        let rel = lufs(mean) - 10.0;
        let gated: Vec<f64> = above_abs.iter().copied().filter(|&p| lufs(p) > rel).collect();
        let m = gated.iter().sum::<f64>() / gated.len().max(1) as f64;
        lufs(m) as f32
    });
    let momentary_max = blocks
        .iter()
        .copied()
        .fold(None, |m: Option<f64>, p| Some(m.map_or(p, |m| m.max(p))))
        .filter(|&p| lufs(p) > -70.0)
        .map(|p| lufs(p) as f32);

    let st = full_blocks(30);
    let short_term_max = st
        .iter()
        .copied()
        .fold(None, |m: Option<f64>, p| Some(m.map_or(p, |m| m.max(p))))
        .filter(|&p| lufs(p) > -70.0)
        .map(|p| lufs(p) as f32);
    let lra = loudness_range(&st, &lufs);

    Some(Loudness {
        integrated,
        short,
        momentary_max,
        short_term_max,
        lra,
        true_peak,
    })
}

/// EBU Tech 3342: short-term powers gated at −70 LUFS and 20 LU below their
/// mean, then the spread between the 10th and 95th percentiles.
fn loudness_range(st: &[f64], lufs: &impl Fn(f64) -> f64) -> Option<f32> {
    let above: Vec<f64> = st.iter().copied().filter(|&p| lufs(p) > -70.0).collect();
    if above.len() < 2 {
        return None;
    }
    let mean = above.iter().sum::<f64>() / above.len() as f64;
    let rel = lufs(mean) - 20.0;
    let mut l: Vec<f64> = above.iter().map(|&p| lufs(p)).filter(|&v| v > rel).collect();
    if l.len() < 2 {
        return None;
    }
    l.sort_by(f64::total_cmp);
    let at = |q: f64| l[((l.len() - 1) as f64 * q).round() as usize];
    Some((at(0.95) - at(0.10)) as f32)
}

/// The two-stage K-weighting filter (high shelf + high pass), with
/// coefficients worked out for any sample rate.
struct KWeight {
    shelf: Biquad,
    hp: Biquad,
}

impl KWeight {
    fn new(rate: u32) -> Self {
        let fs = rate as f64;
        // Stage 1: +4 dB high shelf around 1.7 kHz.
        let (f0, g, q) = (1681.974450955533, 3.999843853973347, 0.7071752369554196);
        let k = (std::f64::consts::PI * f0 / fs).tan();
        let vh = 10f64.powf(g / 20.0);
        let vb = vh.powf(0.4996667741545416);
        let a0 = 1.0 + k / q + k * k;
        let shelf = Biquad::new(
            [(vh + vb * k / q + k * k) / a0, 2.0 * (k * k - vh) / a0, (vh - vb * k / q + k * k) / a0],
            [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        );
        // Stage 2: high pass at 38 Hz.
        let (f0, q) = (38.13547087602444, 0.5003270373238773);
        let k = (std::f64::consts::PI * f0 / fs).tan();
        let a0 = 1.0 + k / q + k * k;
        let hp = Biquad::new(
            [1.0, -2.0, 1.0],
            [2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0],
        );
        Self { shelf, hp }
    }

    fn run(&mut self, x: f64) -> f64 {
        self.hp.run(self.shelf.run(x))
    }
}

struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    fn new(b: [f64; 3], a: [f64; 2]) -> Self {
        Self { b, a, z: [0.0; 2] }
    }

    /// Transposed direct form II.
    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

const OVERSAMPLE: usize = 4;
const TAPS_PER_PHASE: usize = 12;

/// Polyphase interpolation filter: a Blackman-windowed sinc cut at the
/// original Nyquist, each phase scaled to unity gain.
fn tp_filter() -> [[f32; TAPS_PER_PHASE]; OVERSAMPLE] {
    let len = OVERSAMPLE * TAPS_PER_PHASE;
    let mid = (len - 1) as f64 / 2.0;
    let mut h = vec![0f64; len];
    for (i, v) in h.iter_mut().enumerate() {
        let t = (i as f64 - mid) / OVERSAMPLE as f64;
        let sinc = if t.abs() < 1e-9 {
            1.0
        } else {
            (std::f64::consts::PI * t).sin() / (std::f64::consts::PI * t)
        };
        let w = i as f64 / (len - 1) as f64 * std::f64::consts::TAU;
        let blackman = 0.42 - 0.5 * w.cos() + 0.08 * (2.0 * w).cos();
        *v = sinc * blackman;
    }
    let mut out = [[0f32; TAPS_PER_PHASE]; OVERSAMPLE];
    for (p, phase) in out.iter_mut().enumerate() {
        let sum: f64 = (0..TAPS_PER_PHASE).map(|k| h[k * OVERSAMPLE + p]).sum();
        for (k, c) in phase.iter_mut().enumerate() {
            *c = (h[k * OVERSAMPLE + p] / sum) as f32;
        }
    }
    out
}

/// Highest absolute value of `x` upsampled 4×, never below its sample peak.
pub fn true_peak_of(x: &[f32], cancelled: &dyn Fn() -> bool) -> Option<f32> {
    const T: usize = TAPS_PER_PHASE;
    // Reversed so each output is a straight dot product with the history,
    // oldest sample first.
    let mut f = tp_filter();
    for phase in &mut f {
        phase.reverse();
    }
    let mut peak = 0f32;
    // History stored twice over so the last T samples are always one
    // contiguous slice; zeros before the start let the edges ring in.
    let mut buf = [0f32; 2 * T];
    let mut at = 0usize;
    let tail = std::iter::repeat_n(0f32, T / 2);
    for (i, s) in x.iter().copied().chain(tail).enumerate() {
        peak = peak.max(s.abs());
        buf[at] = s;
        buf[at + T] = s;
        at = (at + 1) % T;
        let hist = &buf[at..at + T];
        for phase in &f {
            let acc: f32 = phase.iter().zip(hist).map(|(c, h)| c * h).sum();
            peak = peak.max(acc.abs());
        }
        if i % 65536 == 0 && cancelled() {
            return None;
        }
    }
    Some(peak)
}
