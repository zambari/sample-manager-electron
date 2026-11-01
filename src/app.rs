//! Application state and the frame loop.
//!
//! Two structural rules this file exists to enforce:
//!
//! 1. `ui()` only reads cached state and emits requests. Every filesystem
//!    read or write goes through [`crate::jobs`] on a worker thread.
//! 2. Worker results are absorbed in [`App::logic`], which eframe calls before
//!    each `ui()` pass *and* while the window is hidden -- so results are
//!    never dropped on the floor.

use crate::audio::engine::{DeviceInfo, Engine, PlayParams};
use crate::editor::{Editor, Outcome};
use crate::fs_model::{self, SortKey};
use crate::jobs::{Ev, Jobs};
use crate::keymap::{self, Action, EdAction, Keymap, Nudge};
use crate::ui::editor_view::{self, EdIntent, PlayView};
use crate::ops::{self, Kind, Plan, Policy};
use crate::pane::{Pane, Role};
use crate::preview::Preview;
use crate::sizes::SizeCache;
use crate::state;
use crate::tab::DirTab;
use crate::ui::dialogs::{self, Decision, MonoChoice};
use crate::ui::file_table::{self, DragPayload, Intent, TableCtx};
use std::path::{Path, PathBuf};

/// A transfer the user has been asked about and not yet answered.
struct PendingPlan {
    plan: Plan,
    /// What each op means once the conflict question is answered.
    what: PlanKind,
}

enum PlanKind {
    /// Plain copy/move of everything in the plan.
    Transfer,
    /// A commit: ops whose source is in `fold` are mono-folded exports, the
    /// rest are plain copies.
    Commit { fold: std::collections::HashSet<PathBuf>, dest_dir: PathBuf },
    /// The editor's single-file export (one op).
    Export { mono: bool },
}

struct ActiveJob {
    id: u64,
    label: String,
    done_bytes: u64,
    total_bytes: u64,
    done_items: usize,
    total_items: usize,
}

pub struct App {
    jobs: Jobs,
    keymap: Keymap,
    sizes: SizeCache,
    preview: Preview,
    source: Pane,
    dest: Pane,
    focused: Role,
    next_tab_id: u64,
    show_hidden: bool,
    pending: Option<PendingPlan>,
    /// Destination of an export waiting on the mono-fold warning.
    mono_pending: Option<PathBuf>,
    /// Paths waiting on the "move to Trash?" confirmation.
    trash_pending: Option<Vec<PathBuf>>,
    active_job: Option<ActiveJob>,
    status: String,
    errors: Vec<String>,
    /// Last state seen by `logic`, to notice changes that need saving.
    last_state: Option<state::AppState>,
    engine: Engine,
    editor: Editor,
    /// The editor column has the keyboard. `focused` then remembers which
    /// file pane had it last, so ↑/↓ in the editor still steps through files.
    editor_focused: bool,
    devices: Vec<DeviceInfo>,
    output_device: Option<String>,
    favourites: Vec<PathBuf>,
    /// Files ever loaded into the editor (persisted).
    opened: std::collections::HashSet<PathBuf>,
    /// `opened` grew since the last change check. Kept out of the per-frame
    /// snapshot comparison because the set can get large.
    opened_dirty: bool,
    /// Files ever committed to a destination (persisted). Tracked like
    /// `opened`, through `committed_dirty`.
    committed: std::collections::HashSet<PathBuf>,
    committed_dirty: bool,
    /// Which file the engine's voice is playing.
    voice_path: Option<PathBuf>,
    /// The exact sample buffer the voice was given. A crop (or undoing one)
    /// swaps the editor's buffer, and the voice has to follow.
    voice_sample: Option<std::sync::Arc<crate::audio::decode::Sample>>,
    /// File-cursor target last seen, to auto-load on change.
    last_cursor: Option<PathBuf>,
    /// A saved editor session was restored: don't let the file cursor's
    /// first landing auto-load something else over it.
    hold_cursor_load: bool,
    last_stream_error: Option<String>,
    /// Source / destination column widths from the last frame, used to keep
    /// the editor column from being squeezed below its minimum.
    side_widths: [f32; 2],
}

impl App {
    pub fn new(ctx: egui::Context, storage: Option<&dyn eframe::Storage>) -> Self {
        let home = dirs_home();
        let saved = state::load(storage);
        let mut next_tab_id = 0;
        let mut mk = |p: PathBuf| {
            next_tab_id += 1;
            DirTab::new(next_tab_id, p)
        };
        let mut restore = |role: Role, ps: Option<&state::PaneState>| -> Pane {
            let mut tabs: Vec<DirTab> = ps
                .map(|ps| ps.tabs.as_slice())
                .unwrap_or_default()
                .iter()
                .filter_map(|t| {
                    let mut tab = mk(state::existing_dir(&t.cwd)?);
                    tab.sort_key = t.sort_key;
                    tab.sort_asc = t.sort_asc;
                    tab.tree = t.tree;
                    tab.show_all = t.show_all;
                    Some(tab)
                })
                .collect();
            if tabs.is_empty() {
                tabs.push(mk(home.clone()));
            }
            let active = ps.map_or(0, |ps| ps.active).min(tabs.len() - 1);
            Pane { role, tabs, active }
        };
        let source = restore(Role::Source, saved.as_ref().map(|s| &s.source));
        let dest = restore(Role::Dest, saved.as_ref().map(|s| &s.dest));

        let output_device = saved.as_ref().and_then(|s| s.output_device.clone());
        let engine = Engine::open(output_device.as_deref());
        let mut editor = Editor::new(ctx.clone(), saved.as_ref().map(|s| s.editor.clone()).unwrap_or_default());
        let session = saved.as_ref().and_then(|s| s.session.clone()).filter(|s| s.path.is_file());
        if let Some(s) = &session {
            editor.restore_session(s);
        }
        let mut errors = Vec::new();
        if let Some(e) = &engine.open_error {
            errors.push(format!("audio output: {e}"));
        }

        let mut app = Self {
            engine,
            editor,
            editor_focused: false,
            devices: Engine::devices(),
            output_device,
            favourites: saved.as_ref().map(|s| s.favourites.clone()).unwrap_or_default(),
            opened: saved.as_ref().map(|s| s.opened.iter().cloned().collect()).unwrap_or_default(),
            opened_dirty: false,
            committed: saved.as_ref().map(|s| s.committed.iter().cloned().collect()).unwrap_or_default(),
            committed_dirty: false,
            voice_path: None,
            voice_sample: None,
            last_cursor: None,
            hold_cursor_load: session.is_some(),
            last_stream_error: None,
            side_widths: [0.0; 2],
            jobs: Jobs::new(ctx),
            keymap: Keymap::platform_default(),
            sizes: SizeCache::default(),
            preview: Preview::default(),
            source,
            dest,
            focused: saved.as_ref().map_or(Role::Source, |s| s.focused),
            next_tab_id,
            show_hidden: saved.as_ref().is_some_and(|s| s.show_hidden),
            pending: None,
            mono_pending: None,
            trash_pending: None,
            active_job: None,
            status: String::new(),
            errors,
            last_state: None,
        };
        app.reload_all();
        app
    }

