//! Drawing and mouse handling for the sample editor column.
//!
//! Mouse: click parks the playhead, drag on empty space selects a region,
//! drag near the in/out lines moves them, drag the ruler bar between them
//! slides the whole region, double-click on an in/out line
//! parks the playhead there, double-click elsewhere selects all (in a kit:
//! the slice between the markers around it; click near a slice marker parks
//! the playhead exactly on it), wheel
//! zooms around the pointer, horizontal scroll / middle-drag / ⌥-drag pans.
//! Keys are handled in the app (see `App::perform_editor`).

use crate::audio::stats::{self, Fold, Stats};
use crate::editor::{Drag, Editor};
use crate::ui::theme::{SECTION_H, section_header, vis_toggle};
use egui::{Align2, Color32, CursorIcon, FontId, Pos2, Rect, RichText, Sense, Stroke, Ui, vec2};

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
}

/// What the header needs to know about the two editor slots.
pub struct Slots {
    pub showing_kit: bool,
    pub kit_name: Option<String>,
    pub kit_dirty: bool,
    /// File in the sample slot, shown or not.
    pub sample_name: Option<String>,
}

const RULER_H: f32 = 18.0;
const OVERVIEW_H: f32 = 30.0;
const SCOPE_H: f32 = 110.0;
/// Width of the scope strip's fixed window, in seconds.
const SCOPE_SECS: f64 = 0.040;
const GRAB_PX: f32 = 6.0;
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

pub fn show(ui: &mut Ui, ed: &mut Editor, pv: PlayView, focused: bool, can_export: bool, slots: &Slots) -> Vec<EdIntent> {
    let mut out = Vec::new();

    header(ui, ed, focused, &mut out);
    slot_bar(ui, ed, slots, &mut out);
    toolbar(ui, ed, &pv, can_export, slots.showing_kit, &mut out);

    if ed.current.is_none() {
        let msg = if let Some(p) = &ed.loading {
            format!("Loading {} …", p.file_name().unwrap_or_default().to_string_lossy())
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
    if pn.wave {
        reserved += sec;
    }
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
        reserved += 150.0 + sec;
    }
    // Measure the wrapped help text so the waveform leaves room for it.
    let help_font = egui::TextStyle::Small.resolve(ui.style());
    let help_h = if pn.help {
        let g = ui.fonts_mut(|f| f.layout(crate::keymap::EDITOR_HELP.to_owned(), help_font.clone(), egui::Color32::WHITE, avail.x));
        g.size().y + sec
    } else {
        0.0
    };
    reserved += help_h;

    if pn.wave {
        section_header(ui, "Wave");
        let wave_h = (avail.y - reserved - 12.0).max(80.0);
        let (rect, _) = ui.allocate_exact_size(vec2(avail.x, wave_h), Sense::hover());
        waveform(ui, ed, &pv, rect, focused, &mut out);
    }

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
        section_header(ui, "Stats");
    }
    if phase {
        phase_meter(ui, ed, &pv);
    }

    if pn.stats {
        stats_table(ui, ed);
    }

    if pn.help {
        section_header(ui, "Help");
        ui.label(RichText::new(crate::keymap::EDITOR_HELP).weak().small());
    }
    out
}

/// L/R correlation bar: −1 (cancels in mono) … 0 (unrelated) … +1 (mono).
/// Live around the play cursor while playing, else over the region.
fn phase_meter(ui: &mut Ui, ed: &mut Editor, pv: &PlayView) {
    let live = pv.playing;
    let ph = if live { ed.phase_at(pv.pos) } else { ed.region_phase() };
    ui.horizontal(|ui| {
        ui.label(RichText::new("Phase").weak()).on_hover_text(
            "L/R correlation. +1: both sides the same (mono-safe). 0: unrelated / very wide. \
             Below 0: out of phase — those parts cancel when folded to mono.",
        );
        let (rect, _) = ui.allocate_exact_size(vec2(180.0, 12.0), Sense::hover());
        let vis = ui.visuals().clone();
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 2.0, vis.extreme_bg_color);
        let mid = rect.center().x;
        let left = Rect::from_x_y_ranges(rect.left()..=mid, rect.y_range());
        p.rect_filled(left, 2.0, CLIP_COLOR.gamma_multiply(0.18));
        for t in [-0.5f32, 0.0, 0.5] {
            let x = mid + t * rect.width() / 2.0;
            let h = if t == 0.0 { 0.0 } else { 3.0 };
            p.line_segment([Pos2::new(x, rect.top() + h), Pos2::new(x, rect.bottom() - h)], Stroke::new(1.0, vis.weak_text_color().gamma_multiply(0.6)));
        }
        // Ease the needle so the live reading is readable rather than jittery.
        let target = ph.corr.unwrap_or(0.0);
        let c = ui.ctx().animate_value_with_time(ui.scope_id().with("phase_needle"), target, if live { 0.12 } else { 0.0 });
        if ph.corr.is_some() {
            let x = mid + c * rect.width() / 2.0;
            let col = if c < 0.0 { CLIP_COLOR } else if c < 0.3 { vis.warn_fg_color } else { IN_COLOR };
            p.rect_filled(Rect::from_x_y_ranges(mid.min(x)..=mid.max(x), rect.shrink(3.0).y_range()), 1.0, col.gamma_multiply(0.5));
            p.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(2.0, col));
        }
        p.rect_stroke(rect, 2.0, Stroke::new(1.0, vis.widgets.noninteractive.bg_stroke.color), egui::StrokeKind::Inside);
        let corr = ph.corr.map_or("  —  ".to_owned(), |c| format!("{c:+.2}"));
        ui.monospace(corr);
        ui.separator();
        let loss = RichText::new(format!("mono sum {}", stats::fmt_db(ph.mono_loss_db))).monospace();
        ui.label(if ph.mono_loss_db < -4.0 { loss.color(vis.warn_fg_color) } else { loss.weak() })
            .on_hover_text("Level of the mono fold relative to the stereo signal. ~0 dB: nothing lost. ~−3 dB: wide/unrelated sides. Much lower: cancellation.");
        ui.label(RichText::new(if live { "live" } else { "region" }).weak().small());
    });
}

