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
use crate::audio::stats::Fold;
use crate::device::{self, Device};
use crate::editor::{Editor, Outcome};
use crate::fs_model::{self, SortKey};
use crate::jobs::{Ev, Jobs};
use crate::keymap::{self, Action, EdAction, Keymap, Nudge};
use crate::ops::{self, Clipboard, Kind, Plan, Policy};
use crate::pane::{Pane, Role};
use crate::preview::Preview;
use crate::sizes::SizeCache;
use crate::state;
use crate::tab::DirTab;
use crate::ui::dialogs::{self, Decision, LeaveChoice, SaveScope};
use crate::ui::editor_view::{self, EdIntent, PlayView};
use crate::ui::file_table::{self, DragPayload, Intent, TableCtx};
use crate::ui::theme;
use std::path::{Path, PathBuf};

mod actions;
mod bars;
mod editing;
mod elek;
use editing::ExportNaming;
mod kit;
mod layout;
mod nav;
mod os_dnd;
mod pane_view;
mod pending_dialogs;
mod pool_edit;
mod prints;
mod relaunch;
mod startup;
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
    Commit {
        fold: std::collections::HashSet<PathBuf>,
        dest_dir: PathBuf,
    },
    /// Anything touching the device. Op destinations are only used to ask
    /// about conflicts; the device worker settles the final names.
    Device {
        dir: device::Dir,
        mono: Option<Fold>,
    },
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
    /// The kit's [x]: close it once saved or discarded.
    CloseKit,
    Quit,
}

/// An editor export rendered to a temp file, waiting to be uploaded.
/// A local copy to rename after its device file was renamed.
/// Pool slots dragged onto a local pane where some samples already have a
/// local copy: waiting on "download anyway" or "show the local file".
struct SlotDownload {
    /// The pane they were dropped on, and the folder it showed.
    role: Role,
    dest: PathBuf,
    /// Every dragged sample's +Drive path.
    paths: Vec<PathBuf>,
    /// The ones we already have: `(+Drive path, local copy)`.
    have: Vec<(PathBuf, PathBuf)>,
    /// Slot label (`060`, `A060`) of every dragged sample, by +Drive path.
    labels: std::collections::HashMap<PathBuf, String>,
}

/// Several pool samples about to be downloaded: waiting on whether to put
/// their slot numbers in front of the file names.
struct SlotPrefix {
    dest: PathBuf,
    /// The +Drive paths to download.
    paths: Vec<PathBuf>,
    /// Slot label of each, by +Drive path.
    labels: std::collections::HashMap<PathBuf, String>,
}

struct LocalRename {
    /// The link as it was; `local` is the file to rename.
    link: crate::links::Link,
    /// The new local path.
    to: PathBuf,
}

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
    /// When the first job of this run started moving bytes, for the ETA.
    started_at: Option<std::time::Instant>,
}

