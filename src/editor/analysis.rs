//! Region analysis for the stats panel: loudness / true peak and the
//! waterfall spectra. Nothing here runs unless the panel asks, and each
//! result is kept until the region, the audio or the mono fold changes.
//! Gain is applied by the caller, so dragging it never recomputes.

use super::Editor;
use crate::audio::loudness::{self, Loudness};
use crate::audio::spectrum::Analyzer;
use crate::audio::stats::Fold;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Rows in the waterfall, back to front.
pub const ROWS: usize = 48;
/// Points along each row.
pub const BANDS: usize = 120;
/// FFT size for the still (region) waterfall.
const STILL_FFT: usize = 4096;
/// FFT size while playing: shorter, so transients stay sharp in time.
const LIVE_FFT: usize = 2048;
/// Live rows per second of audio. ROWS / this = seconds of history.
const LIVE_ROWS_PER_SEC: usize = 48;

/// What a cached result was computed for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Key {
    sample: usize,
    frames: usize,
    rev: u64,
    sel: (usize, usize),
    fold: Option<Fold>,
}

#[derive(Default)]
pub struct Analysis {
    /// Latest finished loudness, and what it was for.
    loud: Arc<Mutex<Option<(Key, Loudness)>>>,
    /// Bumped per request; a worker whose number is stale gives up.
    loud_gen: Arc<AtomicU64>,
    loud_asked: Option<Key>,
    still: Option<(Key, Arc<Vec<Vec<f32>>>)>,
    live: Live,
}

#[derive(Default)]
struct Live {
    /// Key without the selection: moving the region doesn't reset history.
    key: Option<Key>,
    rows: VecDeque<Vec<f32>>,
    /// Frame the next row ends at.
    next: usize,
    fft: Option<Analyzer>,
}

/// A loudness reading, possibly for an earlier region while the new one
/// is still being measured.
pub struct LoudnessView {
    pub value: Option<Loudness>,
    pub pending: bool,
}

impl Editor {
    fn analysis_key(&self) -> Option<Key> {
        let c = self.current.as_ref()?;
        Some(Key {
            sample: Arc::as_ptr(&c.sample) as usize,
            frames: c.sample.frames(),
            rev: self.rev,
            sel: self.sel,
            fold: self.fold(),
        })
    }

    /// Loudness of the region (before gain). Starts a measurement in the
    /// background when the region changed; meanwhile the last reading is
    /// returned with `pending` set.
    pub fn region_loudness(&mut self) -> LoudnessView {
        let Some(key) = self.analysis_key() else {
            return LoudnessView { value: None, pending: false };
        };
        let a = &mut self.analysis;
        let have = *a.loud.lock().unwrap();
        if a.loud_asked != Some(key) {
            a.loud_asked = Some(key);
            let generation = a.loud_gen.fetch_add(1, Ordering::SeqCst) + 1;
            let gen_now = a.loud_gen.clone();
            let slot = a.loud.clone();
            let sample = self.current.as_ref().unwrap().sample.clone();
            let ctx = self.ctx.clone();
            let _ = std::thread::Builder::new()
                .name("loudness".into())
                .spawn(move || {
                    let (s, e) = (key.sel.0.min(key.frames), key.sel.1.min(key.frames));
                    let (s, e) = (s.min(e), e);
                    let cancelled = || gen_now.load(Ordering::Relaxed) != generation;
                    let r = if let Some(fold) = key.fold {
                        let m = crate::audio::stats::fold_region(&sample.channels, s, e, fold);
                        loudness::measure(&[&m], sample.rate, &cancelled)
                    } else {
                        let chans: Vec<&[f32]> = sample.channels.iter().map(|c| &c[s..e]).collect();
                        loudness::measure(&chans, sample.rate, &cancelled)
                    };
                    if let Some(r) = r
                        && !cancelled()
                    {
                        *slot.lock().unwrap() = Some((key, r));
                        ctx.request_repaint();
                    }
                });
        }
        LoudnessView {
            value: have.map(|(_, l)| l),
            pending: have.is_none_or(|(k, _)| k != key),
        }
    }

    /// Waterfall of the region: `ROWS` spectra spread across it, first row
    /// at the in point. dB per band, before gain.
    pub fn region_waterfall(&mut self) -> Option<Arc<Vec<Vec<f32>>>> {
        let key = self.analysis_key()?;
        if let Some((k, rows)) = &self.analysis.still
            && *k == key
        {
            return Some(rows.clone());
        }
        let s = self.sample()?.clone();
        let fold = key.fold.unwrap_or(Fold::Mix);
        let (a, b) = (key.sel.0.min(key.frames), key.sel.1.min(key.frames));
        let len = b.saturating_sub(a);
        let mut fft = Analyzer::new(STILL_FFT, s.rate, BANDS);
        let rows: Vec<Vec<f32>> = (0..ROWS)
            .map(|i| {
                // Window centred on its slice of the region, in region frames;
                // audio outside the region reads as silence.
                let centre = (i as f64 + 0.5) / ROWS as f64 * len as f64;
                let start = centre as isize - (STILL_FFT / 2) as isize;
                fft.frame(|f| fold.at(&s.channels, a + f), len, start)
            })
            .collect();
        let rows = Arc::new(rows);
        self.analysis.still = Some((key, rows.clone()));
        Some(rows)
    }

    /// Rolling waterfall up to the play position, oldest row first. Rows
    /// are added at a fixed rate of audio time; a jump (seek, loop wrap)
    /// refills the history from the audio just before `pos`.
    pub fn live_waterfall(&mut self, pos: usize) -> Vec<Vec<f32>> {
        let Some(mut key) = self.analysis_key() else {
            return Vec::new();
        };
        key.sel = (0, 0);
        let Some(s) = self.sample().cloned() else {
            return Vec::new();
        };
        let fold = key.fold.unwrap_or(Fold::Mix);
        let hop = (s.rate as usize / LIVE_ROWS_PER_SEC).max(1);
        let live = &mut self.analysis.live;
        let jumped = pos + hop < live.next || pos > live.next + hop * ROWS;
        if live.key != Some(key) || jumped {
            live.key = Some(key);
            live.rows.clear();
            live.next = pos.saturating_sub(hop * (ROWS - 1));
        }
        if live.fft.as_ref().is_none_or(|f| f.rate() != s.rate) {
            live.fft = Some(Analyzer::new(LIVE_FFT, s.rate, BANDS));
        }
        let fft = live.fft.as_mut().unwrap();
        let n = fft.size();
        let frames = s.frames();
        while live.next <= pos {
            let start = live.next as isize - n as isize;
            let row = fft.frame(|f| fold.at(&s.channels, f), frames, start);
            live.rows.push_back(row);
            if live.rows.len() > ROWS {
                live.rows.pop_front();
            }
            live.next += hop;
        }
        live.rows.iter().cloned().collect()
    }
}
