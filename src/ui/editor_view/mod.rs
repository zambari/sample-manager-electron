//! Drawing and mouse handling for the sample editor column.
//!
//! Mouse: click parks the playhead, drag on empty space selects a region,
//! drag near the in/out lines moves them, drag the ruler bar between them
//! slides the whole region, double-click on an in/out line
//! parks the playhead there, double-click elsewhere selects all (in a kit:
//! the slice between the markers around it; click near a slice marker parks
//! the playhead exactly on it), wheel
//! zooms around the pointer, horizontal scroll / middle-drag / ⌥-drag pans.
//! Clicks and drags within a few pixels of a slice marker or a file end land
//! exactly on it; a click on an in/out line parks the playhead exactly there.
//! Keys are handled in the app (see `App::perform_editor`).

use crate::audio::stats::{self, Fold, Stats};
use crate::editor::{Drag, Editor};
use crate::ui::theme::{SECTION_H, section_header, vis_toggle};
use egui::{Align2, Color32, CursorIcon, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui, vec2};
mod strip;
mod stats_view;
mod overview;
mod scope;
mod level_strip;
mod toolbar;
mod trim;
mod waveform;

use stats_view::{STATS_H, stats_panel};
use overview::overview;
use scope::scope;
use trim::trim_section;
use toolbar::{file_row, header, positions, slot_bar, toolbar};
use waveform::{draw_lanes, draw_ruler, marker, slice_marks, waveform};

pub struct PlayView {
    /// True when the engine is playing *this* sample.
    pub playing: bool,
    pub pos: usize,
}

pub enum EdIntent {
    TookFocus,
    TogglePlay,
    /// Playhead was moved while playing: jump the voice there.
    Seek(usize),
    Export,
    /// Export the kit, then close it once the file is written.
    ExportAndClose,
    SaveInPlace,
    /// Select this local file in the source pane (the open file, or the
    /// original it was uploaded from).
    ShowInSource(std::path::PathBuf),
    /// Kit slice (0-based): flash the pool slot holding its source.
    SliceInPool(usize),
    /// Flash the pool slot the open file is on the device as.
    OriginInPool,
    /// Show the slice kit (`true`) or the browsed sample.
    ShowKit(bool),
    AddSlice,
    UseAsKit,
    CloseKit,
    /// Close every open kit (`false`) or all but the active one (`true`),
    /// asking about each with unsaved slices.
    CloseAllKits(bool),
    /// Lay the kit out on this many equal slots, or free it.
    SetGrid(Option<usize>),
    /// Give the kit this name (no extension).
    RenameKit(String),
    /// Make parked kit `n` the one K adds to.
    SwitchKit(usize),
    /// Start another kit from the sample's selection.
    NewKit,
    /// The DEVICE switch: close the device panels and its MIDI ports, or
    /// connect again.
    ToggleDevice,
    /// Open the Device view on this page (Samples / Patterns).
    DevicePage(crate::ui::elek_view::ElekPage),
    /// The +Drive tab next to Patterns: show the +Drive in the right column.
    ShowDrive,
    /// Fade and cut every kit slice's tail below the threshold.
    TrimTails,
    /// Upload what you hear as a new file next to its device original.
    SendToDevice,
    /// Put what you hear into a pool slot (a filled one asks first).
    SaveToSlot(SlotPick),
    /// Write what you hear over the local original of a device copy (asks first).
    SaveOverOriginal,
}

/// Which pool slot "Save to" aims at.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlotPick {
    /// The one slot selected in the device view, empty or filled.
    Selected,
    /// The first empty slot in the pool.
    FirstFree,
}

/// Pool slots the editor's sample could be saved into.
pub struct PoolTargets {
    /// The one selected slot: its label, and the name of what it holds
    /// (`None` when empty).
    pub selected: Option<(String, Option<String>)>,
    /// The first empty slot's label.
    pub first_free: Option<String>,
    /// Why nothing can go in right now.
    pub note: Option<&'static str>,
}

/// One open kit, for the kit switcher.
pub struct KitEntry {
    pub name: String,
    pub dirty: bool,
    /// Index among the parked kits; `None` for the active one.
    pub parked: Option<usize>,
}