    /// `with_opened` is false for the per-frame change check, which tracks
    /// `opened` through `opened_dirty` instead.
    fn snapshot(&self, with_opened: bool) -> state::AppState {
        let pane = |p: &Pane| state::PaneState {
            tabs: p
                .tabs
                .iter()
                .map(|t| state::TabState {
                    cwd: t.cwd.clone(),
                    sort_key: t.sort_key,
                    sort_asc: t.sort_asc,
                    tree: t.tree,
                    show_all: t.show_all,
                })
                .collect(),
            active: p.active,
        };
        state::AppState {
            source: pane(&self.source),
            dest: pane(&self.dest),
            focused: self.focused,
            show_hidden: self.show_hidden,
            editor: self.editor.prefs.clone(),
            output_device: self.output_device.clone(),
            favourites: self.favourites.clone(),
            opened: if with_opened {
                let mut v: Vec<PathBuf> = self.opened.iter().cloned().collect();
                v.sort();
                v
            } else {
                Vec::new()
            },
            committed: if with_opened {
                let mut v: Vec<PathBuf> = self.committed.iter().cloned().collect();
                v.sort();
                v
            } else {
                Vec::new()
            },
            session: self.editor.session(),
        }
    }

    fn pane(&self, role: Role) -> &Pane {
        match role {
            Role::Source => &self.source,
            Role::Dest => &self.dest,
        }
    }

    fn pane_mut(&mut self, role: Role) -> &mut Pane {
        match role {
            Role::Source => &mut self.source,
            Role::Dest => &mut self.dest,
        }
    }

    fn new_tab_id(&mut self) -> u64 {
        self.next_tab_id += 1;
        self.next_tab_id
    }

    // ---- requests ---------------------------------------------------------

    fn reload(&mut self, role: Role, tab_idx: usize) {
        let show_hidden = self.show_hidden;
        let pane = self.pane_mut(role);
        if let Some(tab) = pane.tabs.get_mut(tab_idx) {
            let generation = tab.begin_reload();
            let mut requests = vec![(tab.cwd.clone(), generation)];
            // Expanded tree folders are part of what's shown; refresh them too.
            for dir in tab.expanded_dirs() {
                let g = tab.begin_child_load(&dir);
                requests.push((dir, g));
            }
            for (dir, g) in requests {
                self.jobs.list(dir, g, show_hidden);
            }
        }
    }

    /// Expand a folder in the active tab's tree and fetch its contents.
    fn expand(&mut self, role: Role, dir: PathBuf) {
        let show_hidden = self.show_hidden;
        let tab = self.pane_mut(role).active_tab_mut();
        if tab.expand(&dir) {
            let g = tab.begin_child_load(&dir);
            self.jobs.list(dir, g, show_hidden);
        }
    }

    fn reload_all(&mut self) {
        for role in [Role::Source, Role::Dest] {
            for i in 0..self.pane(role).tabs.len() {
                self.reload(role, i);
            }
        }
    }

    /// Re-list any tab showing a directory at or under `dir`.
    fn reload_showing(&mut self, dir: &Path) {
        let mut todo = Vec::new();
        for role in [Role::Source, Role::Dest] {
            for (i, tab) in self.pane(role).tabs.iter().enumerate() {
                if tab.cwd == dir || dir.starts_with(&tab.cwd) {
                    todo.push((role, i));
                }
            }
        }
        for (role, i) in todo {
            self.reload(role, i);
        }
    }

    fn mark_committed(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        for p in paths {
            self.committed_dirty |= self.committed.insert(p);
        }
    }

    fn request_sizes(&mut self, paths: Vec<PathBuf>) {
        for p in paths {
            if p.is_dir() && self.sizes.request(&p) {
                self.jobs.dir_size(p);
            }
        }
    }

    // ---- the core verb ----------------------------------------------------

    /// Commit the source pane's selection into the active destination tab.
    ///
    /// Deliberately directional rather than Commander's "active pane -> other
    /// pane": the panes have fixed roles here. To get the Commander behaviour,
    /// swap the two role arguments for `(self.focused, self.focused.other())`.
    fn commit(&mut self, kind: Kind) {
        let sources = self.pane(Role::Source).active_tab().effective_selection();
        // Nothing selected (e.g. right after launch, with a file restored in
        // the editor): commit the editor's file as you hear it.
        if sources.is_empty() && kind == Kind::Copy && self.editor.current.is_some() {
            self.export();
            return;
        }
        if sources.is_empty() {
            self.status = "Nothing selected in the source pane".to_owned();
            return;
        }
        let dest_dir = self.pane(Role::Dest).active_tab().cwd.clone();
        if kind == Kind::Move {
            self.start_transfer(kind, sources, dest_dir);
            return;
        }

        // A single file that's open in the editor goes out as you hear it:
        // trimmed, with gain, and folded to mono if that's set.
        if let [only] = sources.as_slice() {
            if self.editor.path() == Some(only.as_path())
                && (self.editor.is_edited() || self.editor.exports_mono())
            {
                self.export();
                return;
            }
        }

        // Otherwise trim and gain don't apply; files are copied as they are,
        // except multichannel audio when "save mono" is on, which gets folded.
        let (fold, copy): (Vec<PathBuf>, Vec<PathBuf>) = if self.editor.prefs.save_mono {
            sources.into_iter().partition(|p| {
                p.is_file()
                    && crate::fs_model::is_audio_path(p)
                    && crate::audio::decode::channel_count(p).is_some_and(|n| n > 1)
            })
        } else {
            (Vec::new(), sources)
        };
        // Copies and folds go into one plan so existing files are asked about
        // once, in one popup.
        let mut plan = ops::plan(kind, &copy, &dest_dir, |p| self.sizes.bytes_of(p));
        for src in &fold {
            let dst = crate::audio::export::target_for(src, &dest_dir);
            plan.ops.push(ops::Op {
                conflict: dst.exists(),
                approx_bytes: std::fs::metadata(src).map(|m| m.len()).unwrap_or(0),
                src: src.clone(),
                dst,
                is_dir: false,
            });
        }
        let what = PlanKind::Commit { fold: fold.into_iter().collect(), dest_dir };
        self.offer(plan, what);
    }

