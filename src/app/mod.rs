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
use crate::device::{self, Device};
use crate::editor::{Editor, Outcome};
use crate::fs_model::{self, SortKey};
use crate::jobs::{Ev, Jobs};
use crate::keymap::{self, Action, EdAction, Keymap, Nudge};
use crate::ui::editor_view::{self, EdIntent, PlayView};
use crate::ops::{self, Clipboard, Kind, Plan, Policy};
use crate::pane::{Pane, Role};
use crate::preview::Preview;
use crate::sizes::SizeCache;
use crate::audio::stats::Fold;
use crate::state;
use crate::tab::DirTab;
use crate::ui::dialogs::{self, Decision, LeaveChoice};
use crate::ui::theme;
use crate::ui::file_table::{self, DragPayload, Intent, TableCtx};
use std::path::{Path, PathBuf};

mod actions;
mod editing;
mod panels;
mod transfers;
mod undo;

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
    Export { mono: Option<Fold> },
    /// Anything touching the device. Op destinations are only used to ask
    /// about conflicts; the device worker settles the final names.
    Device { dir: device::Dir, mono: Option<Fold> },
    /// The editor's export, rendered locally and then uploaded (one op).
    DeviceExport { mono: Option<Fold> },
}

/// What was about to happen when the editor's unsaved audio edits got in
/// the way.
#[derive(Clone)]
enum Leave {
    /// Load another file (`audition` arguments).
    Audition(PathBuf, bool),
    /// `load_into_editor`, e.g. a fresh download from the device.
    LoadEditor(PathBuf),
    Quit,
}

/// An editor export rendered to a temp file, waiting to be uploaded.
struct StagedUpload {
    staged: PathBuf,
    dest_dir: PathBuf,
    policy: Policy,
}