/// What the header needs to know about the two editor slots.
pub struct Slots {
    /// The right column shows the +Drive (lights its Device tab).
    pub drive_shown: bool,
    pub showing_kit: bool,
    pub kit_name: Option<String>,
    pub kit_dirty: bool,
    /// As picked: free, [`crate::editor::AUTO_GRID`] or a slot count.
    pub kit_grid: Option<usize>,
    /// Slices so far (free) or slots used (grid), and the slot count.
    pub kit_fill: (usize, Option<usize>),
    /// The kit has a file of its own to save over: exported, or opened from
    /// a kit file. False while it still points at a sample its slices came
    /// from.
    pub kit_on_disk: bool,
    /// Every open kit, sorted by name.
    pub kits: Vec<KitEntry>,
    /// File in the sample slot, shown or not.
    pub sample_name: Option<String>,
    /// The device file the sample came from or went to, when known.
    pub device_home: Option<String>,
    /// The sample came from this device pool slot: saving updates it there.
    pub pool_slot: Option<String>,
    /// The active destination tab is on the +Drive: this folder there.
    pub export_drive: Option<String>,
    /// Pool slots "Save to" can aim at; None without a device project.
    pub pool: Option<PoolTargets>,
    /// Which file the editor has open, in words (sample slot only).
    pub origin: Option<Origin>,
    /// Kit: where each slice can be found, by 0-based slice (empty when
    /// no kit is shown).
    pub slice_homes: Vec<SliceHome>,
}

/// Where a kit slice's source can be shown, for its right-click menu.
#[derive(Clone, Default)]
pub struct SliceHome {
    /// The local file it came from (an original rather than a preview
    /// cache copy, when known).
    pub local: Option<std::path::PathBuf>,
    /// `local` was found by the slice's audio, not by a saved name.
    pub by_audio: bool,
    /// The pool slot holding the same device file, e.g. `A012`: by a
    /// transfer link or audio fingerprint, else by file name.
    pub pool: Option<String>,
    /// That slot's audio is the source's (`Some(true)`), differs
    /// (`Some(false)`), or wasn't compared (found by name, never fetched).
    pub pool_same: Option<bool>,
    /// Found only by file name.
    pub pool_by_name: bool,
    /// What the kit recorded, when nothing could be resolved from it.
    pub saved_as: Option<String>,
}

/// What the open file is, relative to the device: the line under **File**.
pub struct Origin {
    /// The open file.
    pub path: std::path::PathBuf,
    /// Short kind: "Original", "Device copy (preview cache)", ...
    pub kind: &'static str,
    /// The kind needs attention (not what the device plays, or a cache copy
    /// with an original elsewhere).
    pub warn: bool,
    pub tip: String,
    /// A cache or fetched copy whose original we know: `(path, unchanged
    /// since upload)`.
    pub original: Option<(std::path::PathBuf, bool)>,
    /// The pool slot holding it (see [`SliceHome`]): `(slot, same audio,
    /// found by name only)`.
    pub pool: Option<(String, Option<bool>, bool)>,
}

const RULER_H: f32 = 18.0;
const OVERVIEW_H: f32 = 30.0;
const SCOPE_H: f32 = 110.0;
/// Width of the scope strip's fixed window, in seconds.
const SCOPE_SECS: f64 = 0.080;
const GRAB_PX: f32 = 6.0;
/// Clicks and drags this close to a slice marker or a file end land on it.
const SNAP_PX: f32 = 8.0;
const MIN_FPP: f64 = 1.0 / 48.0;
/// Below this many frames per pixel, draw the waveform as a smooth filled
/// curve through the samples instead of per-column min/max bars.
const SOLID_CURVE_FPP: f64 = 4.0;

pub const IN_COLOR: Color32 = Color32::from_rgb(90, 200, 120);
pub const OUT_COLOR: Color32 = Color32::from_rgb(235, 115, 90);
const PLAY_COLOR: Color32 = Color32::from_rgb(245, 200, 70);
const CLIP_COLOR: Color32 = Color32::from_rgb(235, 60, 60);
/// Audio the soft limiter is bending (gain pushed into the top dB).
const LIMIT_COLOR: Color32 = Color32::from_rgb(236, 165, 55);
const MONO_COLOR: Color32 = Color32::from_rgb(120, 170, 245);
const MARK_COLOR: Color32 = Color32::from_rgb(200, 140, 245);

