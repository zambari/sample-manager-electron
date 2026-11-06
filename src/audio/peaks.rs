//! Min/max summaries for drawing. Zoomed all the way out, one pixel can cover
//! tens of thousands of frames; scanning those every frame would stall the
//! UI, so we keep a small pyramid and read from the coarsest level that still
//! resolves one pixel.

const BASE_BLOCK: usize = 64;
const FACTOR: usize = 8;

struct Level {
    block: usize,
    min: Vec<f32>,
    max: Vec<f32>,
}

pub struct ChannelPeaks {
    levels: Vec<Level>,
}

impl ChannelPeaks {
    pub fn build(data: &[f32]) -> Self {
        let mut levels = Vec::new();
        let first = Level {
            block: BASE_BLOCK,
            min: data
                .chunks(BASE_BLOCK)
                .map(|c| c.iter().copied().fold(f32::INFINITY, f32::min))
                .collect(),
            max: data
                .chunks(BASE_BLOCK)
                .map(|c| c.iter().copied().fold(f32::NEG_INFINITY, f32::max))
                .collect(),
        };
        levels.push(first);
        while levels.last().is_some_and(|l| l.min.len() > FACTOR) {
            let prev = levels.last().expect("non-empty");
            let next = Level {
                block: prev.block * FACTOR,
                min: prev
                    .min
                    .chunks(FACTOR)
                    .map(|c| c.iter().copied().fold(f32::INFINITY, f32::min))
                    .collect(),
                max: prev
                    .max
                    .chunks(FACTOR)
                    .map(|c| c.iter().copied().fold(f32::NEG_INFINITY, f32::max))
                    .collect(),
            };
            levels.push(next);
        }
        Self { levels }
    }

    /// Min and max of `data[f0..f1]`, approximated at block granularity when
    /// the span is large. The approximation only ever widens the range,
    /// which is the right way to be wrong for a waveform.
    pub fn range(&self, data: &[f32], f0: usize, f1: usize) -> (f32, f32) {
        let f1 = f1.min(data.len());
        if f0 >= f1 {
            return (0.0, 0.0);
        }
        let span = f1 - f0;
        let level = self.levels.iter().rev().find(|l| l.block * 2 <= span);
        match level {
            None => data[f0..f1]
                .iter()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &s| {
                    (lo.min(s), hi.max(s))
                }),
            Some(l) => {
                let b0 = f0 / l.block;
                let b1 = f1.div_ceil(l.block).min(l.min.len());
                let lo = l.min[b0..b1].iter().copied().fold(f32::INFINITY, f32::min);
                let hi = l.max[b0..b1]
                    .iter()
                    .copied()
                    .fold(f32::NEG_INFINITY, f32::max);
                (lo, hi)
            }
        }
    }
}
