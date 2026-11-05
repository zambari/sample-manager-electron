//! The sample editor's model: what's loaded, the trim region, gain, playhead
//! and view. Drawing lives in [`crate::ui::editor_view`]; playback in
//! [`crate::audio::engine`]. Decoding and export run on worker threads and
//! report back through [`Editor::drain`], same pattern as [`crate::jobs`].

use crate::audio::cues;
use crate::audio::decode::{self, Sample};
use crate::audio::export::{self, Render};
use crate::audio::peaks::ChannelPeaks;
use crate::audio::stats::{self, Fold, Phase, Stats};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

mod clip;
mod history;
mod selection;

use history::History;

/// Editor toggles that survive a restart.
#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct Prefs {
    /// Not saved: always starts at the default.
    #[serde(skip)]
    pub looping: bool,
    pub snap_zero: bool,
    /// Load whatever the file cursor lands on.
    pub auto_load: bool,
    /// ...and start playing it.
    pub auto_play: bool,
    pub normalize_db: f32,
    /// Space starts at the in point instead of the parked playhead.
    pub play_from_in: bool,
    /// Scroll the waveform along with the play cursor.
    pub follow_playback: bool,
    /// Fold multichannel files to one channel: what you hear, the stats,
    /// normalize and export all follow. Not saved: always starts off.
    #[serde(skip)]
    pub mono: Option<Fold>,
    /// Last fold picked, so M turns mono back on the same way.
    pub fold: Fold,
    /// Which editor panels are shown.
    pub panels: Panels,
}

/// Visibility of each editor panel, toggled from the header row.
#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct Panels {
    /// Play / loop / snap toggles.
    pub transport: bool,
    /// In / out / len / playhead readout, under the waveform.
    pub nums: bool,
    pub gain: bool,
    /// Undo, crop, loop scaling, export.
    pub tools: bool,
    /// The main zoomable waveform.
    pub wave: bool,
    /// The fixed 40 ms extreme-zoom strip.
    pub scalpel: bool,
    pub minimap: bool,
    /// Stats table and phase meter.
    pub stats: bool,
    /// Keyboard shortcut cheat sheet at the bottom.
    pub help: bool,
}

impl Default for Panels {
    fn default() -> Self {
        Self { transport: true, nums: true, gain: true, tools: true, wave: true, scalpel: true, minimap: true, stats: true, help: true }
    }
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            looping: true,
            snap_zero: false,
            auto_load: true,
            auto_play: false,
            normalize_db: -0.3,
            play_from_in: false,
            follow_playback: true,
            mono: None,
            fold: Fold::Mix,
            panels: Panels::default(),
        }
    }
}

/// The file in the editor and its edits, saved so a restart picks up where
/// you left off.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub path: PathBuf,
    pub memo: Memo,
}

#[derive(Clone)]
pub struct Loaded {
    pub sample: Arc<Sample>,
    pub peaks: Arc<Vec<ChannelPeaks>>,
}

impl Loaded {
    fn new(sample: Sample) -> Self {
        let peaks = sample.channels.iter().map(|c| ChannelPeaks::build(c)).collect();
        Self { sample: Arc::new(sample), peaks: Arc::new(peaks) }
    }
}

/// Per-file edits kept for the session, so arrowing past a file you trimmed
/// and coming back doesn't lose your work. Positions are in whole-file frames
/// so a crop can be re-applied after the file is decoded again.
#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Memo {
    sel: (usize, usize),
    playhead: usize,
    gain_db: f32,
    crop: Option<(usize, usize)>,
}

impl Memo {
    /// "Open it fresh": the out-of-range selection makes `install` fall back
    /// to the whole file at unity gain.
    const FRESH: Memo = Memo { sel: (0, usize::MAX), playhead: 0, gain_db: 0.0, crop: None };
}

/// Audio cut or copied in the editor, kept across file switches so it can be
/// pasted into another file. Gain is baked in: it's what you heard.
pub struct AudioClip {
    pub rate: u32,
    pub channels: Vec<Vec<f32>>,
}

