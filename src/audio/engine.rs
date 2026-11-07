//! Output device + one playback voice.
//!
//! The audio callback and the UI share a [`Voice`] behind a mutex. The UI only
//! holds the lock for a few field writes, and the callback uses `try_lock`,
//! so a contended block costs one buffer of silence rather than a glitch
//! that waits on the UI thread.
//!
//! Loop region, gain and loop flag can be changed while playing, which is
//! the whole point: you nudge a trim point and hear the loop seam move.

use super::decode::Sample;
use super::stats::Fold;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Voice {
    sample: Option<Arc<Sample>>,
    /// Fractional read position in source frames.
    pos: f64,
    /// Where a loop jumps back to.
    loop_start: usize,
    /// Playback stops (or loops) here. Exclusive.
    end: usize,
    looping: bool,
    gain: f32,
    /// Fold the source channels into one and send it to every output.
    mono: Option<Fold>,
    playing: bool,
}

struct Shared {
    voice: Mutex<Voice>,
    /// Mirror of `voice.pos` for the UI, readable without the lock.
    pos: AtomicU64,
    playing: AtomicBool,
    error: Mutex<Option<String>>,
    /// Master volume (f32 bits), applied on top of the editor's gain.
    volume: AtomicU32,
    /// Playback speed multiplier (f32 bits) from the pitch fader. Shifts
    /// pitch and tempo together, like a turntable.
    speed: AtomicU32,
    /// Output peak per side since the UI last read it (f32 bits; positive
    /// floats sort like their bit patterns, so `fetch_max` works).
    peak: [AtomicU32; 2],
    /// Last few thousand output frames as (L, R) f32 bit pairs, for the
    /// phase scope. `scope_at` is the next slot to write.
    scope: Vec<AtomicU64>,
    scope_at: AtomicUsize,
    /// The voice is putting out two distinct channels.
    stereo: AtomicBool,
}

/// Frames kept for the phase scope.
pub const SCOPE_LEN: usize = 2048;

#[derive(Clone, PartialEq)]
pub struct DeviceInfo {
    /// Stable id (cpal `DeviceId` display form), what we persist.
    pub id: String,
    pub name: String,
}

pub struct Engine {
    shared: Arc<Shared>,
    stream: Option<cpal::Stream>,
    pub device_name: String,
    pub device_rate: u32,
    pub device_channels: u16,
    pub open_error: Option<String>,
}

pub struct PlayParams {
    pub from: usize,
    pub loop_start: usize,
    pub end: usize,
    pub looping: bool,
    pub gain: f32,
    pub mono: Option<Fold>,
}

impl Engine {
    /// Open `device_id`, or the system default when `None` or not found.
    pub fn open(device_id: Option<&str>) -> Self {
        let shared = Arc::new(Shared {
            voice: Mutex::new(Voice::default()),
            pos: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            error: Mutex::new(None),
            volume: AtomicU32::new(1.0f32.to_bits()),
            speed: AtomicU32::new(1.0f32.to_bits()),
            peak: [AtomicU32::new(0), AtomicU32::new(0)],
            scope: (0..SCOPE_LEN).map(|_| AtomicU64::new(0)).collect(),
            scope_at: AtomicUsize::new(0),
            stereo: AtomicBool::new(false),
        });
        let mut e = Self {
            shared,
            stream: None,
            device_name: String::new(),
            device_rate: 0,
            device_channels: 0,
            open_error: None,
        };
        if let Err(err) = e.open_stream(device_id) {
            e.open_error = Some(err);
        }
        e
    }

    pub fn devices() -> Vec<DeviceInfo> {
        let host = cpal::default_host();
        let Ok(devs) = host.output_devices() else {
            return Vec::new();
        };
        devs.filter_map(|d| {
            let id = d.id().ok()?.to_string();
            let name = d
                .description()
                .map(|x| x.name().to_owned())
                .unwrap_or_else(|_| id.clone());
            Some(DeviceInfo { id, name })
        })
        .collect()
    }

    /// Swap to another output device. Playback stops.
    pub fn switch(&mut self, device_id: Option<&str>) {
        self.stop();
        self.stream = None;
        self.open_error = self.open_stream(device_id).err();
    }