    fn start_transfer(&mut self, kind: Kind, sources: Vec<PathBuf>, dest_dir: PathBuf) {
        let plan = ops::plan(kind, &sources, &dest_dir, |p| self.sizes.bytes_of(p));
        self.offer(plan, PlanKind::Transfer);
    }

    /// Run `plan` now, or hold it and ask first if anything already exists.
    fn offer(&mut self, plan: Plan, what: PlanKind) {

        for (path, why) in &plan.rejected {
            self.errors.push(format!("{}: {why}", path.display()));
        }
        if plan.ops.is_empty() {
            self.status = "Nothing to transfer".to_owned();
            return;
        }
        if plan.conflicts() > 0 {
            // Hold the plan and let the modal decide. No bytes move until then.
            self.pending = Some(PendingPlan { plan, what });
            return;
        }
        self.launch(&plan, what, Policy::KeepBoth);
    }

    fn launch(&mut self, plan: &Plan, what: PlanKind, policy: Policy) {
        let mut resolved = ops::resolve(plan, policy);
        if resolved.is_empty() {
            self.status = "All items skipped".to_owned();
            return;
        }
        match what {
            PlanKind::Transfer => {}
            PlanKind::Export { mono } => {
                let op = resolved.remove(0);
                if let Some(p) = self.editor.path().map(Path::to_path_buf) {
                    self.mark_committed([p]);
                }
                self.status = format!("Exporting{} to {}…", if mono { " (mono)" } else { "" }, op.dst.display());
                self.editor.export(op.dst, mono);
                return;
            }
            PlanKind::Commit { fold, dest_dir } => {
                let (folds, copies): (Vec<_>, Vec<_>) = resolved.into_iter().partition(|o| fold.contains(&o.src));
                resolved = copies;
                if !folds.is_empty() {
                    self.mark_committed(folds.iter().map(|o| o.src.clone()));
                    self.status = format!("Folding {} file(s) to mono…", folds.len());
                    self.editor.export_mono_batch(folds.into_iter().map(|o| (o.src, o.dst)).collect(), dest_dir);
                }
                if resolved.is_empty() {
                    return;
                }
            }
        }
        if plan.kind == Kind::Copy {
            self.mark_committed(resolved.iter().map(|o| o.src.clone()));
        }
        let total_items = resolved.len();
        let total_bytes = resolved.iter().map(|o| o.approx_bytes).sum();
        let id = self.jobs.execute(plan.kind, resolved);
        self.active_job = Some(ActiveJob {
            id,
            label: String::new(),
            done_bytes: 0,
            total_bytes,
            done_items: 0,
            total_items,
        });
    }

    // ---- worker results ---------------------------------------------------

    fn absorb(&mut self, events: Vec<Ev>) {
        for ev in events {
            match ev {
                Ev::Listed { generation, path, result } => {
                    for role in [Role::Source, Role::Dest] {
                        for tab in self.pane_mut(role).tabs.iter_mut() {
                            if tab.cwd == path {
                                tab.accept_listing(generation, result.clone());
                            } else if tab.expanded.contains(&path) {
                                tab.accept_child_listing(&path, generation, result.clone());
                            }
                        }
                    }
                }
                Ev::DirSize { path, bytes, files } => {
                    self.sizes.set(path, bytes, files);
                }
                Ev::Progress {
                    job,
                    done_bytes,
                    total_bytes,
                    done_items,
                    total_items,
                    label,
                } => {
                    if let Some(a) = self.active_job.as_mut() {
                        if a.id == job {
                            a.done_bytes = done_bytes;
                            a.total_bytes = total_bytes;
                            a.done_items = done_items;
                            a.total_items = total_items;
                            if !label.is_empty() {
                                a.label = label;
                            }
                        }
                    }
                }
                Ev::Finished {
                    job,
                    touched,
                    errors,
                    cancelled,
                } => {
                    self.jobs.forget(job);
                    if self.active_job.as_ref().is_some_and(|a| a.id == job) {
                        self.active_job = None;
                    }
                    self.status = if cancelled {
                        "Cancelled".to_owned()
                    } else if errors.is_empty() {
                        "Done".to_owned()
                    } else {
                        format!("Finished with {} error(s)", errors.len())
                    };
                    self.errors.extend(errors);
                    for dir in touched {
                        // A transfer changes sizes up the whole chain, so the
                        // cached values above it are now wrong too.
                        self.sizes.invalidate_subtree(&dir);
                        for anc in dir.ancestors() {
                            self.sizes.invalidate_subtree(anc);
                            break;
                        }
                        self.reload_showing(&dir);
                    }
                }
            }
        }
    }

    // ---- keyboard ---------------------------------------------------------

    fn handle_keys(&mut self, ui: &egui::Ui) {
        // An open rename editor or modal owns the keyboard.
        let renaming = self.pane(self.focused).active_tab().renaming.is_some();
        if renaming || self.pending.is_some() || self.mono_pending.is_some() || self.trash_pending.is_some() {
            return;
        }
        if ui.ctx().text_edit_focused() {
            return; // a text field (filter box, typed value) has the keyboard
        }

        if self.editor_focused {
            let actions = ui.input(keymap::editor_actions);
            let wave = self.editor.wave_rect;
            for a in actions {
                self.perform_editor(a, wave);
            }
            return;
        }

        let actions = ui.input(|i| self.keymap.actions(i));
        for action in actions {
            self.perform(action);
        }
    }

    // ---- playback ---------------------------------------------------------

    fn playing_current(&self) -> bool {
        self.engine.is_playing() && self.voice_path.is_some() && self.voice_path.as_deref() == self.editor.path()
    }

    /// Play from wherever the toolbar's "From" setting says.
    fn start_play(&mut self) {
        self.start_play_from(self.editor.prefs.play_from_in);
    }