fn header(ui: &mut Ui, ed: &mut Editor, focused: bool, out: &mut Vec<EdIntent>) {
    ui.horizontal(|ui| {
        if ui.add(crate::ui::theme::pane_title("Sample Editor", focused)).clicked() {
            out.push(EdIntent::TookFocus);
        }
        ui.separator();
        if let Some(p) = ed.path() {
            let mut name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            if ed.is_dirty() {
                name.push_str("  ● unsaved");
            } else if ed.is_edited() {
                name.push_str("  •");
            }
            ui.add(egui::Label::new(RichText::new(name).strong()).truncate())
                .on_hover_text(p.display().to_string());
        }
        if let (Some((a, b)), Some(s)) = (ed.crop, ed.sample()) {
            ui.label(RichText::new(format!("✂ cropped view {:.3}–{:.3}s", s.seconds(a), s.seconds(b))).weak())
                .on_hover_text("Only part of the file is loaded for editing. ⇧C or Uncrop shows the whole file again.");
        }
        if let Some(f) = ed.fold() {
            let txt = if f == Fold::Mix { "◐ MONO".to_owned() } else { format!("◐ MONO {}", f.short()) };
            ui.label(RichText::new(txt).color(MONO_COLOR).strong())
                .on_hover_text("Playing folded to mono. M toggles.");
        }
        if ed.loading.is_some() {
            ui.spinner();
        }
    });
    panel_toggles(ui, &mut ed.prefs.panels);
}

/// Sample / Kit switch, plus the kit's own buttons.
fn slot_bar(ui: &mut Ui, ed: &Editor, s: &Slots, out: &mut Vec<EdIntent>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let sample = s.sample_name.as_deref().unwrap_or("—");
        if ui
            .selectable_label(!s.showing_kit, format!("Sample: {}", crate::app::short_name(sample, 28)))
            .on_hover_text("B — the sample you're browsing. Moving the file cursor always loads here.")
            .clicked()
        {
            out.push(EdIntent::ShowKit(false));
        }
        // No kit yet: nothing to show until K starts one.
        if let Some(k) = &s.kit_name {
            let mut label = format!("Kit: {}", crate::app::short_name(k, 28));
            if s.kit_dirty {
                label.push_str(" ●");
            }
            if ui
                .selectable_label(s.showing_kit, label)
                .on_hover_text("B — the slice kit. K adds the sample's selection to its end. ⌘S here saves the whole kit.")
                .clicked()
            {
                out.push(EdIntent::ShowKit(true));
            }
            if !s.showing_kit
                && ui
                    .add_enabled(ed.current.is_some(), egui::Button::new("→").small())
                    .on_hover_text(format!("K — add the selection (as you hear it) to the end of {k}"))
                    .clicked()
            {
                out.push(EdIntent::AddSlice);
            }
            if ui
                .add_enabled(!s.kit_dirty, egui::Button::new("✕").small())
                .on_hover_text(if s.kit_dirty { "Save the kit (⌘S in kit view) before closing it" } else { "Close the kit (the file stays)" })
                .clicked()
            {
                out.push(EdIntent::CloseKit);
            }
        }
        if !s.showing_kit && ed.current.is_some() {
            ui.separator();
            if ui
                .small_button("Use as kit")
                .on_hover_text("Make this file the slice kit, to add slices to its end")
                .clicked()
            {
                out.push(EdIntent::UseAsKit);
            }
        }
    });
}

/// Right-aligned chips that show / hide each editor panel. Saved with the prefs.
fn panel_toggles(ui: &mut Ui, p: &mut crate::editor::Panels) {
    section_header(ui, "Show");
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let items: [(&mut bool, &str, &str); 9] = [
            (&mut p.transport, "Transport", "Play, loop, follow, snap and auto-load / auto-play"),
            (&mut p.gain, "Gain", "Gain and normalize"),
            (&mut p.tools, "Tools", "Undo, crop, loop scaling, export"),
            (&mut p.wave, "Wave", "Main waveform"),
            (&mut p.nums, "Nums", "In / out / len / playhead readout, under the waveform"),
            (&mut p.scalpel, "Scalpel", "40 ms extreme-zoom strip around the playhead"),
            (&mut p.minimap, "Map", "Minimap: the whole file, click to jump"),
            (&mut p.stats, "Stats", "Stats table and phase meter"),
            (&mut p.help, "Help", "Keyboard shortcut list"),
        ];
        for (v, label, hover) in items {
            ui.add(vis_toggle(v, label)).on_hover_text(format!("Show / hide: {hover}"));
        }
    });
}