    fn open_stream(&mut self, device_id: Option<&str>) -> Result<(), String> {
        let host = cpal::default_host();
        let device = device_id
            .and_then(|id| id.parse::<cpal::DeviceId>().ok())
            .and_then(|id| host.device_by_id(&id))
            .or_else(|| host.default_output_device())
            .ok_or("no output device")?;
        let config = device.default_output_config().map_err(|e| e.to_string())?;
        self.device_name = device
            .description()
            .map(|d| d.name().to_owned())
            .unwrap_or_else(|_| "output".to_owned());
        self.device_rate = config.sample_rate();
        self.device_channels = config.channels();
        *self.shared.error.lock().expect("poisoned") = None;

        let sc: cpal::StreamConfig = config.config();
        let sh = self.shared.clone();
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => build::<f32>(&device, sc, sh),
            cpal::SampleFormat::F64 => build::<f64>(&device, sc, sh),
            cpal::SampleFormat::I16 => build::<i16>(&device, sc, sh),
            cpal::SampleFormat::I32 => build::<i32>(&device, sc, sh),
            cpal::SampleFormat::U16 => build::<u16>(&device, sc, sh),
            other => return Err(format!("unsupported device sample format {other}")),
        }?;
        stream.play().map_err(|e| e.to_string())?;
        self.stream = Some(stream);
        Ok(())
    }

    pub fn play(&self, sample: Arc<Sample>, p: PlayParams) {
        let mut v = self.shared.voice.lock().expect("poisoned");
        v.sample = Some(sample);
        v.pos = p.from as f64;
        v.loop_start = p.loop_start;
        v.end = p.end;
        v.looping = p.looping;
        v.gain = p.gain;
        v.mono = p.mono;
        v.playing = true;
        self.shared.pos.store(p.from as u64, Ordering::Relaxed);
        self.shared.playing.store(true, Ordering::Relaxed);
    }

    /// Live parameter changes while playing.
    pub fn update(
        &self,
        loop_start: usize,
        end: usize,
        looping: bool,
        gain: f32,
        mono: Option<Fold>,
    ) {
        let mut v = self.shared.voice.lock().expect("poisoned");
        v.loop_start = loop_start;
        v.end = end;
        v.looping = looping;
        v.gain = gain;
        v.mono = mono;
    }

    pub fn seek(&self, frame: usize) {
        let mut v = self.shared.voice.lock().expect("poisoned");
        v.pos = frame as f64;
        self.shared.pos.store(frame as u64, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        let mut v = self.shared.voice.lock().expect("poisoned");
        v.playing = false;
        self.shared.playing.store(false, Ordering::Relaxed);
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    pub fn position(&self) -> usize {
        self.shared.pos.load(Ordering::Relaxed) as usize
    }

    pub fn set_volume(&self, v: f32) {
        self.shared.volume.store(v.to_bits(), Ordering::Relaxed);
    }

    pub fn set_speed(&self, s: f32) {
        self.shared.speed.store(s.to_bits(), Ordering::Relaxed);
    }

    /// Output peaks (L, R) since the last call, then reset.
    pub fn take_peaks(&self) -> [f32; 2] {
        self.shared
            .peak
            .each_ref()
            .map(|p| f32::from_bits(p.swap(0, Ordering::Relaxed)))
    }

    /// Whether the current voice is playing a stereo signal.
    pub fn is_stereo(&self) -> bool {
        self.is_playing() && self.shared.stereo.load(Ordering::Relaxed)
    }

    /// The most recent `n` output frames as (L, R), oldest first.
    pub fn scope(&self, n: usize) -> Vec<(f32, f32)> {
        let n = n.min(SCOPE_LEN);
        let end = self.shared.scope_at.load(Ordering::Acquire);
        (0..n)
            .map(|k| {
                let bits = self.shared.scope[(end + SCOPE_LEN - n + k) % SCOPE_LEN]
                    .load(Ordering::Relaxed);
                (
                    f32::from_bits(bits as u32),
                    f32::from_bits((bits >> 32) as u32),
                )
            })
            .collect()
    }

    /// Runtime error from the stream (device unplugged, etc).
    pub fn stream_error(&self) -> Option<String> {
        self.shared.error.lock().ok()?.clone()
    }
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    sh: Arc<Shared>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let out_ch = config.channels as usize;
    let dev_rate = config.sample_rate as f64;
    let err_sh = sh.clone();
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| render(data, out_ch, dev_rate, &sh),
            move |err| {
                if let Ok(mut e) = err_sh.error.lock() {
                    *e = Some(err.to_string());
                }
            },
            None,
        )
        .map_err(|e| e.to_string())
}

