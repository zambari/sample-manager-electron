//! The sample editor's model: what's loaded, the trim region, gain, playhead
//! and view. Drawing lives in [`crate::ui::editor_view`]; playback in
//! [`crate::audio::engine`]. Decoding and export run on worker threads and
//! report back through [`Editor::drain`], same pattern as [`crate::jobs`].

use crate::audio::decode::{self, Sample};
use crate::audio::export::{self, Render};
use crate::audio::peaks::ChannelPeaks;
use crate::audio::stats::{self, Fold, Phase, Stats};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

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

/// One undo step: the trim, playhead, gain and crop. Prefs (loop, snap...)
/// are deliberately left out.
#[derive(Clone)]
struct EditState {
    sel: (usize, usize),
    playhead: usize,
    gain_db: f32,
    crop: Option<(usize, usize)>,
    /// The (possibly cropped) sample that goes with `crop`. Cheap: Arcs.
    shown: Option<Loaded>,
    /// The whole edited file and its content revision (cut / paste).
    full: Option<Loaded>,
    rev: u64,
}

const CH_IN: u8 = 1;
const CH_OUT: u8 = 2;
const CH_PLAYHEAD: u8 = 4;
const CH_GAIN: u8 = 8;
const CH_CROP: u8 = 16;
const CH_AUDIO: u8 = 32;

impl EditState {
    /// Which fields differ, as a CH_* mask.
    fn diff(&self, o: &EditState) -> u8 {
        let mut m = 0;
        if self.sel.0 != o.sel.0 {
            m |= CH_IN;
        }
        if self.sel.1 != o.sel.1 {
            m |= CH_OUT;
        }
        if self.playhead != o.playhead {
            m |= CH_PLAYHEAD;
        }
        if self.gain_db.to_bits() != o.gain_db.to_bits() {
            m |= CH_GAIN;
        }
        if self.crop != o.crop {
            m |= CH_CROP;
        }
        if self.rev != o.rev {
            m |= CH_AUDIO;
        }
        m
    }
}

#[derive(Default)]
struct History {
    undo: Vec<EditState>,
    redo: Vec<EditState>,
    /// State as of the last recorded step; edits are diffed against it.
    committed: Option<EditState>,
    /// What changed in the last step and when, to fold key-repeat runs
    /// (holding `]`, nudging with arrows) into one undo step.
    last_mask: u8,
    last_time: f64,
}

const UNDO_LIMIT: usize = 300;
/// Same-kind edits closer together than this merge into one undo step.
const COALESCE_SECS: f64 = 0.6;

enum Ev {
    Loaded { generation: u64, path: PathBuf, result: Result<Loaded, String> },
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

