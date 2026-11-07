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
    SaveInPlace,
    /// Show the slice kit (`true`) or the browsed sample.
    ShowKit(bool),
    AddSlice,
    UseAsKit,
    CloseKit,
    /// Lay the kit out on this many equal slots, or free it.
    SetGrid(Option<usize>),
    /// Give the kit this name (no extension).
    RenameKit(String),
    /// Make parked kit `n` the one K adds to.
    SwitchKit(usize),
    /// Start another kit from the sample's selection.
    NewKit,
    /// Switch the column to the Elk Sample Order view.
    ElkMode,
    /// Fade and cut every kit slice's tail below the threshold.
    TrimTails,
    /// Upload what you hear as a new file next to its device original.
    SendToDevice,
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
    pub showing_kit: bool,
    pub kit_name: Option<String>,
    pub kit_dirty: bool,
    /// As picked: free, [`crate::editor::AUTO_GRID`] or a slot count.
    pub kit_grid: Option<usize>,
    /// Slices so far (free) or slots used (grid), and the slot count.
    pub kit_fill: (usize, Option<usize>),
    /// The kit's file exists (it was exported or opened from disk).
    pub kit_on_disk: bool,
    /// Every open kit, sorted by name.
    pub kits: Vec<KitEntry>,
    /// File in the sample slot, shown or not.
    pub sample_name: Option<String>,
    /// The device file the sample came from or went to, when known.
    pub device_home: Option<String>,
}

const RULER_H: f32 = 18.0;
const OVERVIEW_H: f32 = 30.0;
const SCOPE_H: f32 = 110.0;
/// Width of the scope strip's fixed window, in seconds.
const SCOPE_SECS: f64 = 0.040;
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

    header(ui, ed, focused, &mut out);
    slot_bar(ui, ed, slots, &mut out);
    toolbar(ui, ed, &pv, can_export, slots, &mut out);

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

    // File row sits right above the last of wave / scalpel.
    if !pn.scalpel {
        file_row(ui, ed, can_export, slots, &mut out);
    }

    section_header(ui, "Wave");
    let wave_h = (avail.y - reserved - 12.0).max(80.0);
    let (rect, _) = ui.allocate_exact_size(vec2(avail.x, wave_h), Sense::hover());
    waveform(ui, ed, &pv, rect, focused, &fades, trim.as_ref(), &mut out);

    if pn.nums {
        section_header(ui, "Nums");
        positions(ui, ed, &pv);
    }

    if pn.scalpel {
        file_row(ui, ed, can_export, slots, &mut out);
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