fn render<T: SizedSample + FromSample<f32>>(
    data: &mut [T],
    out_ch: usize,
    dev_rate: f64,
    sh: &Shared,
) {
    let silence = T::from_sample(0.0f32);
    let Ok(mut v) = sh.voice.try_lock() else {
        data.fill(silence);
        return;
    };
    let Some(sample) = v.sample.clone().filter(|_| v.playing) else {
        data.fill(silence);
        return;
    };

    // Play at the file's own rate on whatever the device runs at. Linear
    // interpolation is plenty for auditioning trim points.
    let speed = f32::from_bits(sh.speed.load(Ordering::Relaxed)) as f64;
    let step = sample.rate as f64 / dev_rate * speed.max(0.01);
    let src_ch = sample.channels.len();
    let frames = sample.frames();
    let end = v.end.min(frames).max(1);
    // The editor's gain goes through the limiter; master volume comes after,
    // so turning the monitor down doesn't change what gets limited.
    let lim = super::limit::Limiter::new(v.gain);
    let volume = f32::from_bits(sh.volume.load(Ordering::Relaxed));
    let mono = v.mono.filter(|_| src_ch > 1);
    let stereo = src_ch > 1 && mono.is_none() && out_ch > 1;
    sh.stereo.store(stereo, Ordering::Relaxed);
    let mut peak = [0.0f32; 2];
    let mut at = sh.scope_at.load(Ordering::Relaxed);

    for frame in data.chunks_mut(out_ch) {
        if !v.playing {
            frame.fill(silence);
            continue;
        }
        if v.pos >= end as f64 {
            if v.looping && v.loop_start < end {
                v.pos = v.loop_start as f64 + (v.pos - end as f64);
                if v.pos >= end as f64 {
                    v.pos = v.loop_start as f64;
                }
            } else {
                v.playing = false;
                frame.fill(silence);
                continue;
            }
        }
        let i = v.pos as usize;
        let t = (v.pos - i as f64) as f32;
        // Interpolate within the region only, so a loop seam sounds exactly as
        // the exported file will when looped on a sampler.
        let j = if i + 1 < end {
            i + 1
        } else if v.looping {
            v.loop_start
        } else {
            i
        };
        if let Some(fold) = mono {
            let a = fold.at(&sample.channels, i);
            let b = fold.at(&sample.channels, j);
            let x = (lim.apply(a + (b - a) * t) * volume).clamp(-1.0, 1.0);
            peak[0] = peak[0].max(x.abs());
            peak[1] = peak[0];
            frame.fill(T::from_sample(x));
            v.pos += step;
            continue;
        }
        let mut lr = [0.0f32; 2];
        for (k, out) in frame.iter_mut().enumerate() {
            let c = if src_ch == 1 {
                0
            } else if k < src_ch {
                k
            } else {
                usize::MAX
            };
            let s = if c == usize::MAX {
                0.0
            } else {
                let ch = &sample.channels[c];
                ch[i] + (ch[j] - ch[i]) * t
            };
            let x = (lim.apply(s) * volume).clamp(-1.0, 1.0);
            if k < 2 {
                lr[k] = x;
            }
            *out = T::from_sample(x);
        }
        if out_ch == 1 {
            lr[1] = lr[0];
        }
        peak[0] = peak[0].max(lr[0].abs());
        peak[1] = peak[1].max(lr[1].abs());
        if stereo {
            let bits = lr[0].to_bits() as u64 | (lr[1].to_bits() as u64) << 32;
            sh.scope[at].store(bits, Ordering::Relaxed);
            at = (at + 1) % SCOPE_LEN;
        }
        v.pos += step;
    }

    sh.scope_at.store(at, Ordering::Release);
    for (p, x) in sh.peak.iter().zip(peak) {
        p.fetch_max(x.to_bits(), Ordering::Relaxed);
    }

    sh.pos.store(v.pos as u64, Ordering::Relaxed);
    if !v.playing {
        sh.playing.store(false, Ordering::Relaxed);
    }
}
