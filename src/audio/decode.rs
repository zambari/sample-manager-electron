//! File -> planar f32 in memory. Samples are short, so decoding the whole
//! thing up front keeps playback, drawing and stats trivially random-access.

use std::path::{Path, PathBuf};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

pub struct Sample {
    pub path: PathBuf,
    pub rate: u32,
    /// One `Vec` per channel, all the same length.
    pub channels: Vec<Vec<f32>>,
    /// Source bit depth when the container reports one (PCM, FLAC).
    pub bits: Option<u32>,
    /// Short format label for the stats panel, e.g. "WAV".
    pub format: String,
    pub file_len: u64,
}

impl Sample {
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    pub fn seconds(&self, frames: usize) -> f64 {
        frames as f64 / self.rate.max(1) as f64
    }
}

pub fn decode(path: &Path) -> Result<Sample, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let mut hint = Hint::new();
    if !ext.is_empty() {
        hint.with_extension(&ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| format!("unsupported file: {e}"))?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or("no audio track")?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or("no audio codec parameters")?
        .clone();

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|e| format!("no decoder: {e}"))?;

    let mut rate = params.sample_rate.unwrap_or(0);
    let mut channels: Vec<Vec<f32>> = Vec::new();
    let mut tmp: Vec<f32> = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            // A truncated file still gives us everything up to the damage.
            Err(Error::IoError(_)) => break,
            Err(e) => return Err(e.to_string()),
        };
        if packet.track_id != track_id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(b) => b,
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(e.to_string()),
        };
        let n_ch = buf.spec().channels().count().max(1);
        if rate == 0 {
            rate = buf.spec().rate();
        }
        if channels.len() != n_ch {
            channels.resize_with(n_ch, Vec::new);
        }
        tmp.resize(buf.samples_interleaved(), 0.0);
        buf.copy_to_slice_interleaved(&mut tmp);
        for frame in tmp.chunks_exact(n_ch) {
            for (c, s) in frame.iter().enumerate() {
                channels[c].push(*s);
            }
        }
    }

    if channels.is_empty() || channels[0].is_empty() {
        return Err("file contains no audio".to_owned());
    }

    Ok(Sample {
        path: path.to_path_buf(),
        rate: rate.max(1),
        channels,
        bits: params.bits_per_sample,
        format: if ext.is_empty() { "?".to_owned() } else { ext.to_ascii_uppercase() },
        file_len,
    })
}

/// Channel count from the file header alone, without decoding any audio.
pub fn channel_count(path: &Path) -> Option<usize> {
    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension() {
        hint.with_extension(&ext.to_string_lossy().to_ascii_lowercase());
    }
    let format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .ok()?;
    let track = format.default_track(TrackType::Audio)?;
    let params = track.codec_params.as_ref()?.audio()?;
    params.channels.as_ref().map(|c| c.count())
}