impl AudioClip {
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    /// The clip reshaped for a file with `nch` channels at `rate`: mono is
    /// spread to every channel, multichannel folded to mono, and a different
    /// rate linearly resampled so pitch and length are kept.
    fn adapt(&self, rate: u32, nch: usize) -> Vec<Vec<f32>> {
        let src = &self.channels;
        let chans: Vec<Vec<f32>> = if src.len() == nch {
            src.clone()
        } else if nch == 1 {
            let n = self.frames();
            vec![(0..n).map(|f| stats::mono_at(src, f)).collect()]
        } else {
            (0..nch).map(|i| src[i % src.len()].clone()).collect()
        };
        if self.rate == rate || self.rate == 0 || rate == 0 {
            return chans;
        }
        let ratio = self.rate as f64 / rate as f64;
        let n_in = self.frames();
        let n_out = ((n_in as f64) / ratio).round() as usize;
        chans
            .iter()
            .map(|c| {
                (0..n_out)
                    .map(|i| {
                        let x = i as f64 * ratio;
                        let j = x.floor() as usize;
                        let t = (x - j as f64) as f32;
                        let a = c.get(j).copied().unwrap_or(0.0);
                        let b = c.get(j + 1).copied().unwrap_or(a);
                        a + (b - a) * t
                    })
                    .collect()
            })
            .collect()
    }
}


enum Ev {
    /// `cues`: slice markers stored in the file, for a kit.
    Loaded { generation: u64, path: PathBuf, result: Result<Loaded, String>, cues: Vec<usize> },
    /// `rev` is the content revision that was rendered.
    Exported { rev: u64, result: Result<PathBuf, String> },
    BatchExported { dest_dir: PathBuf, done: usize, errors: Vec<String> },
}