pub struct App {
    os_dnd: os_dnd::OsDnd,
    jobs: Jobs,
    device: Device,
    /// Last connection news from the device worker.
    device_status: Option<Result<String, String>>,
    /// The device presence seen last frame, to act when the cable goes in or out.
    last_presence: crate::device::Presence,
    /// Port name of the unit last seen alone on USB: tells a different
    /// machine (a Digitakt II after a Digitakt) from the same one coming back.
    last_unit: Option<String>,
    /// The device subpage last shown (a centre page, or the +Drive on
    /// the right), reopened when the device comes back.
    device_last: Option<crate::ui::elek_view::DevicePick>,
    staged_upload: Option<StagedUpload>,
    /// A pool sample in the editor, saved back into its slot.
    pool_edit: Option<pool_edit::PoolEdit>,
    /// The editor's sample on its way into an empty pool slot.
    slot_save: Option<pool_edit::SlotSave>,
    /// A save that overwrites something other than the open file, waiting
    /// on the user's yes.
    save_confirm: Option<pool_edit::SaveConfirm>,
    keymap: Keymap,
    sizes: SizeCache,
    preview: Preview,
    source: Pane,
    dest: Pane,
    focused: Role,
    next_tab_id: u64,
    pending: Option<PendingPlan>,
    /// A local job that made "Keep both" copies: once it finishes, select
    /// them and (for just one) open the inline rename on it.
    rename_after: Option<(u64, Vec<PathBuf>)>,
    /// A pool sample's +Drive rename: `(job, key, new path)`, shown in the
    /// pool once the device has done it.
    pool_rename: Option<(u64, (u32, u32), String)>,
    /// A +Drive rename of a file that may have a local copy: `(job, (hash,
    /// size), new device path)`. Once it lands, offer to rename the local file.
    relink_after: Option<(u64, (u32, u32), PathBuf)>,
    /// The local copy's rename waiting on the user's yes.
    local_rename_pending: Option<LocalRename>,
    /// Pool slots dropped on a local pane, some already downloaded.
    slot_download_pending: Option<SlotDownload>,
    slot_prefix_pending: Option<SlotPrefix>,
    /// An export's upload job, and the device file to select once it lands.
    reveal_after: Option<(u64, PathBuf)>,
    /// The file an export (not a save in place) is writing: once written,
    /// it opens in the inline rename.
    export_rename: Option<PathBuf>,
    /// A local job's summary to show instead of a plain "Done".
    finish_note: Option<(u64, String)>,
    /// Device transfers queued or running: their direction and `(source,
    /// destination folder)` items, so committing the same files again
    /// doesn't queue them twice.
    device_queued: std::collections::HashMap<u64, (device::Dir, Vec<(PathBuf, PathBuf)>)>,
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
    /// The editor slot on screen: the sample being browsed, or the slice kit
    /// when `showing_kit`.
    editor: Editor,
    /// The hidden editor slot (see [`kit`]).
    other: Editor,
    showing_kit: bool,
    /// Other open kits, waiting to be switched to (see [`kit`]).
    parked: Vec<Editor>,
    /// Kit opened by browsing onto a sliced "[N] name" file. Browsing on
    /// closes it again while it has no edits, so kits don't pile up.
    browsed_kit: Option<kit::BrowsedKit>,
    /// Slice to add once the kit file finishes loading.
    /// For the kit file on its way in: the slice to add (none when a file
    /// was just made the kit) and the grid to put it on.
    kit_pending: Option<(Option<crate::editor::AudioClip>, Option<usize>)>,
    /// A plain sample on its way in as a kit's start, and the new (unsaved)
    /// file the kit takes once it loads: `(sample, kit)`.
    kit_seed: Option<(PathBuf, PathBuf)>,
    /// Grid picked last time, offered for the next new kit.
    kit_grid: Option<usize>,
    /// The last K, so ⌘Z away from the kit can take it back out.
    kit_add: Option<kit::KitAdd>,
    /// Waiting on the "name the slice kit" question.
    kit_naming: Option<kit::KitNaming>,
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
    /// The voice plays its region once, whatever the Loop toggle says: a
    /// slot previewed from the device pages.
    voice_once: bool,
    /// The next play of this file is such a one-shot preview (set when the
    /// click comes in, used once the file has loaded).
    once_for: Option<PathBuf>,
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
    /// Pitch fader, as a fraction (+0.05 = 5% faster). Resets when a
    /// different file loads.
    pitch: f32,
    /// File the pitch was last reset for, so reloading the same file keeps it.
    pitch_path: Option<PathBuf>,
    /// Level meter: shown level and peak-hold mark per side (linear).
    meter: [f32; 2],
    meter_hold: [f32; 2],
    /// The window had focus last frame; regaining it re-lists local folders.
    window_focused: bool,
    /// Device file waiting on the "load to editor?" confirmation, with its size.
    /// Show the "factory samples can't be copied" popup.
    factory_popup: bool,
    /// Already shown during this run of queued jobs.
    factory_warned: bool,
    device_load_pending: Option<(PathBuf, u64)>,
    /// Download job fetching a device file for the editor, and where it lands.
    device_load: Option<(u64, PathBuf)>,
    /// Waiting on the "unsaved edits" question.
    leave_pending: Option<Leave>,
    /// Chose to save first: carry on with this once the export lands.
    leave_after_save: Option<Leave>,
    /// The kit was saved or discarded after its [x]: close it next frame
    /// (not mid-absorb, when the slots may be swapped).
    close_kit_now: bool,
    /// "Close all kits" in progress: closing these one by one.
    close_queue: Option<kit::CloseQueue>,
    /// Save in place is waiting on "trim to selection or whole file?".
    save_scope_pending: bool,
    /// Files the editor is writing (`None`) or just wrote (when), for the
    /// file table's highlight.
    saving: std::collections::HashMap<PathBuf, Option<std::time::Instant>>,
    /// Export hit an existing file: waiting on a new name.
    export_naming: Option<ExportNaming>,
    /// The user okayed closing the window with edits unsaved (or saved them).
    allow_close: bool,
    /// Close once the running job (an upload of the saved edit) finishes.
    close_when_idle: bool,
    /// Dev builds: watches for a newer binary on disk.
    build_watch: relaunch::BuildWatch,
    /// The window is closing to start the new build.
    relaunch: bool,
    /// The centre column shows the Elek Sample Order view instead of the editor.
    elek_mode: bool,
    elek: crate::ui::elek_view::ElekState,
    /// Last file under each pane's cursor and the last slot under the pool's
    /// cursor, so a change pings its match on the other side (`cross_ping`).
    seen_file: [Option<PathBuf>; 2],
    seen_slot: Option<(u32, u32)>,
    /// Local files of pool samples edited since they went to the device
    /// (`ElekState::changed_local`), and when that was last checked.
    changed_paths: std::collections::HashSet<PathBuf>,
    changed_at: Option<std::time::Instant>,
    /// Slots (as read) taking an edited file: push once the last lands.
    update_push: std::collections::HashSet<u16>,
    /// The right column shows the sample pool (SAMPLE POOL in its switch),
    /// not the Destination pane (FILES, or +DRIVE when that pane's active
    /// tab is on the device). See [`Self::pool_in_col`].
    dock_pool: bool,
    /// The right column already snapped to the pool's width once this run.
    pool_snapped: bool,
    /// The docked pool has the keyboard.
    pool_focused: bool,
    /// Where the docked pool was drawn last frame, to tell clicks into it.
    pool_rect: Option<egui::Rect>,
    /// The device view was entered once this run: the first entry reads
    /// the project by itself.
    elek_greeted: bool,
    /// That first read is running: once the names are in, show the
    /// Patterns page with the pool docked beside it.
    first_dock: bool,
    /// The device came back while unpushed project edits were open: ask
    /// whether to drop them and read the device again. Holds the count.
    reconnect_ask: Option<usize>,
    pool_fetch: elek::PoolFetch,
    /// Device files we have a local copy of (see [`crate::links`]).
    links: crate::links::Links,
    /// Audio fingerprints of local files and device samples (see
    /// [`crate::prints`]), and the background run making more.
    prints: crate::prints::Prints,
    print_run: prints::PrintRun,
    /// Last device file opened through a local copy or a fetch:
    /// `(local file, device path)`, for "send to device as".
    device_origin: Option<(PathBuf, PathBuf)>,
    /// Waiting on the "send to device as" question.
    send_back: Option<dialogs::SendBack>,
}

