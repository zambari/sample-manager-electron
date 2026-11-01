//! Render the edited region (trim + gain) to a new WAV file. Never touches the
//! source file.

use super::decode::Sample;
use std::path::{Path, PathBuf};

pub struct Render {
    pub start: usize,
    pub end: usize,
    pub gain: f32,
}

/// Write `dest_dir/<stem>.wav`, with a ` 2`, ` 3`... suffix if that name is
/// taken. Returns the path written.
pub fn write_wav(sample: &Sample, r: &Render, dest_dir: &Path) -> Result<PathBuf, String> {
    let stem = sample
        .path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sample".to_owned());
    let target = crate::fs_model::unique_sibling(&dest_dir.join(format!("{stem}.wav")));

    // Keep the source's integer depth where we know it; anything else
    // (MP3, Vorbis, float WAV) goes out as 32-bit float so nothing is lost.
    let (bits, float) = match sample.bits {
        Some(16) => (16, false),
        Some(24) => (24, false),
        _ => (32, true),
    };
    let spec = hound::WavSpec {
        channels: sample.channels.len() as u16,
        sample_rate: sample.rate,
        bits_per_sample: bits,
        sample_format: if float { hound::SampleFormat::Float } else { hound::SampleFormat::Int },
    };
    let mut w = hound::WavWriter::create(&target, spec).map_err(|e| e.to_string())?;
    let end = r.end.min(sample.frames());
    let scale = ((1i64 << (bits - 1)) - 1) as f32;
    for f in r.start..end {
        for ch in &sample.channels {
            let s = ch[f] * r.gain;
            let res = if float {
                w.write_sample(s)
            } else {
                w.write_sample((s.clamp(-1.0, 1.0) * scale).round() as i32)
            };
            res.map_err(|e| e.to_string())?;
        }
    }
    w.finalize().map_err(|e| e.to_string())?;
    Ok(target)
}