fn toolbar(ui: &mut Ui, ed: &mut Editor, pv: &PlayView, can_export: bool, showing_kit: bool, out: &mut Vec<EdIntent>) {
    let playing = pv.playing;
    let has = ed.current.is_some();
    let pn = ed.prefs.panels.clone();
    if pn.transport {
    section_header(ui, "Transport");
    ui.horizontal_wrapped(|ui| {
        let icon = if playing { "⏹" } else { "▶" };
        let hover = if playing { "Stop (Space)" } else { "Play (Space)" };
        if ui.add_enabled(has, egui::Button::new(icon)).on_hover_text(hover).clicked() {
            out.push(EdIntent::TogglePlay);
        }
        // One button that flips the start point.
        let (label, hover) = if ed.prefs.play_from_in {
            ("[ Playhead", "Space plays from the in point (⇧Space: from the current position). Click to switch.")
        } else {
            ("▸ From current", "Space plays from the parked playhead (⇧Space: from the in point). Click to switch.")
        };
        if ui.button(label).on_hover_text(hover).clicked() {
            ed.prefs.play_from_in = !ed.prefs.play_from_in;
        }
        ui.separator();
        ui.toggle_value(&mut ed.prefs.looping, "🔁 Loop").on_hover_text("L — loop the in/out region");
        ui.toggle_value(&mut ed.prefs.follow_playback, "Follow")
            .on_hover_text("V — scroll the view along with the play cursor. Turn off to stay put while zoomed in on a loop end.");
        if ui
            .toggle_value(&mut ed.prefs.snap_zero, "Snap to 0")
            .on_hover_text("S — snap in/out points to the nearest zero crossing. Turning it on snaps the current selection.")
            .changed()
            && ed.prefs.snap_zero
        {
            ed.snap_selection();
        }
        ui.separator();
        ui.toggle_value(&mut ed.prefs.auto_load, "Auto-load")
            .on_hover_text("Load the sample under the file cursor");
        ui.toggle_value(&mut ed.prefs.auto_play, "Auto-play")
            .on_hover_text("Play each sample as the cursor lands on it");
    });
    }
    if pn.gain {
    section_header(ui, "Gain");
    ui.horizontal_wrapped(|ui| {
        ui.add_enabled_ui(has, |ui| {
            ui.label("Gain");
            ui.add(
                egui::Slider::new(&mut ed.gain_db, -36.0..=36.0)
                    .suffix(" dB")
                    .step_by(0.1)
                    .clamping(egui::SliderClamping::Never),
            )
            .on_hover_text("Double-click the number to type a value");
            if ui.button("Normalize").on_hover_text("N — set gain so the region peaks at the target").clicked() {
                ed.normalize();
            }
            ui.add(
                egui::DragValue::new(&mut ed.prefs.normalize_db)
                    .range(-24.0..=0.0)
                    .speed(0.05)
                    .suffix(" dB"),
            )
            .on_hover_text("Normalize target peak");
            if ui.button("Reset").on_hover_text("Whole file, 0 dB").clicked() {
                ed.reset();
            }
        });
    });
    }
    if pn.tools {
    section_header(ui, "Tools");
    ui.horizontal_wrapped(|ui| {
        ui.add_enabled_ui(has, |ui| {
            if ui.add_enabled(ed.can_undo(), egui::Button::new("↶ Undo")).on_hover_text("⌘Z").clicked() {
                ed.undo();
            }
            if ui.add_enabled(ed.can_redo(), egui::Button::new("↷ Redo")).on_hover_text("⇧⌘Z").clicked() {
                ed.redo();
            }
            ui.separator();
            let whole = ed.sel == (0, ed.frames());
            if ui
                .add_enabled(ed.can_remove(), egui::Button::new("Cut"))
                .on_hover_text("⌘X — take the selection out of the audio, onto the clipboard")
                .clicked()
            {
                ed.cut();
            }
            if ui.button("Copy").on_hover_text("⌘C — copy the selection (with gain) to the clipboard. It stays there when you open another file.").clicked() {
                ed.copy();
            }
            let paste_tip = match &ed.clip {
                Some(c) if whole => format!("⌘V — insert {:.3}s at the playhead", c.frames() as f64 / c.rate.max(1) as f64),
                Some(c) => format!("⌘V — replace the selection with {:.3}s", c.frames() as f64 / c.rate.max(1) as f64),
                None => "⌘V — nothing copied yet".to_owned(),
            };
            if ui.add_enabled(ed.can_paste(), egui::Button::new("Paste")).on_hover_text(paste_tip).clicked() {
                ed.paste();
            }
            if ui
                .button("+100 ms")
                .on_hover_text("G — insert 100 ms of silence just before the playhead, pushing everything after it (slice markers too) later")
                .clicked()
            {
                ed.insert_silence(0.1);
            }
            if ui
                .add_enabled(ed.is_dirty() && !ed.exporting, egui::Button::new("Save"))
                .on_hover_text("⌘S — write what you hear back over the original file")
                .clicked()
            {
                out.push(EdIntent::SaveInPlace);
            }
            ui.separator();
            if ui
                .button("✂ Crop")
                .on_hover_text("C — load only the selected region, to zoom and fine-tune inside it. The file isn't changed.")
                .clicked()
            {
                ed.crop_to_selection();
            }
            if ui
                .add_enabled(ed.crop.is_some(), egui::Button::new("Uncrop"))
                .on_hover_text("⇧C — show the whole file again, keeping the selection")
                .clicked()
            {
                ed.uncrop();
            }
            ui.separator();
            if ui
                .add_enabled(ed.sel.1 - ed.sel.0 >= 2, egui::Button::new("½ Loop"))
                .on_hover_text("Halve the selected region, keeping the in point")
                .clicked()
            {
                ed.scale_selection(0.5);
            }
            if ui
                .add_enabled(ed.can_double_selection(), egui::Button::new("×2 Loop"))
                .on_hover_text("Double the selected region, keeping the in point")
                .clicked()
            {
                ed.scale_selection(2.0);
            }
            if ui
                .add_enabled(ed.can_shift_selection(-1), egui::Button::new("◀"))
                .on_hover_text("Move the loop left by half its length")
                .clicked()
            {
                ed.shift_selection(-1);
            }
            if ui
                .add_enabled(ed.can_shift_selection(1), egui::Button::new("▶"))
                .on_hover_text("Move the loop right by half its length")
                .clicked()
            {
                ed.shift_selection(1);
            }
            ui.separator();
            // One control for listening and export: stereo, or which mono fold.
            ui.label("◐").on_hover_text("Mono: what you hear, the stats, Normalize and export all follow. M toggles.");
            let mut mono = ed.prefs.mono;
            let mut changed = ui
                .selectable_value(&mut mono, None, "Stereo")
                .on_hover_text("Keep all channels. M toggles mono.")
                .changed();
            for f in Fold::ALL {
                let tip = match f {
                    Fold::Mix => "Mono: L+R averaged. Very wide stereo can lose level or sound hollow (phase cancellation).",
                    Fold::Left => "Mono from the left channel only — no phase cancellation.",
                    Fold::Right => "Mono from the right channel only — no phase cancellation.",
                };
                changed |= ui.selectable_value(&mut mono, Some(f), f.short()).on_hover_text(tip).changed();
            }
            if changed {
                ed.set_mono(mono);
            }
            let label = if ed.exporting {
                "Exporting…"
            } else if showing_kit {
                if ed.exports_mono() { "Export kit (mono)" } else { "Export kit" }
            } else if ed.exports_mono() {
                match ed.fold() {
                    Some(Fold::Left) => "Export mono (L) → Destination",
                    Some(Fold::Right) => "Export mono (R) → Destination",
                    _ => "Export mono → Destination",
                }
            } else {
                "Export → Destination"
            };
            let tip = if showing_kit {
                "↩ — write the whole kit (in / out ignored), with its slice markers, into the active destination tab, then name it"
            } else {
                "↩ — write the trimmed, gained region as a new WAV into the active destination tab"
            };
            if ui
                .add_enabled(can_export && !ed.exporting, egui::Button::new(label))
                .on_hover_text(tip)
                .clicked()
            {
                out.push(EdIntent::Export);
            }
            if !showing_kit
                && ui
                    .add_enabled(ed.current.is_some(), egui::Button::new("+ Slice → Kit"))
                    .on_hover_text("K — add the selection (as you hear it) to the end of the slice kit")
                    .clicked()
            {
                out.push(EdIntent::AddSlice);
            }
        });
    });
    }
}

