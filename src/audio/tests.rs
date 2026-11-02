use super::decode::decode;
use super::export::{Render, write_wav};
use super::peaks::ChannelPeaks;
use super::stats;
use std::path::PathBuf;

fn tmp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sm_test_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A stereo 16-bit sine at half scale, left and right inverted.
fn write_sine(path: &std::path::Path, frames: usize) {
    let spec = hound::WavSpec { channels: 2, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for i in 0..frames {
        let s = (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.5;
        w.write_sample((s * 32767.0) as i16).unwrap();
        w.write_sample((-s * 32767.0) as i16).unwrap();
    }
    w.finalize().unwrap();
}

#[test]
fn decode_reports_format_and_levels() {
    let d = tmp_dir("decode");
    let p = d.join("sine.wav");
    write_sine(&p, 48_000);
    let s = decode(&p).unwrap();
    assert_eq!(s.rate, 48_000);
    assert_eq!(s.channels.len(), 2);
    assert_eq!(s.frames(), 48_000);
    assert_eq!(s.bits, Some(16));
    let st = stats::compute(&s.channels, 0, s.frames(), 1.0);
    assert!((stats::to_db(st.peak) - -6.02).abs() < 0.1, "peak {}", st.peak);
    assert!((st.rms - 0.5 / 2f32.sqrt()).abs() < 0.01);
    assert!(st.dc.abs() < 1e-3);
    assert_eq!(st.clipped, 0);
    // +12 dB pushes the 0.5 peak over full scale.
    assert!(stats::compute(&s.channels, 0, s.frames(), stats::from_db(12.0)).clipped > 0);
}

#[test]
fn export_trims_and_applies_gain() {
    let d = tmp_dir("export");
    let src = d.join("sine.wav");
    write_sine(&src, 48_000);
    let s = decode(&src).unwrap();
    let out_dir = d.join("out");
    std::fs::create_dir_all(&out_dir).unwrap();

    let r = Render { start: 1000, end: 13_000, gain: 2.0, mono: None };
    let a = write_wav(&s, &r, &out_dir.join("sine.wav")).unwrap();
    assert_eq!(a.file_name().unwrap(), "sine.wav");

    let e = decode(&a).unwrap();
    assert_eq!(e.frames(), 12_000);
    assert_eq!(e.bits, Some(16));
    let want = s.channels[0][1000 + 77] * 2.0;
    assert!((e.channels[0][77] - want).abs() < 1e-3);
    let st = stats::compute(&e.channels, 0, e.frames(), 1.0);
    assert!(stats::to_db(st.peak) > -0.2, "doubled 0.5 peak should be ~0 dBFS");
}

#[test]
fn peaks_match_brute_force() {
    let data: Vec<f32> = (0..100_003).map(|i| ((i * 7919) % 2001) as f32 / 1000.0 - 1.0).collect();
    let p = ChannelPeaks::build(&data);
    for (f0, f1) in [(0, 10), (5, 300), (1000, 70_000), (99_000, 100_003), (0, 100_003)] {
        let (lo, hi) = p.range(&data, f0, f1);
        let blo = data[f0..f1].iter().copied().fold(f32::INFINITY, f32::min);
        let bhi = data[f0..f1].iter().copied().fold(f32::NEG_INFINITY, f32::max);
        // Block approximation may only widen the range.
        assert!(lo <= blo && hi >= bhi, "{f0}..{f1}");
        assert!(blo - lo < 0.01 && hi - bhi < 0.01, "{f0}..{f1} too loose");
    }
}
