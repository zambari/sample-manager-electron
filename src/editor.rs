//! The sample editor's model: what's loaded, the trim region, gain, playhead
//! and view. Drawing lives in [`crate::ui::editor_view`]; playback in
//! [`crate::audio::engine`]. Decoding and export run on worker threads and
//! report back through [`Editor::drain`], same pattern as [`crate::jobs`].

use crate::audio::decode::{self, Sample};
use crate::audio::export::{self, Render};
use crate::audio::peaks::ChannelPeaks;
use crate::audio::stats::{self, Stats};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

/// Editor toggles that survive a restart.
#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct Prefs {
    pub looping: bool,
    pub snap_zero: bool,
    /// Load whatever the file cursor lands on.
    pub auto_load: bool,
    /// ...and start playing it.
    pub auto_play: bool,
    pub normalize_db: f32,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            looping: true,
            snap_zero: false,
            auto_load: true,
            auto_play: false,
            normalize_db: -0.3,
        }
    }
}

pub struct Loaded {
    pub sample: Arc<Sample>,
    pub peaks: Vec<ChannelPeaks>,
}

/// Per-file edits kept for the session, so arrowing past a file you trimmed
/// and coming back doesn't lose your work.
#[derive(Clone, Copy)]
struct Memo {
    sel: (usize, usize),
    playhead: usize,
    gain_db: f32,
}

enum Ev {
    Loaded { generation: u64, path: PathBuf, result: Result<Loaded, String> },
    Exported(Result<PathBuf, String>),
}