    /// Play the editor's sample from the in point, or from the parked
    /// playhead (falling back to the in point if the playhead is outside
    /// the region).
    fn start_play_from(&mut self, from_in: bool) {
        let Some(s) = self.editor.sample().cloned() else { return };
        let (a, b) = self.editor.sel;
        let ph = self.editor.playhead;
        let from = if !from_in && (a..b).contains(&ph) { ph } else { a };
        self.voice_path = Some(s.path.clone());
        self.voice_sample = Some(s.clone());
        self.engine.play(
            s,
            PlayParams {
                from,
                loop_start: a,
                end: b,
                looping: self.editor.prefs.looping,
                gain: self.editor.gain(),
                mono: self.editor.mono_active(),
            },
        );
    }

    fn toggle_play(&mut self) {
        if self.playing_current() {
            self.engine.stop();
        } else {
            self.start_play();
        }
    }

    /// Load `path` into the editor, optionally starting playback once it's in.
    fn audition(&mut self, path: &Path, play: bool) {
        if self.editor.path() == Some(path) {
            if play {
                self.start_play();
            }
            return;
        }
        if self.engine.is_playing() {
            self.engine.stop();
        }
        self.editor.play_when_loaded = play;
        self.editor.request(path);
        if self.opened.insert(path.to_path_buf()) {
            self.opened_dirty = true;
        }
    }

    /// Space / double-click on a file row.
    fn preview_toggle(&mut self, path: &Path) {
        if self.editor.path() == Some(path) && self.playing_current() {
            self.engine.stop();
        } else {
            self.audition(path, true);
        }
    }

    /// Auto-load whatever the file cursor is on, when it changes.
    fn follow_cursor(&mut self) {
        let cur = self
            .pane(self.focused)
            .active_tab()
            .cursor_entry()
            .filter(|e| e.is_audio)
            .map(|e| e.path.clone());
        if self.hold_cursor_load {
            // Wait for the restored listing to settle, then adopt its cursor
            // without loading it.
            if !self.pane(self.focused).active_tab().loading {
                self.hold_cursor_load = false;
                self.last_cursor = cur;
            }
            return;
        }
        if cur == self.last_cursor {
            return;
        }
        self.last_cursor = cur.clone();
        if let Some(p) = cur {
            if self.editor.prefs.auto_load {
                self.audition(&p, self.editor.prefs.auto_play);
            }
        }
    }

    /// Step the file cursor to the next audio file in `dir` direction.
    fn step_file(&mut self, dir: isize) {
        let tab = self.pane_mut(self.focused).active_tab_mut();
        for _ in 0..tab.entries.len() {
            let before = tab.cursor;
            tab.move_cursor(dir, false);
            if tab.cursor == before || tab.cursor_entry().is_some_and(|e| e.is_audio) {
                break;
            }
        }
        let path = tab.cursor_entry().filter(|e| e.is_audio).map(|e| e.path.clone());
        if let Some(p) = path {
            // Explicit request: load even when auto-load is off.
            self.last_cursor = Some(p.clone());
            self.audition(&p, self.editor.prefs.auto_play);
        }
    }

    fn export(&mut self) {
        if self.editor.current.is_none() {
            return;
        }
        let dest = self.pane(Role::Dest).active_tab().cwd.clone();
        // Folding to mono without having listened to the fold: ask first.
        if self.editor.exports_mono() && !self.editor.mono_active() {
            self.mono_pending = Some(dest);
            return;
        }
        let mono = self.editor.exports_mono();
        self.export_to(dest, mono);
    }

    fn export_to(&mut self, dest: PathBuf, mono: bool) {
        let Some(src) = self.editor.path().map(Path::to_path_buf) else { return };
        let dst = crate::audio::export::target_for(&src, &dest);
        let op = ops::Op {
            conflict: dst.exists(),
            approx_bytes: 0,
            src,
            dst,
            is_dir: false,
        };
        let plan = Plan { kind: Kind::Copy, ops: vec![op], rejected: Vec::new() };
        self.offer(plan, PlanKind::Export { mono });
    }

    fn perform_editor(&mut self, a: EdAction, wave: Option<egui::Rect>) {
        let ed = &mut self.editor;
        let frames = ed.frames();
        // Points set while playing land where you hear them.
        let here = if self.engine.is_playing() && self.voice_path.as_deref() == ed.path() {
            self.engine.position().min(frames)
        } else {
            ed.playhead
        };
        match a {
            EdAction::TogglePlay => self.toggle_play(),
            EdAction::TogglePlayAlt => {
                if self.playing_current() {
                    self.engine.stop();
                } else {
                    self.start_play_from(!self.editor.prefs.play_from_in);
                }
            }
            EdAction::ToggleFollow => ed.prefs.follow_playback = !ed.prefs.follow_playback,
            EdAction::ToggleSnap => {
                ed.prefs.snap_zero = !ed.prefs.snap_zero;
                if ed.prefs.snap_zero {
                    ed.snap_selection();
                }
            }
            EdAction::InZero(dir) | EdAction::OutZero(dir) => {
                if matches!(a, EdAction::InZero(_)) {
                    ed.step_in(dir);
                } else {
                    ed.step_out(dir);
                }
                let f = if matches!(a, EdAction::InZero(_)) { ed.sel.0 } else { ed.sel.1 };
                if let Some(r) = wave {
                    editor_view::ensure_visible(ed, r.width(), f);
                }
            }
            EdAction::Crop => ed.crop_to_selection(),
            EdAction::Uncrop => ed.uncrop(),
            EdAction::Undo => ed.undo(),
            EdAction::Redo => ed.redo(),
            EdAction::Stop => self.engine.stop(),
            EdAction::SetIn => ed.set_in(here),
            EdAction::SetOut => ed.set_out(here),
            EdAction::ToggleLoop => ed.prefs.looping = !ed.prefs.looping,
            EdAction::ToggleMono => ed.prefs.mono_preview = !ed.prefs.mono_preview,
            EdAction::Nudge(n, dir) => {
                let px = ed.view.fpp.round().max(1.0) as isize;
                let step = match n {
                    Nudge::Pixel => px,
                    Nudge::TenPixels => px * 10,
                    Nudge::Frame => 1,
                };
                ed.playhead = (ed.playhead as isize + step * dir as isize).clamp(0, frames as isize) as usize;
                if let Some(r) = wave {
                    editor_view::ensure_visible(ed, r.width(), ed.playhead);
                }
            }
            EdAction::JumpIn | EdAction::JumpOut => {
                ed.playhead = if a == EdAction::JumpIn { ed.sel.0 } else { ed.sel.1.saturating_sub(1) };
                if let Some(r) = wave {
                    editor_view::ensure_visible(ed, r.width(), ed.playhead);
                }
            }
            EdAction::ZoomIn | EdAction::ZoomOut => {
                if let Some(r) = wave {
                    let factor = if a == EdAction::ZoomIn { 0.5 } else { 2.0 };
                    // Zoom around the playhead when it's on screen.
                    let xp = r.left() + ((ed.playhead as f64 - ed.view.start) / ed.view.fpp) as f32;
                    let anchor = if r.x_range().contains(xp) { xp } else { r.center().x };
                    editor_view::zoom_at(ed, r, anchor, factor);
                }
            }
            EdAction::ZoomFit => ed.view.fit_pending = true,
            EdAction::ZoomRegion => {
                if let Some(r) = wave {
                    let len = (ed.sel.1 - ed.sel.0).max(1) as f64;
                    ed.view.fpp = len * 1.1 / r.width().max(1.0) as f64;
                    ed.view.start = ed.sel.0 as f64 - len * 0.05;
                    editor_view::clamp_view(ed, r.width());
                }
            }
            EdAction::SelectAll => ed.select_all(),
            EdAction::Normalize => ed.normalize(),
            EdAction::Export => self.export(),
            EdAction::NextFile => self.step_file(1),
            EdAction::PrevFile => self.step_file(-1),
            EdAction::SwitchPane => {
                self.editor_focused = false;
                self.focused = Role::Dest;
            }
        }
    }