/// What the app needs to react to after a drain.
pub enum Outcome {
    Loaded,
    Exported(PathBuf),
    /// Several files folded to mono into `dest_dir`.
    BatchExported(PathBuf, usize),
    Error(String),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Drag {
    In,
    Out,
    /// Rubber-band a new region from this frame.
    Select(usize),
    Pan,
    /// Slide the whole region. Holds the region and the pointer frame at
    /// the start of the drag.
    Move((usize, usize), usize),
    /// Scope strip: move the playhead by dragging the wave. Holds the
    /// playhead and pointer x at the start of the drag.
    Scrub(usize, f32),
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
    /// The whole decoded file. `current` is this, or a crop of it.
    full: Option<Loaded>,
    /// Crop range in whole-file frames; frame 0 of `current` is `crop.0`.
    pub crop: Option<(usize, usize)>,
    history: History,
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
    /// This slot holds the slice kit: export writes the whole file with its
    /// slice markers, and the markers are drawn and edited.
    pub is_kit: bool,
    /// Kit slice starts, in whole-file frames, sorted, strictly inside the
    /// file (0 and the end are implied).
    markers: Vec<usize>,
    memos: HashMap<PathBuf, Memo>,
    stats_key: Option<(usize, usize, u32, usize, Option<Fold>)>,
    /// Content revision: bumped by every cut / paste. Unique per edit so undo
    /// and redo land back on a matching number.
    rev: u64,
    next_rev: u64,
    /// `rev` when the file was loaded, and when it was last written out.
    loaded_rev: u64,
    clean_rev: u64,
    /// Audio cut or copied, for pasting (into this file or another).
    pub clip: Option<AudioClip>,
    stats: Stats,
    phase: Phase,
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
            full: None,
            crop: None,
            history: History::default(),
            sel: (0, 0),
            playhead: 0,
            gain_db: 0.0,
            prefs,
            view: View { start: 0.0, fpp: 1.0, fit_pending: true },
            drag: None,
            wave_rect: None,
            exporting: false,
            play_when_loaded: false,
            is_kit: false,
            markers: Vec::new(),
            memos: HashMap::new(),
            stats_key: None,
            rev: 0,
            next_rev: 0,
            loaded_rev: 0,
            clean_rev: 0,
            clip: None,
            stats: Stats::default(),
            phase: Phase::default(),
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

    pub fn is_multichannel(&self) -> bool {
        self.sample().is_some_and(|s| s.channels.len() > 1)
    }

    /// The fold in effect: mono is on and there is more than one channel.
    pub fn fold(&self) -> Option<Fold> {
        self.prefs.mono.filter(|_| self.is_multichannel())
    }

    /// Mono is on and there is more than one channel to fold.
    pub fn mono_active(&self) -> bool {
        self.fold().is_some()
    }

    /// Export would fold channels together.
    pub fn exports_mono(&self) -> bool {
        self.mono_active()
    }

    /// Ask for `path` to be loaded. A no-op if it already is (or is on its way).
    pub fn request(&mut self, path: &Path) {
        if self.path() == Some(path) || self.loading.as_deref() == Some(path) {
            return;
        }
        self.load(path);
    }

    /// Re-read the open file from disk, e.g. after an export replaced it.
    pub fn reload(&mut self) {
        if let Some(p) = self.path().map(Path::to_path_buf) {
            self.load(&p);
        }
    }

    fn load(&mut self, path: &Path) {
        self.generation += 1;
        let generation = self.generation;
        self.loading = Some(path.to_path_buf());
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let path = path.to_path_buf();
        let _ = std::thread::Builder::new().name("decode".into()).spawn(move || {
            let result = decode::decode(&path).map(Loaded::new);
            let cues = cues::read(&path);
            let _ = tx.send(Ev::Loaded { generation, path, result, cues });
            ctx.request_repaint();
        });
    }

    pub fn drain(&mut self) -> Vec<Outcome> {
        let mut out = Vec::new();
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Ev::Loaded { generation, path, result, cues } => {
                    if generation != self.generation {
                        continue; // the cursor moved on
                    }
                    self.loading = None;
                    match result {
                        Ok(loaded) => {
                            self.remember();
                            self.install(loaded, cues);
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
                Ev::Exported { rev, result } => {
                    self.exporting = false;
                    out.push(match result {
                        Ok(p) => {
                            // The edits are safely written somewhere.
                            self.clean_rev = rev;
                            Outcome::Exported(p)
                        }
                        Err(e) => Outcome::Error(format!("export failed: {e}")),
                    });
                }
                Ev::BatchExported { dest_dir, done, errors } => {
                    out.extend(errors.into_iter().map(|e| Outcome::Error(format!("export failed: {e}"))));
                    out.push(Outcome::BatchExported(dest_dir, done));
                }
            }
        }
        out
    }

    fn memo(&self) -> Memo {
        let off = self.crop_offset();
        Memo {
            sel: (self.sel.0 + off, self.sel.1 + off),
            playhead: self.playhead + off,
            gain_db: self.gain_db,
            crop: self.crop,
        }
    }

    /// Positions only make sense for the file as it is on disk: once its
    /// audio was cut or pasted into, forget them instead.
    fn remember(&mut self) {
        let Some(path) = self.path().map(Path::to_path_buf) else { return };
        if self.rev != self.loaded_rev {
            self.memos.remove(&path);
            return;
        }
        let memo = self.memo();
        self.memos.insert(path, memo);
    }

    /// The loaded file and its edits, for saving.
    pub fn session(&self) -> Option<Session> {
        let memo = if self.rev == self.loaded_rev { self.memo() } else { Memo::FRESH };
        Some(Session { path: self.path()?.to_path_buf(), memo })
    }

    /// Reload a saved session: the edits are re-applied once the file decodes.
    pub fn restore_session(&mut self, s: &Session) {
        self.memos.insert(s.path.clone(), s.memo);
        self.request(&s.path);
    }

    fn install(&mut self, loaded: Loaded, cues: Vec<usize>) {
        let frames = loaded.sample.frames();
        self.markers = if self.is_kit { cues.into_iter().filter(|&m| m > 0 && m < frames).collect() } else { Vec::new() };
        let memo = self.memos.get(&loaded.sample.path).copied();
        self.file_stats = stats::compute(&loaded.sample.channels, 0, frames, 1.0);
        self.full = Some(loaded.clone());
        self.current = Some(loaded);
        self.crop = None;
        self.error = None;
        self.drag = None;
        self.stats_key = None;
        match memo {
            Some(m) if m.sel.1 <= frames => {
                self.sel = m.sel;
                self.playhead = m.playhead.min(frames);
                self.gain_db = m.gain_db;
                if let Some((a, b)) = m.crop.filter(|c| c.1 <= frames && c.0 < c.1) {
                    self.apply_crop(a, b);
                }
            }
            _ => {
                self.sel = (0, frames);
                self.playhead = 0;
                self.gain_db = 0.0;
            }
        }
        self.view.fit_pending = true;
        self.next_rev += 1;
        self.rev = self.next_rev;
        self.loaded_rev = self.rev;
        self.clean_rev = self.rev;
        self.history = History { committed: Some(self.state()), ..History::default() };
    }

    // ---- crop -------------------------------------------------------------

    fn crop_offset(&self) -> usize {
        self.crop.map_or(0, |c| c.0)
    }

    /// Show only `[a, b)` of the whole file (whole-file frames), with the
    /// selection and playhead given in whole-file frames too and mapped in.
    fn apply_crop(&mut self, a: usize, b: usize) {
        let Some(full) = &self.full else { return };
        let fs = &full.sample;
        let sample = Sample {
            path: fs.path.clone(),
            rate: fs.rate,
            channels: fs.channels.iter().map(|c| c[a..b].to_vec()).collect(),
            bits: fs.bits,
            format: fs.format.clone(),
            file_len: fs.file_len,
        };
        let len = b - a;
        self.current = Some(Loaded::new(sample));
        self.crop = Some((a, b));
        let map = |f: usize| f.saturating_sub(a).min(len);
        self.sel = (map(self.sel.0), map(self.sel.1).max(map(self.sel.0) + 1).min(len));
        self.playhead = map(self.playhead);
        self.stats_key = None;
        self.view.fit_pending = true;
    }

    /// Crop the in-memory view to the selection, for finer work inside it.
    /// The file on disk is untouched; [`Editor::uncrop`] brings it all back.
    pub fn crop_to_selection(&mut self) {
        if self.current.is_none() || self.sel == (0, self.frames()) {
            return;
        }
        let off = self.crop_offset();
        let (a, b) = (self.sel.0 + off, self.sel.1 + off);
        self.sel = (a, b);
        self.playhead += off;
        self.apply_crop(a, b);
    }

    pub fn uncrop(&mut self) {
        let Some((a, _)) = self.crop else { return };
        let Some(full) = self.full.clone() else { return };
        self.sel = (self.sel.0 + a, self.sel.1 + a);
        self.playhead += a;
        self.current = Some(full);
        self.crop = None;
        self.stats_key = None;
        self.view.fit_pending = true;
    }

    // ---- analysis ---------------------------------------------------------

    /// Level stats for the region as you hear it: folded to mono when mono
    /// preview is on, so normalize targets the mono sum's peak.
    pub fn region_stats(&mut self) -> Stats {
        self.refresh_region();
        self.stats
    }

    /// L/R correlation and mono-fold loss over the region.
    pub fn region_phase(&mut self) -> Phase {
        self.refresh_region();
        self.phase
    }

    fn refresh_region(&mut self) {
        let mono = self.fold();
        let Some(c) = &self.current else {
            self.stats = Stats::default();
            self.phase = Phase::default();
            return;
        };
        let key = (self.sel.0, self.sel.1, self.gain().to_bits(), c.sample.frames(), mono);
        if self.stats_key == Some(key) {
            return;
        }
        let ch = &c.sample.channels;
        self.stats = if let Some(fold) = mono {
            let m = stats::fold_region(ch, self.sel.0, self.sel.1, fold);
            let n = m.len();
            stats::compute(&[m], 0, n, self.gain())
        } else {
            stats::compute(ch, self.sel.0, self.sel.1, self.gain())
        };
        self.phase = stats::phase(ch, self.sel.0, self.sel.1);
        self.stats_key = Some(key);
    }

    /// Correlation in a short window around `frame`, for the live meter.
    pub fn phase_at(&self, frame: usize) -> Phase {
        let Some(s) = self.sample() else { return Phase::default() };
        let half = (s.rate as usize / 40).max(256); // ~50 ms window
        stats::phase(&s.channels, frame.saturating_sub(half), frame + half)
    }

    pub fn file_stats(&self) -> Stats {
        self.file_stats
    }

    /// Have the trim, gain or audio moved away from "the file as it is"?
    pub fn is_edited(&self) -> bool {
        self.rev != self.loaded_rev || self.crop.is_some() || self.sel != (0, self.frames()) || self.gain_db.abs() > 0.001
    }

    /// Give up on the unsaved audio edits. They stay on screen until the
    /// next load, but no longer hold anything up.
    pub fn discard(&mut self) {
        self.clean_rev = self.rev;
    }

    /// Audio was cut or pasted into and that hasn't been written anywhere yet.
    pub fn is_dirty(&self) -> bool {
        self.current.is_some() && self.rev != self.clean_rev
    }

    fn refresh_file_stats(&mut self) {
        if let Some(f) = &self.full {
            let n = f.sample.frames();
            self.file_stats = stats::compute(&f.sample.channels, 0, n, 1.0);
        }
    }


    // ---- export -----------------------------------------------------------

    /// Write the edited region to exactly `target` (overwriting it).
    /// A kit ignores in / out and the crop: the whole file goes out, with its
    /// slice markers as cue points.
    pub fn export(&mut self, target: PathBuf, mono: Option<Fold>) {
        let kit = self.is_kit;
        let shown = if kit { self.full.as_ref().map(|l| &l.sample) } else { self.sample() };
        let Some(sample) = shown.cloned() else { return };
        if self.exporting {
            return;
        }
        self.exporting = true;
        let r = if kit {
            Render { start: 0, end: sample.frames(), gain: self.gain(), mono }
        } else {
            Render { start: self.sel.0, end: self.sel.1, gain: self.gain(), mono }
        };
        let marks = if kit { self.markers.clone() } else { Vec::new() };
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let rev = self.rev;
        let _ = std::thread::Builder::new().name("export".into()).spawn(move || {
            let result = export::write_wav(&sample, &r, &target).and_then(|p| cues::append(&p, &marks).map(|_| p));
            let _ = tx.send(Ev::Exported { rev, result });
            ctx.request_repaint();
        });
    }

    /// Fold whole files to mono, untrimmed and at unity gain, each `(src,
    /// target)` pair writing exactly `target`. Runs independently of the
    /// single-file export.
    pub fn export_mono_batch(&self, jobs: Vec<(PathBuf, PathBuf)>, dest_dir: PathBuf, fold: Fold) {
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let _ = std::thread::Builder::new().name("export-batch".into()).spawn(move || {
            let mut done = 0;
            let mut errors = Vec::new();
            for (p, target) in jobs {
                let res = decode::decode(&p).and_then(|s| {
                    let r = Render { start: 0, end: s.frames(), gain: 1.0, mono: Some(fold) };
                    export::write_wav(&s, &r, &target)
                });
                match res {
                    Ok(_) => done += 1,
                    Err(e) => errors.push(format!("{}: {e}", p.display())),
                }
            }
            let _ = tx.send(Ev::BatchExported { dest_dir, done, errors });
            ctx.request_repaint();
        });
    }
}