/// In / out / length / playhead readout. In and out are drag-or-type fields
/// in frames for when the mouse isn't precise enough.
fn positions(ui: &mut Ui, ed: &mut Editor, pv: &PlayView) {
    let Some(s) = ed.sample().cloned() else { return };
    let frames = s.frames();
    let secs = |f: usize| format!("{:.3}s", s.seconds(f));
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("In").color(IN_COLOR));
        let mut i = ed.sel.0;
        if ui.add(egui::DragValue::new(&mut i).range(0..=frames).speed(1.0)).changed() {
            ed.sel.0 = i.min(ed.sel.1.saturating_sub(1));
        }
        ui.label(RichText::new(secs(ed.sel.0)).weak().monospace());
        ui.separator();
        ui.label(RichText::new("Out").color(OUT_COLOR));
        let mut o = ed.sel.1;
        if ui.add(egui::DragValue::new(&mut o).range(0..=frames).speed(1.0)).changed() {
            ed.sel.1 = o.max(ed.sel.0 + 1).min(frames);
        }
        ui.label(RichText::new(secs(ed.sel.1)).weak().monospace());
        ui.separator();
        let len = ed.sel.1 - ed.sel.0;
        ui.label(RichText::new(format!("Len {} · {len} fr", secs(len))).monospace());
        ui.separator();
        let (label, f) = if pv.playing { ("▶", pv.pos) } else { ("▸", ed.playhead) };
        ui.label(RichText::new(format!("{label} {}", secs(f.min(frames)))).color(PLAY_COLOR).monospace());
    });
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

// ---- waveform ---------------------------------------------------------------

fn waveform(ui: &mut Ui, ed: &mut Editor, pv: &PlayView, rect: Rect, focused: bool, out: &mut Vec<EdIntent>) {
    ed.wave_rect = Some(rect);
    let frames = ed.frames();
    if ed.view.fit_pending && rect.width() > 1.0 {
        ed.view.fpp = frames as f64 / rect.width() as f64;
        ed.view.start = 0.0;
        ed.view.fit_pending = false;
    }

    let id = ui.scope_id().with("wave");
    let resp = ui.interact(rect, id, Sense::click_and_drag());

    // Wheel = zoom around the pointer, horizontal scroll = pan. Consumed so
    // nothing else on screen scrolls too.
    if resp.contains_pointer() {
        let (scroll, zoom) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
        if let Some(p) = resp.hover_pos() {
            if scroll.y != 0.0 {
                zoom_at(ed, rect, p.x, (-scroll.y as f64 * 0.004).exp());
            }
            if zoom != 1.0 {
                zoom_at(ed, rect, p.x, 1.0 / zoom as f64);
            }
        }
        if scroll.x != 0.0 {
            ed.view.start -= scroll.x as f64 * ed.view.fpp;
        }
        if scroll != egui::Vec2::ZERO {
            ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
        }
    }

    // Keep the play cursor on screen: page forward when it runs off.
    if pv.playing && ed.drag.is_none() && ed.prefs.follow_playback {
        let x = rect.left() + ((pv.pos as f64 - ed.view.start) / ed.view.fpp) as f32;
        if x > rect.right() || x < rect.left() {
            ed.view.start = pv.pos as f64 - rect.width() as f64 * ed.view.fpp * 0.05;
        }
    }
    clamp_view(ed, rect.width());

    let m = Map { rect, start: ed.view.start, fpp: ed.view.fpp, frames };
    let x_in = m.x(ed.sel.0 as f64);
    let x_out = m.x(ed.sel.1 as f64);
    let marks = if ed.is_kit { ed.marks() } else { Vec::new() };

    // ---- mouse ----
    let near = |x: f32, p: Pos2| (p.x - x).abs() <= GRAB_PX;
    // The kit slice marker under the pointer, if any (nearest wins).
    let mark_at = |p: Pos2| {
        marks
            .iter()
            .copied()
            .map(|f| (f, (m.x(f as f64) - p.x).abs()))
            .filter(|&(_, d)| d <= GRAB_PX)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(f, _)| f)
    };
    // Range bar: the ruler strip between the markers drags the whole region.
    let on_bar = |p: Pos2| {
        p.y >= rect.top() && p.y <= rect.top() + RULER_H && p.x > x_in + GRAB_PX && p.x < x_out - GRAB_PX
    };
    let mut bar_hot = matches!(ed.drag, Some(Drag::Move(..)));
    if let Some(p) = resp.hover_pos() {
        if ed.drag.is_none() {
            if near(x_in, p) || near(x_out, p) {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            } else if on_bar(p) {
                ui.ctx().set_cursor_icon(CursorIcon::Grab);
                bar_hot = true;
            }
        }
    }
    if matches!(ed.drag, Some(Drag::Move(..))) {
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    }
    if resp.drag_started() {
        out.push(EdIntent::TookFocus);
        let origin = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos());
        let alt = ui.input(|i| i.modifiers.alt);
        ed.drag = origin.map(|o| {
            if resp.dragged_by(egui::PointerButton::Middle) || alt {
                Drag::Pan
            } else if near(x_in, o) && (!near(x_out, o) || (o.x - x_in).abs() <= (o.x - x_out).abs()) {
                Drag::In
            } else if near(x_out, o) {
                Drag::Out
            } else if on_bar(o) {
                Drag::Move(ed.sel, m.frame(o.x))
            } else {
                Drag::Select(m.frame(o.x))
            }
        });
    }
    if resp.dragged() {
        if let (Some(d), Some(p)) = (ed.drag, resp.interact_pointer_pos()) {
            let f = m.frame(p.x.clamp(rect.left(), rect.right()));
            match d {
                Drag::In => ed.set_in(f),
                Drag::Out => ed.set_out(f),
                Drag::Select(a) => ed.set_region(a, f),
                Drag::Move(orig, grab) => ed.move_region(orig, f as i64 - grab as i64),
                Drag::Pan => ed.view.start -= resp.drag_delta().x as f64 * ed.view.fpp,
                Drag::Scrub(..) => {}
            }
            // Dragging a marker past the edge scrolls the view along.
            if !matches!(d, Drag::Pan) {
                let edge = 12.0;
                if p.x > rect.right() - edge {
                    ed.view.start += 8.0 * ed.view.fpp;
                } else if p.x < rect.left() + edge {
                    ed.view.start -= 8.0 * ed.view.fpp;
                }
                ui.ctx().request_repaint();
            }
        }
    }
    if resp.drag_stopped() {
        ed.drag = None;
    }
    if resp.clicked() {
        out.push(EdIntent::TookFocus);
        if let Some(p) = resp.interact_pointer_pos() {
            // Near a kit slice marker: land exactly on it.
            ed.playhead = mark_at(p).unwrap_or_else(|| m.frame(p.x));
            if pv.playing {
                out.push(EdIntent::Seek(ed.playhead));
            }
        }
    }
    if resp.double_clicked() {
        // On a marker: park the playhead exactly on it. Elsewhere: select
        // the kit slice there, or everything.
        let p = resp.interact_pointer_pos();
        let target = p.and_then(|p| {
            let (di, dout) = ((p.x - x_in).abs(), (p.x - x_out).abs());
            if let Some(f) = mark_at(p) {
                Some(f)
            } else if near(x_in, p) && di <= dout {
                Some(ed.sel.0)
            } else if near(x_out, p) {
                Some(ed.sel.1.saturating_sub(1))
            } else {
                None
            }
        });
        match target {
            Some(f) => {
                ed.playhead = f;
                if pv.playing {
                    out.push(EdIntent::Seek(f));
                }
            }
            None if ed.is_kit => {
                if let Some(p) = p {
                    let (a, b) = ed.slice_at(m.frame(p.x));
                    ed.sel = (a, b);
                }
            }
            None => ed.select_all(),
        }
    }
    if resp.secondary_clicked() {
        out.push(EdIntent::TookFocus);
    }

    // ---- paint ----
    let painter = ui.painter_at(rect);
    let vis = ui.visuals().clone();
    painter.rect_filled(rect, 2.0, vis.extreme_bg_color);

    let ruler = Rect::from_min_size(rect.min, vec2(rect.width(), RULER_H));
    let lanes = Rect::from_min_max(Pos2::new(rect.left(), ruler.bottom()), rect.max);

    // Region background.
    let reg = Rect::from_x_y_ranges(x_in.max(rect.left())..=x_out.min(rect.right()), lanes.y_range());
    if reg.width() > 0.0 {
        painter.rect_filled(reg, 0.0, vis.selection.bg_fill.gamma_multiply(0.12));
    }

    draw_ruler(&painter, &m, ruler, ed.sample().map_or(1, |s| s.rate), &vis);

    // Range bar across the ruler between the markers.
    let bar = Rect::from_x_y_ranges(x_in.max(rect.left())..=x_out.min(rect.right()), ruler.y_range());
    if bar.width() > 0.0 {
        let a = if bar_hot { 0.45 } else { 0.25 };
        painter.rect_filled(bar, 0.0, vis.selection.bg_fill.gamma_multiply(a));
        // Grip dots in the middle hint that it can be dragged.
        let c = bar.center();
        if bar.width() > 30.0 {
            let col = vis.strong_text_color().gamma_multiply(if bar_hot { 0.8 } else { 0.45 });
            for dx in [-6.0, -2.0, 2.0, 6.0] {
                for dy in [-2.5, 2.5] {
                    painter.circle_filled(Pos2::new(c.x + dx, c.y + dy), 1.0, col);
                }
            }
        }
    }

    draw_lanes(&painter, ed, &m, lanes, &vis);

    // Dim outside the region on top of the wave.
    let dim = vis.extreme_bg_color.gamma_multiply(0.55);
    if x_in > rect.left() {
        painter.rect_filled(Rect::from_x_y_ranges(rect.left()..=x_in.min(rect.right()), lanes.y_range()), 0.0, dim);
    }
    if x_out < rect.right() {
        painter.rect_filled(Rect::from_x_y_ranges(x_out.max(rect.left())..=rect.right(), lanes.y_range()), 0.0, dim);
    }

    slice_marks(&painter, &m, rect, RULER_H, &marks);
    marker(&painter, rect, x_in, IN_COLOR, true);
    marker(&painter, rect, x_out, OUT_COLOR, false);

    // Parked playhead: thin line + small triangle in the ruler.
    let xp = m.x(ed.playhead as f64);
    if rect.x_range().contains(xp) {
        painter.line_segment([Pos2::new(xp, ruler.bottom()), Pos2::new(xp, rect.bottom())], Stroke::new(1.0, vis.strong_text_color().gamma_multiply(0.7)));
        let t = ruler.bottom();
        painter.add(egui::Shape::convex_polygon(
            vec![Pos2::new(xp - 5.0, t - 7.0), Pos2::new(xp + 5.0, t - 7.0), Pos2::new(xp, t)],
            vis.strong_text_color(),
            Stroke::NONE,
        ));
    }
    if pv.playing {
        let x = m.x(pv.pos as f64);
        if rect.x_range().contains(x) {
            painter.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(1.5, PLAY_COLOR));
        }
    }

    if focused {
        painter.rect_stroke(rect, 2.0, Stroke::new(1.0, crate::ui::theme::ACCENT_BRIGHT.gamma_multiply(0.6)), egui::StrokeKind::Inside);
    }
}