    fn perform(&mut self, action: Action) {
        let role = self.focused;
        match action {
            Action::Commit => self.commit(Kind::Copy),
            Action::Preview => {
                if let Some(e) = self.pane(role).active_tab().cursor_entry() {
                    if e.is_dir {
                        // Space on a folder has no sample to audition; treat it
                        // as "tell me how big this is" instead of doing nothing.
                        let p = e.path.clone();
                        self.request_sizes(vec![p]);
                    } else if e.is_audio {
                        let p = e.path.clone();
                        self.preview_toggle(&p);
                    }
                }
            }
            Action::Rename => {
                let tab = self.pane_mut(role).active_tab_mut();
                if let Some(e) = tab.cursor_entry() {
                    tab.renaming = Some((e.path.clone(), e.name.clone()));
                }
            }
            Action::OpenDir => {
                if let Some(e) = self.pane(role).active_tab().cursor_entry() {
                    if e.is_dir {
                        let p = e.path.clone();
                        self.open_in(role, p);
                    }
                }
            }
            Action::GoUp => self.go_up(role),
            Action::Expand => {
                let tab = self.pane(role).active_tab();
                let Some(e) = tab.cursor_entry() else { return };
                if !e.is_dir {
                    return;
                }
                let p = e.path.clone();
                if !tab.tree {
                    self.open_in(role, p);
                } else if tab.is_expanded(&p) {
                    // Already open: step onto its first child, if it has one.
                    let tab = self.pane_mut(role).active_tab_mut();
                    let c = tab.cursor.unwrap_or(0);
                    if tab.depths.get(c + 1) > tab.depths.get(c) {
                        tab.set_cursor(c + 1);
                    }
                } else {
                    self.expand(role, p);
                }
            }
            Action::Collapse => {
                let tab = self.pane_mut(role).active_tab_mut();
                if !tab.tree {
                    return self.go_up(role);
                }
                let Some(c) = tab.cursor else { return self.go_up(role) };
                let path = tab.entries[c].path.clone();
                if tab.is_expanded(&path) {
                    tab.collapse(&path);
                } else if let Some(pr) = tab.parent_row(c) {
                    tab.set_cursor(pr);
                } else {
                    self.go_up(role);
                }
            }
            Action::ComputeSize => {
                let sel = self.pane(role).active_tab().effective_selection();
                self.request_sizes(sel);
            }
            Action::Trash => {
                let sel = self.pane(role).active_tab().effective_selection();
                if !sel.is_empty() {
                    self.trash_pending = Some(sel);
                }
            }
            Action::SelectAll => self.pane_mut(role).active_tab_mut().select_all(),
            Action::ToggleSelect => {
                let tab = self.pane_mut(role).active_tab_mut();
                if let Some(c) = tab.cursor {
                    tab.toggle_at(c);
                }
            }
            Action::Refresh => {
                let i = self.pane(role).active;
                self.reload(role, i);
            }
            Action::CursorUp => self.pane_mut(role).active_tab_mut().move_cursor(-1, false),
            Action::CursorDown => self.pane_mut(role).active_tab_mut().move_cursor(1, false),
            Action::ExtendUp => self.pane_mut(role).active_tab_mut().move_cursor(-1, true),
            Action::ExtendDown => self.pane_mut(role).active_tab_mut().move_cursor(1, true),
            Action::NewTab => {
                let cwd = self.pane(role).active_tab().cwd.clone();
                let id = self.new_tab_id();
                self.pane_mut(role).add_tab(DirTab::new(id, cwd));
                let i = self.pane(role).active;
                self.reload(role, i);
            }
            Action::CloseTab => {
                let i = self.pane(role).active;
                self.pane_mut(role).close_tab(i);
            }
            Action::NextTab => self.pane_mut(role).cycle(1),
            Action::PrevTab => self.pane_mut(role).cycle(-1),
            // Tab walks the columns left to right: Source → Editor → Destination.
            Action::SwitchPane => match self.focused {
                Role::Source => self.editor_focused = true,
                Role::Dest => self.focused = Role::Source,
            },
            Action::Cancel => {
                self.engine.stop();
                if let Some(a) = &self.active_job {
                    self.jobs.cancel(a.id);
                }
                self.pane_mut(role).active_tab_mut().renaming = None;
            }
        }
    }

    fn go_up(&mut self, role: Role) {
        if let Some(p) = self.pane(role).active_tab().parent() {
            self.open_in(role, p);
        }
    }

    fn open_in(&mut self, role: Role, path: PathBuf) {
        self.pane_mut(role).active_tab_mut().navigate_to(path);
        let i = self.pane(role).active;
        self.reload(role, i);
    }

    fn do_rename(&mut self, from: &Path, to: &str) {
        let to = to.trim();
        if to.is_empty() || to.contains(std::path::MAIN_SEPARATOR) {
            self.errors.push(format!("invalid name: {to:?}"));
            return;
        }
        let Some(parent) = from.parent() else { return };
        let target = parent.join(to);
        if target == from {
            return;
        }
        if target.exists() {
            // Rename is not a transfer, so there's no plan to resolve -- just
            // refuse rather than silently clobbering a neighbour.
            self.errors
                .push(format!("{} already exists", target.display()));
            return;
        }
        match std::fs::rename(from, &target) {
            Ok(()) => {
                self.sizes.invalidate_subtree(parent);
                self.reload_showing(parent);
            }
            Err(e) => self.errors.push(format!("rename failed: {e}")),
        }
    }
}