struct ActiveJob {
    id: u64,
    label: String,
    done_bytes: u64,
    total_bytes: u64,
    done_items: usize,
    total_items: usize,
    /// Has reported progress, so it is running rather than waiting its turn
    /// (device jobs run one after another).
    started: bool,
    /// The file on the wire: `(phase, bytes done, file size)`.
    file: Option<(&'static str, u64, u64)>,
    /// Has reported a file on the wire, so the status bar keeps a slot for
    /// the per-file bar between files instead of letting the main bar jump.
    file_bar: bool,
}

/// Jobs already finished since the queue was last empty, so the status bar
/// can show the whole queue as one run instead of restarting per job.
#[derive(Default)]
struct Batch {
    done_items: usize,
    total_items: usize,
    done_bytes: u64,
    total_bytes: u64,
    /// Some finished job never knew its size: the bar counts files instead.
    bytes_unknown: bool,
    /// Some finished job showed a per-file bar.
    file_bar: bool,
}

pub struct App {
    jobs: Jobs,
    device: Device,
    /// Last connection news from the device worker.
    device_status: Option<Result<String, String>>,
    staged_upload: Option<StagedUpload>,
    keymap: Keymap,
    sizes: SizeCache,
    preview: Preview,
    source: Pane,
    dest: Pane,
    focused: Role,
    next_tab_id: u64,
    show_hidden: bool,
    pending: Option<PendingPlan>,
    /// Digitakt paths waiting on the "delete now?" confirmation.
    delete_pending: Option<Vec<PathBuf>>,
    /// Duplicate groups waiting on the "trash duplicates?" confirmation.
    dupes_pending: Option<Vec<crate::jobs::DupGroup>>,
    /// Paths cut or copied in a file pane, waiting for Paste.
    clipboard: Option<Clipboard>,
    /// File operations ⌘Z can take back, newest last.
    undo: Vec<undo::FileUndo>,
    /// Running jobs whose result becomes an undo step.
    undo_jobs: std::collections::HashMap<u64, undo::UndoJob>,
    /// The pane whose header path is being typed into, and the text so far.
    path_edit: Option<(Role, String)>,
    /// Mounted disks for the drives menu, and when they were last probed.
    volumes: Option<(std::time::Instant, Vec<crate::fs_model::Volume>)>,
    /// Every job not yet finished, oldest first.
    active_jobs: Vec<ActiveJob>,
    /// Finished part of the current queue.
    batch: Batch,
    status: String,
    errors: Vec<String>,
    /// How many of `errors` are already written to the log.
    errors_logged: usize,
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
    /// Master output volume. Session only, never saved.
    volume: f32,
    /// Level meter: shown level and peak-hold mark per side (linear).
    meter: [f32; 2],
    meter_hold: [f32; 2],
    /// The window had focus last frame; regaining it re-lists local folders.
    window_focused: bool,
    /// Device file waiting on the "load to editor?" confirmation, with its size.
    device_load_pending: Option<(PathBuf, u64)>,
    /// Download job fetching a device file for the editor, and where it lands.
    device_load: Option<(u64, PathBuf)>,
    /// Waiting on the "unsaved edits" question.
    leave_pending: Option<Leave>,
    /// Chose to save first: carry on with this once the export lands.
    leave_after_save: Option<Leave>,
    /// The user okayed closing the window with edits unsaved (or saved them).
    allow_close: bool,
    /// Close once the running job (an upload of the saved edit) finishes.
    close_when_idle: bool,
}

impl App {
    pub fn new(ctx: egui::Context, storage: Option<&dyn eframe::Storage>) -> Self {
        theme::apply(&ctx);
        let home = dirs_home();
        let saved = state::load(storage);
        let mut next_tab_id = 0;
        let mut mk = |p: PathBuf, device: bool| {
            next_tab_id += 1;
            if device { DirTab::new_device(next_tab_id, p) } else { DirTab::new(next_tab_id, p) }
        };
        let mut restore = |role: Role, ps: Option<&state::PaneState>| -> Pane {
            let mut tabs: Vec<DirTab> = ps
                .map(|ps| ps.tabs.as_slice())
                .unwrap_or_default()
                .iter()
                .filter_map(|t| {
                    let mut tab = if t.device {
                        mk(t.cwd.clone(), true)
                    } else {
                        mk(state::existing_dir(&t.cwd)?, false)
                    };
                    tab.sort_key = t.sort_key;
                    tab.sort_asc = t.sort_asc;
                    tab.tree = t.tree;
                    tab.show_all = t.show_all;
                    tab.shorten = t.shorten;
                    Some(tab)
                })
                .collect();
            if tabs.is_empty() {
                tabs.push(mk(home.clone(), false));
            }
            let active = ps.map_or(0, |ps| ps.active).min(tabs.len() - 1);
            Pane { role, tabs, active, last_local: None }
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

        let jobs = Jobs::new(ctx);
        let device = Device::new(jobs.emitter());
        let mut app = Self {
            device,
            device_status: None,
            staged_upload: None,
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
            volume: 1.0,
            meter: [0.0; 2],
            meter_hold: [0.0; 2],
            window_focused: true,
            device_load_pending: None,
            device_load: None,
            leave_pending: None,
            leave_after_save: None,
            allow_close: false,
            close_when_idle: false,
            jobs,
            keymap: Keymap::platform_default(),
            sizes: SizeCache::default(),
            preview: Preview::default(),
            source,
            dest,
            focused: saved.as_ref().map_or(Role::Source, |s| s.focused),
            next_tab_id,
            show_hidden: saved.as_ref().is_some_and(|s| s.show_hidden),
            pending: None,
            delete_pending: None,
            clipboard: None,
            undo: Vec::new(),
            undo_jobs: Default::default(),
            path_edit: None,
            volumes: None,
            dupes_pending: None,
            active_jobs: Vec::new(),
            batch: Batch::default(),
            status: String::new(),
            errors,
            errors_logged: 0,
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
                    shorten: t.shorten,
                    device: t.device,
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
        for role in [Role::Source, Role::Dest] {
            self.pane_mut(role).note_active();
        }
        let events = self.jobs.drain();
        if !events.is_empty() {
            self.absorb(events);
        }

        // Back from another app: files may have changed under us.
        let focused = ctx.input(|i| i.focused);
        if focused && !self.window_focused {
            self.reload_local();
        }
        self.window_focused = focused;
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
            self.engine.update(ed.sel.0, ed.sel.1, ed.prefs.looping, ed.gain(), ed.fold());
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

        if let Some((path, bytes)) = &self.device_load_pending {
            if let Some(yes) = dialogs::confirm_device_load(ui.ctx(), path, *bytes) {
                let (path, bytes) = self.device_load_pending.take().expect("checked above");
                if yes {
                    self.start_device_load(path, bytes);
                } else {
                    self.status = "Load cancelled".to_owned();
                }
            }
        }

        if let Some(groups) = &self.dupes_pending {
            if let Some(yes) = dialogs::confirm_duplicates(ui.ctx(), groups) {
                let groups = self.dupes_pending.take().expect("checked above");
                if yes {
                    let paths: Vec<PathBuf> = groups.into_iter().flat_map(|g| g.remove).collect();
                    self.status = format!("Moving {} duplicate(s) to Trash…", paths.len());
                    self.trash_local(paths);
                } else {
                    self.status = "Duplicate removal cancelled".to_owned();
                }
            }
        }

        if let Some(paths) = &self.delete_pending {
            if let Some(yes) = dialogs::confirm_trash(ui.ctx(), paths, true, true) {
                let paths = self.delete_pending.take().expect("checked above");
                if yes {
                    let n = paths.len();
                    self.run_device(device::Task::Delete { paths }, "Deleting", n, 0);
                } else {
                    self.status = "Delete cancelled".to_owned();
                }
            }
        }

        // Closing the window with unsaved audio edits: ask first.
        let close = ui.ctx().input(|i| i.viewport().close_requested());
        if close && self.editor.is_dirty() && !self.allow_close {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.leave_pending.is_none() && self.leave_after_save.is_none() {
                self.leave_pending = Some(Leave::Quit);
            }
        }

        if let Some(leave) = self.leave_pending.clone() {
            let name = self.editor.path().and_then(|p| p.file_name()).unwrap_or_default().to_string_lossy().into_owned();
            let tab = self.pane(Role::Dest).active_tab();
            let dest = (!tab.loading).then(|| {
                let d = tab.cwd.file_name().map_or_else(|| tab.cwd.display().to_string(), |n| n.to_string_lossy().into_owned());
                if tab.device { format!("{d} on the device") } else { d }
            });
            let quitting = matches!(leave, Leave::Quit);
            if let Some(c) = dialogs::leave_editor(ui.ctx(), &name, quitting, dest.as_deref()) {
                self.leave_pending = None;
                self.answer_leave(leave, c);
            }
        }

        // A save chosen in that dialog that never got going (its conflict or
        // mono question was cancelled, or everything was skipped): drop it.
        if self.leave_after_save.is_some()
            && !self.editor.exporting
            && self.pending.is_none()
            && self.staged_upload.is_none()
        {
            self.leave_after_save = None;
        }

        if self.close_when_idle && self.active_jobs.is_empty() {
            self.close_when_idle = false;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
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

/// Show `path` selected in the OS file manager.
fn reveal(path: &Path) {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg("-R").arg(path);
        c
    } else if cfg!(target_os = "windows") {
        let mut c = std::process::Command::new("explorer");
        c.arg(format!("/select,{}", path.display()));
        c
    } else {
        // No portable "select this file"; open the folder that holds it.
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path.parent().unwrap_or(path));
        c
    };
    let _ = cmd.spawn();
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

pub(super) fn dirs_home() -> PathBuf {
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