/// Waveform lanes (one per channel), min/max bars or individual samples
/// depending on zoom. Shared by the main view and the scope strip.
fn draw_lanes(painter: &egui::Painter, ed: &Editor, m: &Map, lanes: Rect, vis: &egui::Visuals) {
    let Some(cur) = &ed.current else { return };
    let rect = m.rect;
    let n_ch = cur.sample.channels.len().max(1);
    let lane_h = lanes.height() / n_ch as f32;
    let gain = ed.gain();
    let wave_in = crate::ui::theme::WAVE;
    let wave_out = vis.weak_text_color();

    for (c, (data, peaks)) in cur.sample.channels.iter().zip(cur.peaks.iter()).enumerate() {
        let lane = Rect::from_min_size(
            Pos2::new(lanes.left(), lanes.top() + c as f32 * lane_h),
            vec2(lanes.width(), lane_h),
        );
        let mid = lane.center().y;
        let half = lane_h * 0.46;
        let y = |s: f32| (mid - s * gain * half).clamp(lane.top(), lane.bottom());
        painter.line_segment(
            [Pos2::new(lane.left(), mid), Pos2::new(lane.right(), mid)],
            Stroke::new(1.0, vis.faint_bg_color),
        );
        if c > 0 {
            painter.line_segment(
                [lane.left_top(), lane.right_top()],
                Stroke::new(1.0, vis.widgets.noninteractive.bg_stroke.color),
            );
        }
        // Full-scale guides at the current gain, so clipping reads at a glance.
        let fs = half * gain;
        if fs < half {
            for yy in [mid - fs, mid + fs] {
                painter.line_segment(
                    [Pos2::new(lane.left(), yy), Pos2::new(lane.right(), yy)],
                    Stroke::new(1.0, vis.faint_bg_color.gamma_multiply(1.6)),
                );
            }
        }

        if m.fpp >= SOLID_CURVE_FPP {
            // One min/max bar per pixel column, always spanning the zero line
            // so the wave reads as a solid shape rather than a thin trace.
            let w = rect.width().ceil() as usize;
            for px in 0..w {
                let ff0 = m.start + px as f64 * m.fpp;
                if ff0 < 0.0 {
                    continue; // left of the file's start (scope strip near frame 0)
                }
                let f0 = ff0 as usize;
                let f1 = ((m.start + (px + 1) as f64 * m.fpp) as usize).max(f0 + 1);
                if f0 >= data.len() {
                    break;
                }
                let (lo, hi) = peaks.range(data, f0, f1);
                let inside = f1 > ed.sel.0 && f0 < ed.sel.1;
                let clip = hi * gain >= 1.0 || lo * gain <= -1.0;
                let col = if clip && inside { CLIP_COLOR } else if inside { wave_in } else { wave_out };
                let x = rect.left() + px as f32 + 0.5;
                let (ya, yb) = (y(hi.max(0.0)), y(lo.min(0.0)));
                painter.line_segment([Pos2::new(x, ya), Pos2::new(x, yb.max(ya + 1.0))], Stroke::new(1.0, col));
            }
        } else {
            // Few samples per pixel: a filled area between the zero line and
            // the signal, with an anti-aliased curve on top for a smooth edge.
            let f0 = (m.start.floor() - 1.0).max(0.0) as usize;
            let f1 = ((m.start + rect.width() as f64 * m.fpp).ceil() as usize + 2).min(data.len());
            if f1 <= f0 {
                continue;
            }
            let pts: Vec<Pos2> = (f0..f1).map(|f| Pos2::new(m.x(f as f64), y(data[f]))).collect();
            let inside = |f: usize| f >= ed.sel.0 && f < ed.sel.1;
            let fill_col = |f: usize, s: f32| {
                let base = if !inside(f) {
                    wave_out
                } else if s.abs() * gain >= 1.0 {
                    CLIP_COLOR
                } else {
                    wave_in
                };
                base.gamma_multiply(0.55)
            };

            let mut mesh = egui::Mesh::default();
            let mut tri = |a: Pos2, b: Pos2, c: Pos2, col: Color32| {
                let i = mesh.vertices.len() as u32;
                mesh.colored_vertex(a, col);
                mesh.colored_vertex(b, col);
                mesh.colored_vertex(c, col);
                mesh.add_triangle(i, i + 1, i + 2);
            };
            for i in 0..pts.len().saturating_sub(1) {
                let (pa, pb) = (pts[i], pts[i + 1]);
                let (sa, sb) = (data[f0 + i], data[f0 + i + 1]);
                let col = fill_col(f0 + i, if sa.abs() > sb.abs() { sa } else { sb });
                let (ma, mb) = (Pos2::new(pa.x, mid), Pos2::new(pb.x, mid));
                if (sa > 0.0 && sb < 0.0) || (sa < 0.0 && sb > 0.0) {
                    // Crosses zero: split into two triangles at the crossing.
                    let t = (pa.y - mid) / (pa.y - pb.y);
                    let cx = Pos2::new(pa.x + (pb.x - pa.x) * t, mid);
                    tri(ma, pa, cx, col);
                    tri(cx, pb, mb, col);
                } else {
                    tri(ma, pa, pb, col);
                    tri(ma, pb, mb, col);
                }
            }
            painter.add(egui::Shape::mesh(mesh));

            // Outline, split into runs so the selection keeps its colour.
            let mut run: Vec<Pos2> = Vec::new();
            let mut run_in = inside(f0);
            for (i, p) in pts.iter().enumerate() {
                let now_in = inside(f0 + i);
                if now_in != run_in && !run.is_empty() {
                    run.push(*p); // join to the next run without a gap
                    painter.line(std::mem::take(&mut run), Stroke::new(1.2, if run_in { wave_in } else { wave_out }));
                    run_in = now_in;
                }
                run.push(*p);
            }
            if run.len() > 1 {
                painter.line(run, Stroke::new(1.2, if run_in { wave_in } else { wave_out }));
            }

            if 1.0 / m.fpp >= 6.0 {
                for (i, p) in pts.iter().enumerate() {
                    let f = f0 + i;
                    let col = if f >= ed.sel.0 && f < ed.sel.1 { wave_in } else { wave_out };
                    painter.circle_filled(*p, 2.2, col);
                }
            }
        }
    }
}

