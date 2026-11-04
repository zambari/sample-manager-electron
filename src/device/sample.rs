//! The Digitakt stores samples in its own raw format, not WAV:
//!
//! ```text
//! 64-byte header:
//!   0  u8   type (0)
//!   1  u8   stereo (1) / mono (0)
//!   4  u32  sample data length in bytes
//!   8  u32  sample rate
//!  12  u32  first sample index
//!  16  u32  last sample index
//!  20  u8   loop type (0x7F = no loop)
//! then 16-bit big-endian PCM, interleaved when stereo.
//! ```
//!
//! All header integers are big-endian.

use crate::audio::decode::Sample;
use std::path::Path;

pub const HEADER_LEN: usize = 64;

/// Encode a decoded sample for the device. The source rate goes in the
/// header as-is: the Digitakt resamples on its own. More than two channels
/// keep the first two; `mono` (or a mono-only device) averages them all.
pub fn to_device(s: &Sample, stereo: bool) -> Vec<u8> {
    let frames = s.frames();
    let stereo = stereo && s.channels.len() >= 2;
    let n_ch = if stereo { 2 } else { 1 };
    let data_len = frames * n_ch * 2;

    let mut out = vec![0u8; HEADER_LEN];
    out[1] = u8::from(stereo);
    out[4..8].copy_from_slice(&(data_len as u32).to_be_bytes());
    out[8..12].copy_from_slice(&s.rate.to_be_bytes());
    out[12..16].copy_from_slice(&0u32.to_be_bytes());
    out[16..20].copy_from_slice(&(frames.saturating_sub(1) as u32).to_be_bytes());
    out[20] = 0x7F;
    out.reserve(data_len);

    let q = |v: f32| ((v.clamp(-1.0, 1.0) * 32767.0).round() as i16).to_be_bytes();
    for f in 0..frames {
        if stereo {
            out.extend(q(s.channels[0][f]));
            out.extend(q(s.channels[1][f]));
        } else {
            out.extend(q(crate::audio::stats::mono_at(&s.channels, f)));
        }
    }
    out
}

/// Write a device sample file out as a 16-bit WAV.
pub fn write_wav(raw: &[u8], target: &Path) -> Result<(), String> {
    if raw.len() < HEADER_LEN {
        return Err("not a sample file (too short)".into());
    }
    let be32 = |o: usize| u32::from_be_bytes([raw[o], raw[o + 1], raw[o + 2], raw[o + 3]]);
    let stereo = raw[1] == 1;
    let rate = be32(8).max(1);
    let data = &raw[HEADER_LEN..];
    let data = &data[..(be32(4) as usize).min(data.len())];

    let spec = hound::WavSpec {
        channels: if stereo { 2 } else { 1 },
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(target, spec).map_err(|e| e.to_string())?;
    let frame = if stereo { 4 } else { 2 };
    // Drop a trailing partial frame rather than writing half of one.
    let usable = data.len() / frame * frame;
    for pair in data[..usable].chunks_exact(2) {
        w.write_sample(i16::from_be_bytes([pair[0], pair[1]])).map_err(|e| e.to_string())?;
    }
    w.finalize().map_err(|e| e.to_string())
}
