//! Render the edited region (trim + gain, optionally folded to mono) to a
//! new WAV file. Never touches the source file.

use super::decode::Sample;
use std::path::{Path, PathBuf};

pub struct Render {
    pub start: usize,
    pub end: usize,
    pub gain: f32,
    /// Fold to a single channel (average of all).
    pub mono: bool,
}

/// Where an export of `src` into `dest_dir` lands: `<stem>.wav`. This can be
/// the source itself; the caller asks before replacing anything.
pub fn target_for(src: &Path, dest_dir: &Path) -> PathBuf {
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sample".to_owned());
    dest_dir.join(format!("{stem}.wav"))
}

/// Write the rendered region to `target`, replacing it if it exists (conflicts
/// are settled by the caller). Returns the path written.
pub fn write_wav(sample: &Sample, r: &Render, target: &Path) -> Result<PathBuf, String> {
    let target = target.to_path_buf();

    // Keep the source's integer depth where we know it; anything else
    // (MP3, Vorbis, float WAV) goes out as 32-bit float so nothing is lost.
    let (bits, float) = match sample.bits {
        Some(16) => (16, false),
        Some(24) => (24, false),
        _ => (32, true),
    };
    let mono = r.mono && sample.channels.len() > 1;
    let spec = hound::WavSpec {
        channels: if mono { 1 } else { sample.channels.len() as u16 },
        sample_rate: sample.rate,
        bits_per_sample: bits,
        sample_format: if float { hound::SampleFormat::Float } else { hound::SampleFormat::Int },
    };
    let mut w = hound::WavWriter::create(&target, spec).map_err(|e| e.to_string())?;
    let end = r.end.min(sample.frames());
    let scale = ((1i64 << (bits - 1)) - 1) as f32;
    let mut put = |s: f32| -> Result<(), String> {
        let res = if float { w.write_sample(s) } else { w.write_sample((s.clamp(-1.0, 1.0) * scale).round() as i32) };
        res.map_err(|e| e.to_string())
    };
    for f in r.start..end {
        if mono {
            put(super::stats::mono_at(&sample.channels, f) * r.gain)?;
        } else {
            for ch in &sample.channels {
                put(ch[f] * r.gain)?;
            }
        }
    }
    w.finalize().map_err(|e| e.to_string())?;
    Ok(target)
}