/// What the app needs to react to after a drain.
pub enum Outcome {
    Loaded,
    Exported(PathBuf),
    Error(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Drag {
    In,
    Out,
    /// Rubber-band a new region from this frame.
    Select(usize),
    Pan,
}

/// Visible window: first frame at the left edge and frames per pixel.
#[derive(Clone, Copy)]
pub struct View {
    pub start: f64,
    pub fpp: f64,
    /// Fit to width on the next draw (we don't know the width until then).
    pub fit_pending: bool,
}

pub struct Editor {
    tx: Sender<Ev>,
    rx: Receiver<Ev>,
    ctx: egui::Context,
    generation: u64,
    pub loading: Option<PathBuf>,
    pub current: Option<Loaded>,
    pub error: Option<String>,
    /// Trim region, `[in, out)` in frames.
    pub sel: (usize, usize),
    /// Parked play-start position.
    pub playhead: usize,
    pub gain_db: f32,
    pub prefs: Prefs,
    pub view: View,
    pub drag: Option<Drag>,
    /// Where the waveform was last drawn, so keyboard zoom knows the width.
    pub wave_rect: Option<egui::Rect>,
    pub exporting: bool,
    /// Start playing once the pending load lands.
    pub play_when_loaded: bool,
    memos: HashMap<PathBuf, Memo>,
    stats_key: Option<(usize, usize, u32, usize)>,
    stats: Stats,
    file_stats: Stats,
}

impl Editor {
    pub fn new(ctx: egui::Context, prefs: Prefs) -> Self {
        let (tx, rx) = channel();
        Self {
            tx,
            rx,
            ctx,
            generation: 0,
            loading: None,
            current: None,
            error: None,
            sel: (0, 0),
            playhead: 0,
            gain_db: 0.0,
            prefs,
            view: View { start: 0.0, fpp: 1.0, fit_pending: true },
            drag: None,
            wave_rect: None,
            exporting: false,
            play_when_loaded: false,
            memos: HashMap::new(),
            stats_key: None,
            stats: Stats::default(),
            file_stats: Stats::default(),
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.current.as_ref().map(|c| c.sample.path.as_path())
    }

    pub fn sample(&self) -> Option<&Arc<Sample>> {
        self.current.as_ref().map(|c| &c.sample)
    }

    pub fn frames(&self) -> usize {
        self.sample().map_or(0, |s| s.frames())
    }

    pub fn gain(&self) -> f32 {
        stats::from_db(self.gain_db)
    }

    /// Ask for `path` to be loaded. A no-op if it already is (or is on its way).
    pub fn request(&mut self, path: &Path) {
        if self.path() == Some(path) || self.loading.as_deref() == Some(path) {
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        self.loading = Some(path.to_path_buf());
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let path = path.to_path_buf();
        let _ = std::thread::Builder::new().name("decode".into()).spawn(move || {
            let result = decode::decode(&path).map(|sample| {
                let peaks = sample.channels.iter().map(|c| ChannelPeaks::build(c)).collect();
                Loaded { sample: Arc::new(sample), peaks }
            });
            let _ = tx.send(Ev::Loaded { generation, path, result });
            ctx.request_repaint();
        });
    }

    pub fn drain(&mut self) -> Vec<Outcome> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Ev::Loaded { generation, path, result } => {
                    if generation != self.generation {
                        continue; // the cursor moved on
                    }
                    self.loading = None;
                    match result {
                        Ok(loaded) => {
                            self.remember();
                            self.install(loaded);
                            out.push(Outcome::Loaded);
                        }
                        Err(e) => {
                            self.play_when_loaded = false;
                            self.error = Some(format!(
                                "{}: {e}",
                                path.file_name().unwrap_or_default().to_string_lossy()
                            ));
                        }
                    }
                }
                Ev::Exported(r) => {
                    self.exporting = false;
                    out.push(match r {
                        Ok(p) => Outcome::Exported(p),
                        Err(e) => Outcome::Error(format!("export failed: {e}")),
                    });
                }
            }
        }
        out
    }

    fn remember(&mut self) {
        let Some(path) = self.path().map(Path::to_path_buf) else { return };
        let memo = Memo { sel: self.sel, playhead: self.playhead, gain_db: self.gain_db };
        self.memos.insert(path, memo);
    }

    fn install(&mut self, loaded: Loaded) {
        let frames = loaded.sample.frames();
        let memo = self.memos.get(&loaded.sample.path).copied();
        self.current = Some(loaded);
        self.error = None;
        self.drag = None;
        self.stats_key = None;
        match memo {
            Some(m) if m.sel.1 <= frames => {
                self.sel = m.sel;
                self.playhead = m.playhead.min(frames);
                self.gain_db = m.gain_db;
            }
            _ => {
                self.sel = (0, frames);
                self.playhead = 0;
                self.gain_db = 0.0;
            }
        }
        self.view.fit_pending = true;
        if let Some(c) = &self.current {
            self.file_stats = stats::compute(&c.sample.channels, 0, frames, 1.0);
        }
    }

    // ---- edits ------------------------------------------------------------

    pub fn set_in(&mut self, frame: usize) {
        let f = self.snap(frame);
        self.sel.0 = f.min(self.sel.1.saturating_sub(1));
    }

    pub fn set_out(&mut self, frame: usize) {
        let f = self.snap(frame);
        self.sel.1 = f.max(self.sel.0 + 1).min(self.frames());
    }

    /// Set both ends at once from an unordered pair (rubber-band select).
    pub fn set_region(&mut self, a: usize, b: usize) {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let lo = self.snap(lo);
        let hi = self.snap(hi).min(self.frames());
        self.sel = (lo, hi.max(lo + 1).min(self.frames().max(1)));
    }

    pub fn select_all(&mut self) {
        self.sel = (0, self.frames());
    }

    pub fn reset(&mut self) {
        self.select_all();
        self.gain_db = 0.0;
    }

    pub fn normalize(&mut self) {
        let peak = self.region_stats().peak;
        if peak > 0.0 {
            self.gain_db = (self.prefs.normalize_db - stats::to_db(peak)).clamp(-60.0, 60.0);
        }
    }

    /// Nearest zero crossing (summed channels) within ~3 ms, if snapping is on.
    fn snap(&self, frame: usize) -> usize {
        let Some(s) = self.sample() else { return frame };
        let n = s.frames();
        let frame = frame.min(n);
        if !self.prefs.snap_zero || n < 2 {
            return frame;
        }
        let mono = |i: usize| -> f32 { s.channels.iter().map(|c| c[i]).sum() };
        let window = (s.rate as usize * 3 / 1000).max(8);
        for d in 0..window {
            for cand in [frame.checked_sub(d), Some(frame + d)].into_iter().flatten() {
                if cand >= 1 && cand < n && (mono(cand - 1) <= 0.0) != (mono(cand) <= 0.0) {
                    return cand;
                }
            }
        }
        frame
    }

    // ---- analysis ---------------------------------------------------------

    pub fn region_stats(&mut self) -> Stats {
        let Some(c) = &self.current else { return Stats::default() };
        let key = (self.sel.0, self.sel.1, self.gain().to_bits(), c.sample.frames());
        if self.stats_key != Some(key) {
            self.stats = stats::compute(&c.sample.channels, self.sel.0, self.sel.1, self.gain());
            self.stats_key = Some(key);
        }
        self.stats
    }

    pub fn file_stats(&self) -> Stats {
        self.file_stats
    }

    /// Have the trim or gain moved away from "the file as it is"?
    pub fn is_edited(&self) -> bool {
        self.sel != (0, self.frames()) || self.gain_db.abs() > 0.001
    }

    // ---- export -----------------------------------------------------------

    pub fn export(&mut self, dest_dir: PathBuf) {
        let Some(sample) = self.sample().cloned() else { return };
        if self.exporting {
            return;
        }
        self.exporting = true;
        let r = Render { start: self.sel.0, end: self.sel.1, gain: self.gain() };
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let _ = std::thread::Builder::new().name("export".into()).spawn(move || {
            let res = export::write_wav(&sample, &r, &dest_dir);
            let _ = tx.send(Ev::Exported(res));
            ctx.request_repaint();
        });
    }
}