// ---- rendering ------------------------------------------------------------

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, state::KEY, &self.snapshot(true));
    }

    fn auto_save_interval(&self) -> std::time::Duration {
        state::SAVE_INTERVAL
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // eframe only autosaves on a frame. After a change, make sure one
        // happens once the interval has passed, even if the app goes idle.
        let snap = self.snapshot(false);
        if self.last_state.as_ref() != Some(&snap) || std::mem::take(&mut self.opened_dirty)
            | std::mem::take(&mut self.committed_dirty)
        {
            self.last_state = Some(snap);
            ctx.request_repaint_after(state::SAVE_INTERVAL + std::time::Duration::from_millis(100));
        }
        // Absorb worker results here rather than in `ui`, so they land even
        // while the window is hidden.
        let events = self.jobs.drain();
        if !events.is_empty() {
            self.absorb(events);
        }
        self.absorb_editor();

        // Edits apply live to the running voice: drag an out point while the
        // loop plays and you hear the seam move.
        if self.playing_current() {
            let swapped = match (&self.voice_sample, self.editor.sample()) {
                (Some(v), Some(e)) => !std::sync::Arc::ptr_eq(v, e),
                _ => false,
            };
            if swapped {
                self.start_play();
            }
            let ed = &self.editor;
            self.engine.update(ed.sel.0, ed.sel.1, ed.prefs.looping, ed.gain(), ed.mono_active());
            ctx.request_repaint();
        }
        self.preview.set(if self.engine.is_playing() { self.voice_path.clone() } else { None });

        if let Some(e) = self.engine.stream_error() {
            if self.last_stream_error.as_ref() != Some(&e) {
                self.errors.push(format!("audio output: {e}"));
                self.last_stream_error = Some(e);
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));

        // Three columns: Source | Sample Editor | Destination.
        // The side columns are drag-resizable; the editor takes whatever is
        // left, but never less than MIN_EDITOR_W.
        const MIN_SIDE_W: f32 = 220.0;
        const MIN_EDITOR_W: f32 = 360.0;
        let w = ui.available_width();
        let default_w = w * 0.27;
        let prev_r = if self.side_widths[1] > 0.0 { self.side_widths[1] } else { default_w };
        let max_for = |other: f32| (w - other - MIN_EDITOR_W).max(MIN_SIDE_W);

        let left = egui::Panel::left("source_col")
            .resizable(true)
            .default_size(default_w)
            .size_range(MIN_SIDE_W..=max_for(prev_r))
            .show(ui, |ui| self.show_pane(ui, Role::Source));
        self.side_widths[0] = left.response.rect.width();
        let right = egui::Panel::right("dest_col")
            .resizable(true)
            .default_size(default_w)
            .size_range(MIN_SIDE_W..=max_for(self.side_widths[0]))
            .show(ui, |ui| self.show_pane(ui, Role::Dest));
        self.side_widths[1] = right.response.rect.width();
        draw_divider(ui, left.response.rect.right(), left.response.rect.y_range());
        draw_divider(ui, right.response.rect.left(), right.response.rect.y_range());

        egui::CentralPanel::default().show(ui, |ui| self.show_editor(ui));

        // Modal last, so it paints over both panes.
        if let Some(p) = &self.pending {
            if let Some(decision) = dialogs::conflicts(ui.ctx(), &p.plan) {
                let pending = self.pending.take().expect("checked above");
                match decision {
                    Decision::Proceed(policy) => self.launch(&pending.plan, pending.what, policy),
                    Decision::Cancel => self.status = "Transfer cancelled".to_owned(),
                }
            }
        }

        if self.mono_pending.is_some() {
            let ph = self.editor.region_phase();
            if let Some(c) = dialogs::mono_export(ui.ctx(), &ph) {
                let dest = self.mono_pending.take().expect("checked above");
                match c {
                    MonoChoice::SaveMono => self.export_to(dest, true),
                    MonoChoice::SaveStereo => self.export_to(dest, false),
                    MonoChoice::Listen => {
                        self.editor.prefs.mono_preview = true;
                        self.status = "Mono preview on — export again when it sounds right".to_owned();
                    }
                    MonoChoice::Cancel => self.status = "Export cancelled".to_owned(),
                }
            }
        }

        if let Some(paths) = &self.trash_pending {
            if let Some(yes) = dialogs::confirm_trash(ui.ctx(), paths) {
                let paths = self.trash_pending.take().expect("checked above");
                if yes {
                    self.jobs.trash(paths);
                } else {
                    self.status = "Delete cancelled".to_owned();
                }
            }
        }

        self.handle_keys(ui);
        self.follow_cursor();

        // Undo steps are recorded by diffing the editor state once a frame,
        // after any drag, slider or typed value has been let go.
        let (busy_ptr, now) = ui.input(|i| (i.pointer.any_down(), i.time));
        let busy = busy_ptr || self.editor.drag.is_some() || ui.ctx().text_edit_focused();
        self.editor.track_history(busy, now);

        // Buttons and toggles take keyboard focus when clicked, after which
        // egui would turn Space/Enter into clicks on them and arrows into
        // focus moves. This app drives everything from its own keymaps, so
        // only text fields get to keep focus.
        let ctx = ui.ctx().clone();
        if !ctx.text_edit_focused() {
            if let Some(id) = ctx.memory(|m| m.focused()) {
                ctx.memory_mut(|m| m.surrender_focus(id));
            }
        }
    }
}

impl App {
    fn absorb_editor(&mut self) {
        for o in self.editor.drain() {
            match o {
                Outcome::Loaded => {
                    if std::mem::take(&mut self.editor.play_when_loaded) {
                        self.start_play();
                    }
                }
                Outcome::Exported(p) => {
                    self.status = format!("Exported {}", p.file_name().unwrap_or_default().to_string_lossy());
                    if let Some(dir) = p.parent() {
                        self.sizes.invalidate_subtree(dir);
                        self.reload_showing(dir);
                    }
                }
                Outcome::BatchExported(dir, n) => {
                    self.status = format!("Folded {n} file(s) to mono");
                    self.sizes.invalidate_subtree(&dir);
                    self.reload_showing(&dir);
                }
                Outcome::Error(e) => self.errors.push(e),
            }
        }
    }