/// Kit slice starts: a line down the whole view, a tab in the ruler and the
/// slice number beside it.
fn slice_marks(painter: &egui::Painter, m: &Map, rect: Rect, ruler_h: f32, marks: &[usize]) {
    for (i, &f) in marks.iter().enumerate() {
        let x = m.x(f as f64);
        if !(rect.left() - 1.0..=rect.right() + 1.0).contains(&x) {
            continue;
        }
        painter.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(1.0, MARK_COLOR.gamma_multiply(0.85)));
        if ruler_h > 0.0 {
            let r = Rect::from_min_size(Pos2::new(x, rect.top()), vec2(4.0, ruler_h));
            painter.rect_filled(r, 0.0, MARK_COLOR);
            painter.text(Pos2::new(x + 6.0, rect.top() + ruler_h + 2.0), Align2::LEFT_TOP, format!("{}", i + 2), FontId::monospace(10.0), MARK_COLOR);
        }
    }
}

fn marker(painter: &egui::Painter, rect: Rect, x: f32, col: Color32, is_in: bool) {
    if !(rect.left() - 1.0..=rect.right() + 1.0).contains(&x) {
        return;
    }
    painter.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(1.5, col));
    // A flag pointing into the region, so in and out can't be confused.
    let top = rect.top() + RULER_H;
    let dx = if is_in { 9.0 } else { -9.0 };
    painter.add(egui::Shape::convex_polygon(
        if is_in {
            vec![Pos2::new(x, top), Pos2::new(x + dx, top + 5.0), Pos2::new(x, top + 10.0)]
        } else {
            vec![Pos2::new(x + dx, top + 5.0), Pos2::new(x, top), Pos2::new(x, top + 10.0)]
        },
        col,
        Stroke::NONE,
    ));
}

fn draw_ruler(painter: &egui::Painter, m: &Map, r: Rect, rate: u32, vis: &egui::Visuals) {
    painter.rect_filled(r, 0.0, vis.faint_bg_color);
    let pps = rate as f64 / m.fpp; // pixels per second
    const STEPS: &[f64] = &[
        0.0001, 0.0002, 0.0005, 0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0,
        30.0, 60.0, 120.0, 300.0,
    ];
    let step = STEPS.iter().copied().find(|s| s * pps >= 80.0).unwrap_or(600.0);
    let decimals = (-step.log10()).ceil().max(0.0) as usize;
    let t0 = (m.start / rate as f64 / step).floor() * step;
    let t1 = (m.start + r.width() as f64 * m.fpp) / rate as f64;
    let mut t = t0;
    let col = vis.weak_text_color();
    while t <= t1 + step {
        let x = m.x(t * rate as f64);
        if x >= r.left() - 1.0 {
            painter.line_segment([Pos2::new(x, r.bottom() - 6.0), Pos2::new(x, r.bottom())], Stroke::new(1.0, col));
            // Minor ticks.
            for k in 1..5 {
                let xm = m.x((t + step * k as f64 / 5.0) * rate as f64);
                painter.line_segment([Pos2::new(xm, r.bottom() - 3.0), Pos2::new(xm, r.bottom())], Stroke::new(1.0, col.gamma_multiply(0.5)));
            }
            painter.text(Pos2::new(x + 3.0, r.top() + 1.0), Align2::LEFT_TOP, format!("{t:.decimals$}"), FontId::monospace(10.0), col);
        }
        t += step;
    }
}