pub fn show(
    ui: &mut Ui,
    ed: &mut Editor,
    pv: PlayView,
    focused: bool,
    can_export: bool,
    slots: &Slots,
) -> Vec<EdIntent> {
    let mut out = Vec::new();

    header(ui, ed, focused, slots.drive_shown, &mut out);
    slot_bar(ui, ed, slots, &mut out);
    toolbar(ui, ed, &pv, slots, &mut out);

    // Tail trim, kits only: its section, and its fades and cuts on the
    // wave. Hidden, nothing is shown or draggable there. Found after the
    // toolbar so its edits count.
    let trimming = ed.prefs.panels.trim_tails && ed.is_kit && ed.current.is_some();
    if trimming {
        let ends = ed.slice_ends();
        trim_section(ui, ed, &ends, &mut out);
    }
    let trim = if trimming { ed.active_trim_plan() } else { None };
    let fades = trim.as_ref().map_or_else(Vec::new, |p| ed.shown_fades(&p.fades()));

    if ed.current.is_none() {
        let msg = if let Some(p) = &ed.loading {
            format!(
                "Loading {} …",
                p.file_name().unwrap_or_default().to_string_lossy()
            )
        } else if let Some(e) = &ed.error {
            e.clone()
        } else {
            "Move the cursor onto a sample in Source or Destination to load it here.".to_owned()
        };
        ui.add_space(40.0);
        ui.vertical_centered(|ui| ui.label(RichText::new(msg).weak()));
        let resp = ui.interact(ui.max_rect(), ui.scope_id().with("empty"), Sense::click());
        if resp.clicked() {
            out.push(EdIntent::TookFocus);
        }
        return out;
    }

    let pn = ed.prefs.panels.clone();

    // The waveform takes whatever height the other shown panels leave.
    let avail = ui.available_size();
    let phase = pn.stats && ed.is_multichannel();
    let sec = SECTION_H + ui.spacing().item_spacing.y;
    let mut reserved = 0.0;
    // The wave is always shown.
    reserved += sec;
    // File row (Save / Export).
    reserved += ui.spacing().interact_size.y + sec;
    if pn.nums {
        reserved += ui.spacing().interact_size.y + sec;
    }
    if pn.scalpel {
        reserved += SCOPE_H + sec;
    }
    if pn.minimap {
        reserved += OVERVIEW_H + sec;
    }
    if phase {
        reserved += 20.0;
    }
    if pn.stats {
        reserved += STATS_H + sec;
    }
    // Measure the wrapped help text so the waveform leaves room for it.
    let help_font = egui::TextStyle::Small.resolve(ui.style());
    let help_h = if pn.help {
        let g = ui.fonts_mut(|f| {
            f.layout(
                crate::keymap::EDITOR_HELP.to_owned(),
                help_font.clone(),
                egui::Color32::WHITE,
                avail.x,
            )
        });
        g.size().y + sec
    } else {
        0.0
    };
    reserved += help_h;

    // File row (mono preview, Save / Export) sits right above the wave.
    file_row(ui, ed, can_export, slots, &mut out);

    section_header(ui, "Wave");
    let wave_h = (avail.y - reserved - 12.0).max(80.0);
    let (rect, _) = ui.allocate_exact_size(vec2(avail.x, wave_h), Sense::hover());
    waveform(ui, ed, &pv, rect, focused, &fades, trim.as_ref(), &slots.slice_homes, &mut out);

    if pn.nums {
        section_header(ui, "Nums");
        positions(ui, ed, &pv);
    }

    if pn.scalpel {
        section_header(ui, "Scalpel");
        let (srect, _) = ui.allocate_exact_size(vec2(avail.x, SCOPE_H), Sense::hover());
        scope(ui, ed, &pv, srect, &mut out);
    }

    if pn.minimap {
        section_header(ui, "Map");
        let (orect, _) = ui.allocate_exact_size(vec2(avail.x, OVERVIEW_H), Sense::hover());
        overview(ui, ed, &pv, orect, &mut out);
    }

    if pn.stats {
        stats_panel(ui, ed, &pv);
    }

    if pn.help {
        section_header(ui, "Help");
        ui.label(RichText::new(crate::keymap::EDITOR_HELP).weak().small());
    }
    out
}

// ---- view math --------------------------------------------------------------

struct Map {
    rect: Rect,
    start: f64,
    fpp: f64,
    frames: usize,
}

impl Map {
    fn x(&self, frame: f64) -> f32 {
        self.rect.left() + ((frame - self.start) / self.fpp) as f32
    }
    fn frame(&self, x: f32) -> usize {
        let f = self.start + (x - self.rect.left()) as f64 * self.fpp;
        f.round().clamp(0.0, self.frames as f64) as usize
    }
}

pub fn clamp_view(ed: &mut Editor, width: f32) {
    let frames = ed.frames() as f64;
    let w = width.max(1.0) as f64;
    let max_fpp = (frames / w).max(MIN_FPP);
    ed.view.fpp = ed.view.fpp.clamp(MIN_FPP, max_fpp);
    let max_start = (frames - w * ed.view.fpp).max(0.0);
    ed.view.start = ed.view.start.clamp(0.0, max_start);
}

/// Zoom by `factor` (<1 zooms in) keeping `anchor_x` fixed on screen.
pub fn zoom_at(ed: &mut Editor, rect: Rect, anchor_x: f32, factor: f64) {
    let anchor = ed.view.start + (anchor_x - rect.left()) as f64 * ed.view.fpp;
    ed.view.fpp *= factor;
    clamp_view(ed, rect.width());
    ed.view.start = anchor - (anchor_x - rect.left()) as f64 * ed.view.fpp;
    clamp_view(ed, rect.width());
}

/// Scroll so `frame` is on screen, leaving a margin, if it isn't already.
pub fn ensure_visible(ed: &mut Editor, width: f32, frame: usize) {
    let w = width as f64 * ed.view.fpp;
    let f = frame as f64;
    if f < ed.view.start || f > ed.view.start + w {
        ed.view.start = f - w * 0.25;
        clamp_view(ed, width);
    }
}