    fn show_editor(&mut self, ui: &mut egui::Ui) {
        let pv = PlayView {
            playing: self.playing_current(),
            pos: self.engine.position(),
        };
        let can_export = !self.pane(Role::Dest).active_tab().loading;
        let intents = editor_view::show(ui, &mut self.editor, pv, self.editor_focused, can_export);
        for i in intents {
            match i {
                EdIntent::TookFocus => self.editor_focused = true,
                EdIntent::TogglePlay => {
                    self.editor_focused = true;
                    self.toggle_play();
                }
                EdIntent::Seek(f) => self.engine.seek(f),
                EdIntent::Export => self.export(),
            }
        }
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.label(egui::RichText::new(keymap::EDITOR_HELP).weak().small());
        });
    }

    fn device_picker(&mut self, ui: &mut egui::Ui) {
        let mut choice: Option<Option<String>> = None;
        let name = if self.engine.device_name.is_empty() { "no output".to_owned() } else { self.engine.device_name.clone() };
        if ui.small_button("⟳").on_hover_text("Rescan audio devices").clicked() {
            self.devices = Engine::devices();
        }
        egui::ComboBox::from_id_salt("output_device")
            .selected_text(format!("🔈 {name}"))
            .width(220.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(self.output_device.is_none(), "System default").clicked() {
                    choice = Some(None);
                }
                for d in &self.devices {
                    let on = self.output_device.as_deref() == Some(d.id.as_str());
                    if ui.selectable_label(on, &d.name).on_hover_text(&d.id).clicked() {
                        choice = Some(Some(d.id.clone()));
                    }
                }
            });
        if self.engine.device_rate > 0 {
            ui.label(
                egui::RichText::new(format!("{:.1} kHz · {} ch", self.engine.device_rate as f64 / 1000.0, self.engine.device_channels))
                    .weak()
                    .small(),
            );
        }
        if let Some(dev) = choice {
            self.engine.switch(dev.as_deref());
            self.output_device = dev;
            self.last_stream_error = None;
            if let Some(e) = &self.engine.open_error {
                self.errors.push(format!("audio output: {e}"));
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Sample Manager");
            ui.separator();

            if ui.checkbox(&mut self.show_hidden, "Hidden").changed() {
                self.reload_all();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Right to left: the first widget lands at the far right.
                let commit_key = self.keymap.describe(Action::Commit);
                if ui
                    .button(format!("Commit to destination  {commit_key}"))
                    .on_hover_text(
                        "Copy the source selection into the active destination tab.\n\
                         A single file open in the editor is written with its trim, gain and mono settings.",
                    )
                    .clicked()
                {
                    self.commit(Kind::Copy);
                }
                if ui.button("Move").clicked() {
                    self.commit(Kind::Move);
                }
                ui.separator();
                ui.label(
                    egui::RichText::new(format!("keys: {}", self.keymap.name))
                        .weak()
                        .small(),
                );
                ui.separator();
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    self.device_picker(ui);
                });
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        if let Some(a) = &self.active_job {
            let frac = if a.total_bytes > 0 {
                a.done_bytes as f32 / a.total_bytes as f32
            } else if a.total_items > 0 {
                a.done_items as f32 / a.total_items as f32
            } else {
                0.0
            };
            let job_id = a.id;
            let text = format!(
                "{}/{} · {} / {} · {}",
                a.done_items + 1,
                a.total_items,
                fs_model::human_bytes(a.done_bytes),
                fs_model::human_bytes(a.total_bytes),
                a.label
            );
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(frac.clamp(0.0, 1.0))
                        .desired_width(240.0)
                        .text(text),
                );
                if ui.button("Cancel").clicked() {
                    self.jobs.cancel(job_id);
                }
            });
            return;
        }

        ui.horizontal(|ui| {
            let tab = self.pane(self.focused).active_tab();
            let n_sel = tab.selected.len();
            let sel_bytes: u64 = tab
                .entries
                .iter()
                .filter(|e| tab.selected.contains(&e.path))
                .map(|e| if e.is_dir { 0 } else { e.len })
                .sum();
            ui.label(format!(
                "{} · {} items · {n_sel} selected ({})",
                if self.editor_focused { "Editor" } else { self.focused.label() },
                tab.entries.len(),
                fs_model::human_bytes(sel_bytes)
            ));

            if let Some(p) = self.preview.playing() {
                ui.separator();
                ui.label(format!("▶ {}", p.file_name().unwrap_or_default().to_string_lossy()));
            }

            if !self.status.is_empty() {
                ui.separator();
                ui.label(egui::RichText::new(&self.status).weak());
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(last) = self.errors.last() {
                    if ui
                        .button(
                            egui::RichText::new(format!("⚠ {}", truncate(last, 60)))
                                .color(ui.visuals().error_fg_color),
                        )
                        .on_hover_text("click to clear")
                        .clicked()
                    {
                        self.errors.clear();
                    }
                }
            });
        });
    }

    fn show_pane(&mut self, ui: &mut egui::Ui, role: Role) {
        let focused = self.focused == role && !self.editor_focused;

        // Pane header: role, path, and the sub-tab strip.
        ui.horizontal(|ui| {
            let title = egui::RichText::new(role.label());
            let title = if focused { title.strong() } else { title.weak() };
            if ui.selectable_label(focused, title).clicked() {
                self.focused = role;
                self.editor_focused = false;
            }
            self.favourites_menu(ui, role);
            ui.separator();
            let cwd = self.pane(role).active_tab().cwd.display().to_string();
            ui.add(egui::Label::new(egui::RichText::new(cwd).weak().small()).truncate());
        });

        self.tab_strip(ui, role);
        ui.separator();

        let intents = {
            let sizes = &self.sizes;
            let preview = &self.preview;
            let opened = &self.opened;
            let committed = &self.committed;
            // Borrow the pane field directly rather than via `pane_mut`, so
            // the borrow checker can see it is disjoint from `sizes`/`preview`.
            let pane = match role {
                Role::Source => &mut self.source,
                Role::Dest => &mut self.dest,
            };
            let tab = pane.active_tab_mut();
            file_table::show(
                ui,
                tab,
                TableCtx {
                    sizes,
                    preview,
                    opened,
                    committed,
                    pane_focused: focused,
                },
            )
        };

        for intent in intents {
            match intent {
                Intent::TookFocus => {
                    self.focused = role;
                    self.editor_focused = false;
                }
                Intent::Open(p) => {
                    if p.is_dir() {
                        self.open_in(role, p);
                    } else {
                        self.preview_toggle(&p);
                    }
                }
                Intent::BeginRename(p) => {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.pane_mut(role).active_tab_mut().renaming = Some((p, name));
                }
                Intent::CommitRename { from, to } => {
                    self.pane_mut(role).active_tab_mut().renaming = None;
                    self.do_rename(&from, &to);
                }
                Intent::CancelRename => {
                    self.pane_mut(role).active_tab_mut().renaming = None;
                }
                Intent::WantSize(paths) => self.request_sizes(paths),
                Intent::GoUp => self.go_up(role),
                Intent::Expand(p) => self.expand(role, p),
                Intent::AddFavourite(p) => self.add_favourite(p),
                Intent::Trash(paths) => {
                    if !paths.is_empty() {
                        self.trash_pending = Some(paths);
                    }
                }
                Intent::Dropped(paths) => {
                    let dest = self.pane(role).active_tab().cwd.clone();
                    self.start_transfer(Kind::Copy, paths, dest);
                }
            }
        }
    }

    fn add_favourite(&mut self, dir: PathBuf) {
        if !self.favourites.contains(&dir) {
            self.favourites.push(dir);
        }
    }

    /// Star dropdown: jump the pane's active tab to a bookmarked folder, or
    /// bookmark the one it's showing.
    fn favourites_menu(&mut self, ui: &mut egui::Ui, role: Role) {
        let cwd = self.pane(role).active_tab().cwd.clone();
        let is_fav = self.favourites.contains(&cwd);
        let mut go = None;
        let mut remove = None;
        let mut toggle_current = false;

        let star = if is_fav { "★ ⏷" } else { "☆ ⏷" };
        ui.menu_button(star, |ui| {
            ui.set_min_width(240.0);
            if self.favourites.is_empty() {
                ui.label(egui::RichText::new("No favourites yet").weak());
            }
            for (i, fav) in self.favourites.iter().enumerate() {
                let name = fav
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| fav.display().to_string());
                // An unplugged drive's folder stays listed but can't be opened.
                let exists = fav.is_dir();
                ui.horizontal(|ui| {
                    if ui.small_button("×").on_hover_text("Remove from favourites").clicked() {
                        remove = Some(i);
                    }
                    let resp = ui
                        .add_enabled(exists, egui::Button::new(name).selected(*fav == cwd).frame(false))
                        .on_hover_text(fav.display().to_string())
                        .on_disabled_hover_text(format!("{} (not found)", fav.display()));
                    if resp.clicked() {
                        go = Some(fav.clone());
                        ui.close();
                    }
                });
            }
            ui.separator();
            let label = if is_fav { "Remove this folder" } else { "Add this folder" };
            if ui.button(label).clicked() {
                toggle_current = true;
                ui.close();
            }
        })
        .response
        .on_hover_text("Favourite folders");

        if let Some(i) = remove {
            self.favourites.remove(i);
        }
        if toggle_current {
            if is_fav {
                self.favourites.retain(|f| *f != cwd);
            } else {
                self.add_favourite(cwd);
            }
        }
        if let Some(p) = go {
            self.focused = role;
            self.editor_focused = false;
            self.open_in(role, p);
        }
    }

    /// The sub-tab strip. Each tab header is also a drop target, so you can
    /// drag onto a folder you have open without first switching to it.
    fn tab_strip(&mut self, ui: &mut egui::Ui, role: Role) {
        let mut switch_to = None;
        let mut close = None;
        let mut new_tab = false;
        let mut drop_on: Option<(usize, Vec<PathBuf>)> = None;

        egui::ScrollArea::horizontal()
            .id_salt(("tabs", role.label()))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let active = self.pane(role).active;
                    let titles: Vec<(usize, String, bool)> = self
                        .pane(role)
                        .tabs
                        .iter()
                        .enumerate()
                        .map(|(i, t)| (i, t.title(), t.loading))
                        .collect();

                    for (i, title, loading) in titles {
                        let label = if loading {
                            format!("{title} …")
                        } else {
                            title
                        };
                        let (inner, payload) =
                            ui.dnd_drop_zone::<DragPayload, _>(egui::Frame::NONE, |ui| {
                                ui.selectable_label(i == active, label)
                            });
                        if inner.inner.clicked() {
                            switch_to = Some(i);
                        }
                        if inner.inner.middle_clicked() {
                            close = Some(i);
                        }
                        if let Some(p) = payload {
                            drop_on = Some((i, p.paths.clone()));
                        }
                    }

                    if ui.small_button("+").on_hover_text("New tab").clicked() {
                        new_tab = true;
                    }
                    if self.pane(role).tabs.len() > 1
                        && ui.small_button("×").on_hover_text("Close tab").clicked()
                    {
                        close = Some(self.pane(role).active);
                    }
                });
            });

        if let Some(i) = switch_to {
            self.pane_mut(role).active = i;
            self.focused = role;
            self.editor_focused = false;
        }
        if let Some((i, paths)) = drop_on {
            if let Some(tab) = self.pane(role).tabs.get(i) {
                let dest = tab.cwd.clone();
                self.start_transfer(Kind::Copy, paths, dest);
            }
        }
        if new_tab {
            let cwd = self.pane(role).active_tab().cwd.clone();
            let id = self.new_tab_id();
            self.pane_mut(role).add_tab(DirTab::new(id, cwd));
            let i = self.pane(role).active;
            self.reload(role, i);
        }
        if let Some(i) = close {
            self.pane_mut(role).close_tab(i);
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

// Keep `SortKey` reachable from here for future column config.
const _: Option<SortKey> = None;

/// A visible column divider with a small grip in the middle, so the
/// draggable panel edge is easy to find even when not hovered.
fn draw_divider(ui: &egui::Ui, x: f32, ys: egui::Rangef) {
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::unique("column_dividers"),
    ));
    let visuals = &ui.style().visuals;
    let line = egui::Stroke::new(1.0, visuals.widgets.inactive.bg_stroke.color.gamma_multiply(1.6));
    painter.vline(x, ys, line);

    // Grip: three dots centred on the divider.
    let cy = ys.center();
    let dot = visuals.widgets.inactive.fg_stroke.color;
    for dy in [-8.0, 0.0, 8.0] {
        painter.circle_filled(egui::pos2(x, cy + dy), 1.8, dot);
    }
}