// ---- scope ------------------------------------------------------------------

/// Fixed 40 ms window centred on the work playhead; it never follows the
/// play cursor (which is drawn only when it passes through). No zoom: drag the wave to nudge the playhead, drag an in/out
/// line that falls inside the window to move it, click to park there.
fn scope(ui: &mut Ui, ed: &mut Editor, pv: &PlayView, rect: Rect, out: &mut Vec<EdIntent>) {
    let frames = ed.frames();
    let Some(rate) = ed.sample().map(|s| s.rate) else { return };
    if frames == 0 || rect.width() < 2.0 {
        return;
    }
    let fpp = SCOPE_SECS * rate as f64 / rect.width() as f64;

    let id = ui.scope_id().with("scope");
    let resp = ui.interact(rect, id, Sense::click_and_drag());

    let centre = ed.playhead;
    let half_w = rect.width() as f64 * 0.5 * fpp;
    let mut m = Map { rect, start: centre as f64 - half_w, fpp, frames };
    let x_in = m.x(ed.sel.0 as f64);
    let x_out = m.x(ed.sel.1 as f64);

    // ---- mouse ----
    let near = |x: f32, p: Pos2| (p.x - x).abs() <= GRAB_PX;
    if let Some(p) = resp.hover_pos() {
        if ed.drag.is_none() {
            let icon = if near(x_in, p) || near(x_out, p) { CursorIcon::ResizeHorizontal } else { CursorIcon::Grab };
            ui.ctx().set_cursor_icon(icon);
        }
    }
    if resp.drag_started() {
        out.push(EdIntent::TookFocus);
        let origin = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos());
        ed.drag = origin.map(|o| {
            if near(x_in, o) && (!near(x_out, o) || (o.x - x_in).abs() <= (o.x - x_out).abs()) {
                Drag::In
            } else if near(x_out, o) {
                Drag::Out
            } else {
                Drag::Scrub(centre, o.x)
            }
        });
    }
    if resp.dragged() {
        if let (Some(d), Some(p)) = (ed.drag, resp.interact_pointer_pos()) {
            match d {
                Drag::In => ed.set_in(m.frame(p.x.clamp(rect.left(), rect.right()))),
                Drag::Out => ed.set_out(m.frame(p.x.clamp(rect.left(), rect.right()))),
                Drag::Scrub(f0, x0) => {
                    // Grab-the-tape: dragging right moves earlier in time.
                    let f = f0 as f64 - (p.x - x0) as f64 * fpp;
                    ed.playhead = f.round().clamp(0.0, frames.saturating_sub(1) as f64) as usize;
                    m.start = ed.playhead as f64 - half_w;
                    ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
                }
                _ => {}
            }
        }
    }
    if resp.drag_stopped() {
        if matches!(ed.drag, Some(Drag::Scrub(..))) && pv.playing {
            out.push(EdIntent::Seek(ed.playhead));
        }
        ed.drag = None;
    }
    if resp.clicked() {
        out.push(EdIntent::TookFocus);
        if let Some(p) = resp.interact_pointer_pos() {
            ed.playhead = m.frame(p.x).min(frames.saturating_sub(1));
            if pv.playing {
                out.push(EdIntent::Seek(ed.playhead));
            }
        }
    }

    // Recompute after the mouse may have moved things.
    let x_in = m.x(ed.sel.0 as f64);
    let x_out = m.x(ed.sel.1 as f64);

    // ---- paint ----
    let painter = ui.painter_at(rect);
    let vis = ui.visuals().clone();
    painter.rect_filled(rect, 2.0, vis.extreme_bg_color);
    let ruler = Rect::from_min_size(rect.min, vec2(rect.width(), RULER_H));
    let lanes = Rect::from_min_max(Pos2::new(rect.left(), ruler.bottom()), rect.max);

    // Hatch the parts of the window that fall outside the file.
    let x0 = m.x(0.0).clamp(rect.left(), rect.right());
    let x1 = m.x(frames as f64).clamp(rect.left(), rect.right());
    let void = vis.faint_bg_color.gamma_multiply(0.6);
    if x0 > rect.left() {
        painter.rect_filled(Rect::from_x_y_ranges(rect.left()..=x0, lanes.y_range()), 0.0, void);
    }
    if x1 < rect.right() {
        painter.rect_filled(Rect::from_x_y_ranges(x1..=rect.right(), lanes.y_range()), 0.0, void);
    }

    let reg = Rect::from_x_y_ranges(x_in.max(rect.left())..=x_out.min(rect.right()), lanes.y_range());
    if reg.width() > 0.0 {
        painter.rect_filled(reg, 0.0, vis.selection.bg_fill.gamma_multiply(0.12));
    }
    draw_ruler(&painter, &m, ruler, rate, &vis);
    draw_lanes(&painter, ed, &m, lanes, &vis);

    let dim = vis.extreme_bg_color.gamma_multiply(0.55);
    if x_in > rect.left() {
        painter.rect_filled(Rect::from_x_y_ranges(rect.left()..=x_in.min(rect.right()), lanes.y_range()), 0.0, dim);
    }
    if x_out < rect.right() {
        painter.rect_filled(Rect::from_x_y_ranges(x_out.max(rect.left())..=rect.right(), lanes.y_range()), 0.0, dim);
    }
    if ed.is_kit {
        slice_marks(&painter, &m, rect, 0.0, &ed.marks());
    }
    marker(&painter, rect, x_in, IN_COLOR, true);
    marker(&painter, rect, x_out, OUT_COLOR, false);

    // Work playhead (always centred) and the play cursor.
    let xp = m.x(ed.playhead as f64);
    if rect.x_range().contains(xp) {
        painter.line_segment([Pos2::new(xp, ruler.bottom()), Pos2::new(xp, rect.bottom())], Stroke::new(1.0, vis.strong_text_color().gamma_multiply(0.7)));
        let t = ruler.bottom();
        painter.add(egui::Shape::convex_polygon(
            vec![Pos2::new(xp - 5.0, t - 7.0), Pos2::new(xp + 5.0, t - 7.0), Pos2::new(xp, t)],
            vis.strong_text_color(),
            Stroke::NONE,
        ));
    }
    if pv.playing {
        let x = m.x(pv.pos as f64);
        if rect.x_range().contains(x) {
            painter.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())], Stroke::new(1.5, PLAY_COLOR));
        }
    }
    painter.text(
        Pos2::new(rect.right() - 4.0, ruler.bottom() + 2.0),
        Align2::RIGHT_TOP,
        "40 ms",
        FontId::proportional(10.0),
        vis.weak_text_color(),
    );
}