    fn install(&mut self, loaded: Loaded) {
        let frames = loaded.sample.frames();
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

    // ---- undo -------------------------------------------------------------

    fn state(&self) -> EditState {
        EditState {
            sel: self.sel,
            playhead: self.playhead,
            gain_db: self.gain_db,
            crop: self.crop,
            shown: self.current.clone(),
            full: self.full.clone(),
            rev: self.rev,
        }
    }

    fn restore(&mut self, st: EditState) {
        if st.rev != self.rev {
            self.full = st.full;
            self.rev = st.rev;
            self.current = st.shown;
            self.crop = st.crop;
            self.refresh_file_stats();
        } else if st.crop != self.crop {
            self.current = st.shown;
            self.crop = st.crop;
            self.view.fit_pending = true;
        }
        self.sel = st.sel;
        self.playhead = st.playhead;
        self.gain_db = st.gain_db;
        self.stats_key = None;
    }

    /// Record an undo step if anything changed since the last one. Call once
    /// per frame; pass `busy` while a drag / slider / typed edit is still in
    /// progress so the whole gesture lands as one step when it ends.
    pub fn track_history(&mut self, busy: bool, now: f64) {
        if busy || self.current.is_none() {
            return;
        }
        let now_state = self.state();
        let h = &mut self.history;
        let Some(prev) = h.committed.take() else {
            h.committed = Some(now_state);
            return;
        };
        let mask = prev.diff(&now_state);
        if mask == 0 {
            h.committed = Some(prev);
            return;
        }
        let coalesce = mask == h.last_mask && mask & (CH_CROP | CH_AUDIO) == 0 && now - h.last_time < COALESCE_SECS;
        if !coalesce {
            h.undo.push(prev);
            if h.undo.len() > UNDO_LIMIT {
                h.undo.remove(0);
            }
        }
        h.redo.clear();
        h.committed = Some(now_state);
        h.last_mask = mask;
        h.last_time = now;
    }

    pub fn can_undo(&self) -> bool {
        !self.history.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.history.redo.is_empty()
    }

    pub fn undo(&mut self) {
        self.track_history(false, f64::INFINITY);
        let Some(prev) = self.history.undo.pop() else { return };
        self.history.redo.push(self.state());
        self.restore(prev);
        self.history.committed = Some(self.state());
        self.history.last_mask = 0;
    }

    pub fn redo(&mut self) {
        self.track_history(false, f64::INFINITY);
        let Some(next) = self.history.redo.pop() else { return };
        self.history.undo.push(self.state());
        self.restore(next);
        self.history.committed = Some(self.state());
        self.history.last_mask = 0;
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

    /// Slide region `orig` by `delta` frames, keeping its length and
    /// staying inside the clip. Snaps the in point when snapping is on.
    pub fn move_region(&mut self, orig: (usize, usize), delta: i64) {
        let n = self.frames();
        let len = orig.1.saturating_sub(orig.0).min(n);
        let max_in = (n - len) as i64;
        let a = (orig.0 as i64 + delta).clamp(0, max_in) as usize;
        let mut a2 = self.snap(a);
        if a2 + len > n {
            a2 = a;
        }
        self.sel = (a2, a2 + len);
    }

    /// Where a keyboard step of a marker at `frame` lands: the next zero
    /// crossing when snapping is on, otherwise one screen pixel over.
    fn step_marker(&self, frame: usize, dir: i8) -> Option<usize> {
        if self.prefs.snap_zero {
            return self.next_zero(frame, dir);
        }
        let px = self.view.fpp.round().max(1.0) as isize;
        Some((frame as isize + px * dir as isize).clamp(0, self.frames() as isize) as usize)
    }

    /// Step the in point in `dir`, staying left of the out point.
    pub fn step_in(&mut self, dir: i8) {
        if let Some(f) = self.step_marker(self.sel.0, dir) {
            self.sel.0 = f.min(self.sel.1.saturating_sub(1));
        }
    }

    /// Step the out point in `dir`, staying right of the in point.
    pub fn step_out(&mut self, dir: i8) {
        if let Some(f) = self.step_marker(self.sel.1, dir) {
            self.sel.1 = f.max(self.sel.0 + 1).min(self.frames());
        }
    }

    /// Snap both ends of the selection to their nearest zero crossings.
    pub fn snap_selection(&mut self) {
        let (a, b) = (self.snap(self.sel.0), self.snap(self.sel.1));
        if a < b {
            self.sel = (a, b);
        }
    }

    /// Whether the selection can be doubled without running past the end.
    pub fn can_double_selection(&self) -> bool {
        let (a, b) = self.sel;
        b > a && a + 2 * (b - a) <= self.frames() + Self::double_slack(b - a)
    }

    /// How far a doubled loop may overshoot the file end and still count as
    /// fitting: halving rounds odd lengths and snaps to zero crossings, so
    /// repeated ½ then ×2 drifts a little. 1% of the loop, at least 64 frames.
    fn double_slack(len: usize) -> usize {
        (len / 50).max(64)
    }

    /// Halve (`factor` 0.5) or double (2.0) the selection length, keeping the
    /// in point where it is. The out point snaps to zero if snapping is on.
    pub fn scale_selection(&mut self, factor: f64) {
        let (a, b) = self.sel;
        let len = ((b - a) as f64 * factor).round().max(1.0) as usize;
        let n = self.frames();
        // Doubling to (nearly) the file end lands exactly on the end.
        let out = if factor > 1.0 && (a + len).abs_diff(n) <= Self::double_slack(b - a) {
            n
        } else {
            self.snap((a + len).min(n))
        };
        if out > a {
            self.sel.1 = out;
        }
    }

    /// Whether the selection can move half its length left (-1) or right (+1)
    /// without running past either end of the file.
    pub fn can_shift_selection(&self, dir: i32) -> bool {
        let (a, b) = self.sel;
        let half = (b - a) / 2;
        half > 0 && if dir < 0 { a >= half } else { b + half <= self.frames() }
    }

    /// Move the selection half its length left (-1) or right (+1), keeping its
    /// length exactly (no zero snapping, so loops stay the same size).
    pub fn shift_selection(&mut self, dir: i32) {
        if !self.can_shift_selection(dir) {
            return;
        }
        let (a, b) = self.sel;
        let half = (b - a) / 2;
        self.sel = if dir < 0 { (a - half, b - half) } else { (a + half, b + half) };
    }

    /// Off, or fold this way (listening and export alike).
    pub fn set_mono(&mut self, mono: Option<Fold>) {
        self.prefs.mono = mono;
        if let Some(f) = mono {
            self.prefs.fold = f;
        }
    }

    /// M: mono off, or back on with the last fold picked.
    pub fn toggle_mono(&mut self) {
        let m = if self.prefs.mono.is_some() { None } else { Some(self.prefs.fold) };
        self.set_mono(m);
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

    /// First zero crossing strictly past `frame` in `dir`. A crossing at `f`
    /// sits between frames `f - 1` and `f`, the same convention as a cut point.
    fn next_zero(&self, frame: usize, dir: i8) -> Option<usize> {
        let s = self.sample()?;
        let n = s.frames();
        let mono = |i: usize| -> f32 { s.channels.iter().map(|c| c[i]).sum() };
        let is_cross = |f: usize| f >= 1 && f < n && (mono(f - 1) <= 0.0) != (mono(f) <= 0.0);
        if dir > 0 {
            (frame + 1..n).find(|&f| is_cross(f))
        } else {
            (1..frame.min(n)).rev().find(|&f| is_cross(f))
        }
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

    // ---- cut / copy / paste ------------------------------------------------

    /// Whether the selection is "everything shown", i.e. nothing picked out.
    fn whole_selected(&self) -> bool {
        self.sel == (0, self.frames())
    }

    /// Copy the selection, with gain, to the editor clipboard.
    pub fn copy(&mut self) -> bool {
        let Some(s) = self.sample() else { return false };
        let (a, b) = self.sel;
        let g = self.gain();
        let channels = s.channels.iter().map(|c| c[a..b].iter().map(|x| x * g).collect()).collect();
        self.clip = Some(AudioClip { rate: s.rate, channels });
        true
    }

    /// Whether removing the selection would leave some audio behind.
    pub fn can_remove(&self) -> bool {
        let Some(full) = &self.full else { return false };
        self.current.is_some() && full.sample.frames() > self.sel.1 - self.sel.0
    }

    pub fn cut(&mut self) -> bool {
        if !self.can_remove() || !self.copy() {
            return false;
        }
        self.delete()
    }

    /// Remove the selection without touching the clipboard.
    pub fn delete(&mut self) -> bool {
        if !self.can_remove() {
            return false;
        }
        let (a, b) = self.sel;
        self.splice(a, b, &[]);
        true
    }

    /// The selection as you hear it (gain, and mono when it's on), for a
    /// slice kit. Leaves the clipboard alone.
    pub fn slice(&self) -> Option<AudioClip> {
        let s = self.sample()?;
        let (a, b) = self.sel;
        if b <= a {
            return None;
        }
        let g = self.gain();
        let channels = match self.fold() {
            Some(f) => vec![stats::fold_region(&s.channels, a, b, f).into_iter().map(|x| x * g).collect()],
            None => s.channels.iter().map(|c| c[a..b].iter().map(|x| x * g).collect()).collect(),
        };
        Some(AudioClip { rate: s.rate, channels })
    }

    /// Add `clip` after the end of the whole file (uncropping first), then
    /// show all of it.
    pub fn append(&mut self, clip: &AudioClip) {
        let Some(full) = &self.full else { return };
        let (rate, nch) = (full.sample.rate, full.sample.channels.len());
        self.uncrop();
        let ins = clip.adapt(rate, nch);
        let end = self.frames();
        self.splice(end, end, &ins);
        self.view.fit_pending = true;
    }

    /// Start a file that isn't on disk yet: `clip` at `path`, unsaved.
    pub fn start_new(&mut self, path: PathBuf, clip: &AudioClip, bits: Option<u32>) {
        self.generation += 1; // drop any load still on its way
        self.loading = None;
        self.play_when_loaded = false;
        self.memos.remove(&path);
        let sample = Sample { path, rate: clip.rate, channels: clip.channels.clone(), bits, format: "WAV".into(), file_len: 0 };
        self.install(Loaded::new(sample));
        self.clean_rev = 0; // nothing written yet
    }

    /// Empty the editor, keeping prefs and the clipboard.
    pub fn close(&mut self) {
        let clip = self.clip.take();
        *self = Editor::new(self.ctx.clone(), self.prefs.clone());
        self.clip = clip;
    }

    pub fn can_paste(&self) -> bool {
        self.current.is_some() && self.clip.as_ref().is_some_and(|c| c.frames() > 0)
    }

    /// Insert the clipboard at the playhead, or over the selection if part
    /// of the file is selected. The playhead ends up after the pasted audio,
    /// so pasting again continues from there.
    pub fn paste(&mut self) -> bool {
        if !self.can_paste() {
            return false;
        }
        let (Some(s), Some(clip)) = (self.sample(), &self.clip) else { return false };
        let ins = clip.adapt(s.rate, s.channels.len());
        let (a, b) = if self.whole_selected() {
            let p = self.playhead.min(self.frames());
            (p, p)
        } else {
            self.sel
        };
        self.splice(a, b, &ins);
        true
    }

    /// Replace `[a, b)` of the shown sample with `ins` (one Vec per channel).
    /// The edit goes into the whole file; a crop around it grows or shrinks
    /// with it. Afterwards nothing is selected and the playhead sits at the
    /// end of what went in.
    fn splice(&mut self, a: usize, b: usize, ins: &[Vec<f32>]) {
        let Some(full) = &self.full else { return };
        let off = self.crop_offset();
        let (fa, fb) = (a + off, b + off);
        let n = ins.first().map_or(0, Vec::len);
        let fs = &full.sample;
        let channels: Vec<Vec<f32>> = fs
            .channels
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut v = Vec::with_capacity(c.len() - (fb - fa) + n);
                v.extend_from_slice(&c[..fa]);
                if let Some(x) = ins.get(i) {
                    v.extend_from_slice(x);
                }
                v.extend_from_slice(&c[fb..]);
                v
            })
            .collect();
        let sample = Sample {
            path: fs.path.clone(),
            rate: fs.rate,
            channels,
            bits: fs.bits,
            format: fs.format.clone(),
            file_len: fs.file_len,
        };
        let total = sample.frames();
        let new_full = Loaded::new(sample);
        self.full = Some(new_full.clone());
        self.next_rev += 1;
        self.rev = self.next_rev;
        self.refresh_file_stats();
        self.drag = None;
        self.stats_key = None;
        let caret = fa + n;
        match self.crop {
            Some((ca, cb)) if cb + n > ca + (fb - fa) => {
                let cb = cb + n - (fb - fa);
                self.sel = (ca, cb);
                self.playhead = caret;
                self.apply_crop(ca, cb);
                // apply_crop asks for a refit; keep the view where it is.
                self.view.fit_pending = false;
            }
            _ => {
                // No crop, or the whole crop was cut away.
                if self.crop.take().is_some() {
                    self.view.fit_pending = true;
                }
                self.current = Some(new_full);
                self.sel = (0, total);
                self.playhead = caret.min(total);
            }
        }
    }

    // ---- export -----------------------------------------------------------

    /// Write the edited region to exactly `target` (overwriting it).
    pub fn export(&mut self, target: PathBuf, mono: Option<Fold>) {
        let Some(sample) = self.sample().cloned() else { return };
        if self.exporting {
            return;
        }
        self.exporting = true;
        let r = Render { start: self.sel.0, end: self.sel.1, gain: self.gain(), mono };
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        let rev = self.rev;
        let _ = std::thread::Builder::new().name("export".into()).spawn(move || {
            let result = export::write_wav(&sample, &r, &target);
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