impl App {
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

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, state::KEY, &self.snapshot(true));
    }

    fn on_exit(&mut self) {
        if !self.relaunch {
            return;
        }
        if let Some(exe) = self.build_watch.exe() {
            if let Err(e) = std::process::Command::new(exe).spawn() {
                crate::log::line(format!("error: relaunch failed: {e}"));
            }
        }
    }

    fn auto_save_interval(&self) -> std::time::Duration {
        state::SAVE_INTERVAL
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.os_dnd_input(ctx, raw_input);
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        #[cfg(target_os = "macos")]
        self.os_drag_out(ctx, frame);
        #[cfg(not(target_os = "macos"))]
        let _ = frame;
        self.build_watch.poll(ctx);
        // eframe only autosaves on a frame. After a change, make sure one
        // happens once the interval has passed, even if the app goes idle.
        let snap = self.snapshot(false);
        if self.last_state.as_ref() != Some(&snap)
            || std::mem::take(&mut self.opened_dirty) | std::mem::take(&mut self.committed_dirty)
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
        self.track_presence();
        crate::ui::elek_presets_view::finish_copy(&mut self.elek, ctx);
        self.prioritize_elek_read();
        self.live_push_tick(ctx);
        self.check_changed(ctx);
        self.print_tick(ctx);

        // Back from another app: files may have changed under us.
        let focused = ctx.input(|i| i.focused);
        if focused && !self.window_focused {
            self.reload_local();
        }
        // Leaving for another app: go quiet.
        if !focused && self.window_focused && self.engine.is_playing() {
            self.engine.stop();
        }
        self.window_focused = focused;
        self.absorb_editors();
        if std::mem::take(&mut self.close_kit_now) && self.has_kit() {
            self.close_kit();
        }
        self.step_close_queue();

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
            self.engine
                .update(ed.sel.0, ed.sel.1, ed.prefs.looping && !self.voice_once, ed.gain(), ed.fold());
            ctx.request_repaint();
        }
        self.elek.playing = (self.elek_mode && self.engine.is_playing())
            .then(|| self.voice_path.as_deref())
            .flatten()
            .map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        self.preview.set(if self.engine.is_playing() {
            self.voice_path.clone()
        } else {
            None
        });

        if let Some(e) = self.engine.stream_error() {
            if self.last_stream_error.as_ref() != Some(&e) {
                self.errors.push(format!("audio output: {e}"));
                self.last_stream_error = Some(e);
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show_layout(ui);
        self.cross_ping();
        self.show_pending_dialogs(ui.ctx());

        self.handle_keys(ui);
        self.follow_cursor();

        // Undo steps are recorded by diffing the editor state once a frame,
        // after any drag, slider or typed value has been let go.
        let (busy_ptr, now) = ui.input(|i| (i.pointer.any_down(), i.time));
        let busy = busy_ptr || self.editor.drag.is_some() || ui.ctx().text_edit_focused();
        self.editor.track_history(busy, now);
        self.other.track_history(false, now);

        // Buttons and toggles take keyboard focus when clicked, after which
        // egui would turn Space/Enter into clicks on them and arrows into
        // focus moves. This app drives everything from its own keymaps, so
        // only text fields get to keep focus.
        let ctx = ui.ctx().clone();
        if !ctx.text_edit_focused() && self.kit_naming.is_none() {
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

pub(crate) fn short_name(s: &str, n: usize) -> String {
    truncate(s, n)
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