// ---- overview ---------------------------------------------------------------

fn overview(ui: &mut Ui, ed: &mut Editor, pv: &PlayView, rect: Rect, out: &mut Vec<EdIntent>) {
    let frames = ed.frames();
    if frames == 0 {
        return;
    }
    let vis = ui.visuals().clone();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, vis.extreme_bg_color);
    let fpp = frames as f64 / rect.width().max(1.0) as f64;
    let x = |f: f64| rect.left() + (f / fpp) as f32;

    // Region band.
    painter.rect_filled(
        Rect::from_x_y_ranges(x(ed.sel.0 as f64)..=x(ed.sel.1 as f64), rect.y_range()),
        0.0,
        vis.selection.bg_fill.gamma_multiply(0.25),
    );

    if let Some(cur) = &ed.current {
        let mid = rect.center().y;
        let half = rect.height() * 0.45;
        let col = vis.weak_text_color();
        for px in 0..rect.width() as usize {
            let f0 = (px as f64 * fpp) as usize;
            let f1 = (((px + 1) as f64 * fpp) as usize).max(f0 + 1);
            let mut hi = 0f32;
            for (data, peaks) in cur.sample.channels.iter().zip(cur.peaks.iter()) {
                let (lo, h) = peaks.range(data, f0, f1);
                hi = hi.max(h.abs()).max(lo.abs());
            }
            let xx = rect.left() + px as f32 + 0.5;
            painter.line_segment([Pos2::new(xx, mid - hi * half), Pos2::new(xx, mid + hi * half + 0.5)], Stroke::new(1.0, col));
        }
    }

    // Visible window.
    let vw = Rect::from_x_y_ranges(
        x(ed.view.start)..=x(ed.view.start + ed.view.fpp * ed.wave_rect.map_or(0.0, |r| r.width() as f64)),
        rect.y_range(),
    );
    painter.rect_stroke(vw, 2.0, Stroke::new(1.0, vis.strong_text_color()), egui::StrokeKind::Inside);
    if ed.is_kit {
        for f in ed.marks() {
            let xx = x(f as f64);
            painter.line_segment([Pos2::new(xx, rect.top()), Pos2::new(xx, rect.bottom())], Stroke::new(1.0, MARK_COLOR.gamma_multiply(0.7)));
        }
    }
    for (f, c) in [(ed.sel.0, IN_COLOR), (ed.sel.1, OUT_COLOR)] {
        let xx = x(f as f64);
        painter.line_segment([Pos2::new(xx, rect.top()), Pos2::new(xx, rect.bottom())], Stroke::new(1.0, c));
    }
    if pv.playing {
        let xx = x(pv.pos as f64);
        painter.line_segment([Pos2::new(xx, rect.top()), Pos2::new(xx, rect.bottom())], Stroke::new(1.0, PLAY_COLOR));
    }

    // Click or drag to centre the main view there.
    let resp = ui.interact(rect, ui.scope_id().with("overview"), Sense::click_and_drag());
    if resp.clicked() || resp.dragged() {
        out.push(EdIntent::TookFocus);
        if let Some(p) = resp.interact_pointer_pos() {
            let w = vw.width() as f64 * fpp;
            ed.view.start = (p.x - rect.left()) as f64 * fpp - w / 2.0;
        }
    }
    if resp.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::Grab);
    }
}

// ---- stats ------------------------------------------------------------------

fn stats_table(ui: &mut Ui, ed: &mut Editor) {
    let Some(s) = ed.sample().cloned() else { return };
    let gain = ed.gain();
    let r: Stats = ed.region_stats();
    let f: Stats = ed.file_stats();
    let dbv = |lin: f32| stats::fmt_db(stats::to_db(lin));
    let crest = |st: &Stats| {
        if st.rms > 0.0 { format!("{:.1} dB", stats::to_db(st.peak / st.rms)) } else { "—".to_owned() }
    };
    let len = |n: usize| format!("{:.3} s", s.seconds(n));

    egui::Grid::new("editor_stats").num_columns(3).spacing([18.0, 3.0]).striped(true).show(ui, |ui| {
        ui.label("");
        ui.label(RichText::new(if ed.mono_active() { "Region (mono, with gain)" } else { "Region (with gain)" }).weak());
        ui.label(RichText::new("File").weak());
        ui.end_row();

        ui.label("Length");
        ui.monospace(len(r.frames));
        ui.monospace(len(f.frames));
        ui.end_row();

        ui.label("Peak");
        let peak_after = r.peak * gain;
        let txt = RichText::new(dbv(peak_after)).monospace();
        ui.label(if peak_after >= 1.0 { txt.color(CLIP_COLOR) } else { txt });
        ui.monospace(dbv(f.peak));
        ui.end_row();

        ui.label("RMS");
        ui.monospace(dbv(r.rms * gain));
        ui.monospace(dbv(f.rms));
        ui.end_row();

        ui.label("Crest");
        ui.monospace(crest(&r));
        ui.monospace(crest(&f));
        ui.end_row();

        ui.label("DC offset");
        ui.monospace(format!("{:+.3}%", r.dc * gain * 100.0));
        ui.monospace(format!("{:+.3}%", f.dc * 100.0));
        ui.end_row();

        ui.label("Clipped");
        let txt = RichText::new(format!("{} samples", r.clipped)).monospace();
        ui.label(if r.clipped > 0 { txt.color(CLIP_COLOR) } else { txt });
        ui.label("");
        ui.end_row();
    });

    let ch = match s.channels.len() {
        1 => "mono".to_owned(),
        2 => "stereo".to_owned(),
        n => format!("{n} ch"),
    };
    let bits = s.bits.map(|b| format!(" · {b}-bit")).unwrap_or_default();
    ui.label(
        RichText::new(format!(
            "{} · {:.1} kHz{bits} · {ch} · {}",
            s.format,
            s.rate as f64 / 1000.0,
            crate::fs_model::human_bytes(s.file_len)
        ))
        .weak()
        .small(),
    );
}
