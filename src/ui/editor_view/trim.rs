//! Trim section (kits only): cut every slice's tail where it drops below
//! the threshold, after a fade, with a live readout of the sample memory
//! that frees. The fades show on the wave, where each can be shaped.

use super::strip::{DB_RANGE, SAVE_COLOR, threshold_strip};
use super::*;
use crate::editor::SliceEnd;
use crate::ui::theme;

const BAR_W: f32 = 300.0;
const BAR_H: f32 = 4.0;

pub(super) fn trim_section(ui: &mut Ui, ed: &mut Editor, ends: &[SliceEnd], out: &mut Vec<EdIntent>) {
    section_header(ui, "Trim");
    let Some(rate) = ed.sample().map(|s| s.rate.max(1) as f64) else { return };
    let nch = ed.sample().map_or(1, |s| s.channels.len().max(1));

    let plan = ed.active_trim_plan();
    ui.horizontal_wrapped(|ui| {
        // Grid kits (a slot count or Auto): every slot is as long as the
        // longest sound, so one slot length is the quick way in.
        if ed.grid.is_some() {
            ui.selectable_value(&mut ed.prefs.grid_trim_by_level, false, "Length").on_hover_text(
                "Cut every slot to one length: drag the orange line on the wave (any slot). \
                 Sounds that run past it fade out just before it.",
            );
            ui.selectable_value(&mut ed.prefs.grid_trim_by_level, true, "Level").on_hover_text(
                "Cut each slice where its sound drops below a level. On a grid the longest slice still sets the slot length.",
            );
            ui.separator();
        }
        if ed.trims_by_length() {
            length_row(ui, ed, rate);
        } else {
            level_row(ui, ed, ends);
        }
        ui.label("fade");
        ui.add(
            egui::DragValue::new(&mut ed.prefs.trim_fade_ms)
                .range(1.0..=500.0)
                .speed(0.25)
                .max_decimals(0)
                .suffix(" ms"),
        )
        .on_hover_text(
            "Default fade before a slot-length cut, or after a level's threshold point. Drag a fade's knob on the wave to shape one: \
             sideways for length, up / down for the curve; double-click resets it.",
        );
        // Apply: cut and fade the kit in memory (one undo step); nothing
        // is written to disk until it's saved or sent.
        let (saved, nf) = plan.as_ref().map_or((0, 0), |p| (p.saved(), p.fades().len()));
        ui.separator();
        if ui
            .add_enabled(
                saved > 0 || nf > 0,
                egui::Button::new(RichText::new("Apply").color(theme::ON_ACCENT)).fill(theme::ACCENT),
            )
            .on_hover_text(format!(
                "Apply the cut and the {nf} fade{} shown on the wave to the kit in memory. ⌘Z undoes; nothing is written to disk.",
                if nf == 1 { "" } else { "s" }
            ))
            .clicked()
        {
            out.push(EdIntent::TrimTails);
        }
    });

    let Some(plan) = plan else { return };
    let saved = plan.saved();
    let nf = plan.fades().len();
    let secs = |f: usize| f as f64 / rate;
    // As the sampler stores it: 16-bit.
    let bytes = |f: usize| f * nch * 2;
    ui.horizontal_wrapped(|ui| {
        let label = if saved > 0 {
            RichText::new(format!("−{:.2} s", secs(saved))).color(SAVE_COLOR)
        } else if nf > 0 {
            RichText::new(format!("fades only: {nf}")).color(ui.visuals().warn_fg_color)
        } else {
            RichText::new("Nothing to trim").weak()
        };
        ui.label(label);
        // Kept vs freed, to scale.
        let (rect, _) = ui.allocate_exact_size(vec2(BAR_W, 12.0), Sense::hover());
        let bar = Rect::from_center_size(rect.center(), vec2(BAR_W, BAR_H));
        let p = ui.painter_at(rect);
        let kept = plan.new_total as f32 / plan.total.max(1) as f32;
        let split = bar.left() + bar.width() * kept.min(1.0);
        p.rect_filled(
            Rect::from_x_y_ranges(bar.left()..=split, bar.y_range()),
            0.0,
            ui.visuals().strong_text_color().gamma_multiply(0.6),
        );
        p.rect_filled(Rect::from_x_y_ranges(split..=bar.right(), bar.y_range()), 0.0, SAVE_COLOR);
        let pct = 100.0 * saved as f64 / plan.total.max(1) as f64;
        ui.label(
            RichText::new(format!(
                "{:.2} s → {:.2} s  ·  frees {pct:.0}%  ·  {} → {}",
                secs(plan.total),
                secs(plan.new_total),
                size(bytes(plan.total)),
                size(bytes(plan.new_total)),
            ))
            .monospace(),
        )
        .on_hover_text(format!("Sizes at 16-bit, {nch} channel{}", if nch == 1 { "" } else { "s" }));
    });
}

fn size(b: usize) -> String {
    if b >= 1 << 20 {
        format!("{:.2} MB", b as f64 / (1 << 20) as f64)
    } else {
        format!("{:.0} KB", b as f64 / 1024.0)
    }
}

/// Trim by level: the threshold over each slice's end level.
fn level_row(ui: &mut Ui, ed: &mut Editor, ends: &[SliceEnd]) {
    // Share of the kit a trim frees, every 0.5 dB, for the strip's glow and
    // its range: from where trimming starts to pay to where it peaks.
    let total = ed.trim_plan(0.0).map_or(0, |p| p.total).max(1);
    let steps: Vec<(f32, f32)> = (0..=((DB_RANGE.1 - DB_RANGE.0) * 2.0) as usize)
        .map(|i| {
            let d = DB_RANGE.0 + i as f32 * 0.5;
            (d, ed.trim_plan(d).map_or(0.0, |p| p.saved() as f32 / total as f32))
        })
        .collect();
    let base = steps.first().map_or(0.0, |s| s.1);
    let (top_db, top) = steps
        .iter()
        .copied()
        .fold((DB_RANGE.0, 0.0f32), |m, s| if s.1 > m.1 { s } else { m });
    let start = steps
        .iter()
        .find(|s| s.1 > base + 0.02 * (top - base))
        .map_or(DB_RANGE.0, |s| s.0);
    let mut range = ((start - 6.0).max(DB_RANGE.0), (top_db + 6.0).min(DB_RANGE.1));
    if range.1 - range.0 < 24.0 {
        range.0 = (range.1 - 24.0).max(DB_RANGE.0);
    }
    let heat = |d: f32| {
        let i = (((d - DB_RANGE.0) * 2.0).floor().max(0.0) as usize).min(steps.len() - 1);
        steps[i].1
    };

    ui.label("Cut below").on_hover_text(
        "Each slice keeps its sound up to the last moment it's louder than this. There a fade starts (orange on the wave), \
         and everything after the fade goes. The slices close up (a grid kit gets shorter slots), so the kit takes less sample memory.\n\
         A slice still louder than this where it stops (cut off) gets a fade over its end instead, and keeps its length.\n\
         A slice that's quieter than this all the way through is left as it is.\n\n\
         The strip glows green where a threshold frees more. Ticks: each slice's level where it stops — orange ones are cut off at this threshold.",
    );
    threshold_strip(ui, &mut ed.prefs.tail_db, ends, Some(&heat), Some(range));
    ui.add(
        egui::DragValue::new(&mut ed.prefs.tail_db)
            .range(DB_RANGE.0..=DB_RANGE.1)
            .speed(0.25)
            .max_decimals(1)
            .suffix(" dB"),
    );
}

/// Trim by length (grid kits): one slot length for every slot.
fn length_row(ui: &mut Ui, ed: &mut Editor, rate: f64) {
    let Some((len, sounds)) = ed.slot_sounds() else { return };
    let before = ed.slot_cut_len().unwrap_or(len);
    let mut cut = before;
    ui.label("Slot length").on_hover_text(
        "Every slot gets cut to this length, so the kit shrinks by the same amount per slot. \
         Sounds that run past it fade out over the fade time just before it; shorter ones stay as they are.\n\n\
         Drag the fader here or the orange line on the wave (any slot). Ticks: where each slot's sound ends — \
         orange ones get faded. Dragging snaps to them, so landing on the second-longest tick cuts only the longest sound.",
    );
    slot_strip(ui, &mut cut, len, &sounds, rate);
    if ui
        .add_enabled(cut < len, egui::Button::new("Full"))
        .on_hover_text("Back to the slot length as it is: nothing cut")
        .clicked()
    {
        cut = len;
    }
    if cut != before {
        ed.slot_cut = (cut < len).then_some(cut.clamp(1, len));
    }
}

/// Slot length over 0 … the slot as it is, with a tick where each slot's
/// sound ends. Green right of the line is what every slot loses.
fn slot_strip(ui: &mut Ui, cut: &mut usize, len: usize, sounds: &[usize], rate: f64) {
    const W: f32 = 300.0;
    const H: f32 = 18.0;
    const SNAP: f32 = 4.0;
    const CAP_W: f32 = 10.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(W, H), Sense::click_and_drag());
    let len_f = len.max(1) as f32;
    let x_of = |f: usize| rect.left() + f as f32 / len_f * rect.width();
    let f_of = |x: f32| (((x - rect.left()) / rect.width()).clamp(0.0, 1.0) * len_f) as usize;
    let ms = |f: usize| f as f64 * 1000.0 / rate;

    if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
        // Snap onto a sound's end when close: cutting right there frees
        // the most without touching that sound.
        *cut = sounds
            .iter()
            .copied()
            .filter(|&s| s > 0 && (x_of(s) - p.x).abs() <= SNAP)
            .min_by(|a, b| (x_of(*a) - p.x).abs().total_cmp(&(x_of(*b) - p.x).abs()))
            .unwrap_or_else(|| f_of(p.x));
    }
    if resp.hovered() {
        let (scroll, fine) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers.shift));
        let s = if scroll.y != 0.0 { scroll.y } else { scroll.x };
        if s != 0.0 {
            // About 1 % of the slot per wheel notch; ⇧ for a quarter.
            let d = s as f64 * len as f64 * if fine { 0.000_25 } else { 0.001 };
            *cut = (*cut as f64 + d).round().max(1.0) as usize;
            ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
        }
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
    }
    *cut = (*cut).clamp(1, len.max(1));

    let vis = ui.visuals().clone();
    let warn = vis.warn_fg_color;
    let p = ui.painter_at(rect.expand2(vec2(CAP_W / 2.0 + 1.0, 2.0)));
    let mid = rect.center().y;
    let strip = Rect::from_x_y_ranges(rect.x_range(), mid - BAR_H / 2.0..=mid + BAR_H / 2.0);
    let tx = x_of(*cut);
    p.rect_filled(strip, 0.0, vis.extreme_bg_color);
    p.rect_filled(Rect::from_x_y_ranges(tx..=strip.right(), strip.y_range()), 0.0, SAVE_COLOR);
    let pointer = resp.hover_pos();
    let mut near = Vec::new();
    for (i, &s) in sounds.iter().enumerate() {
        if s == 0 {
            continue;
        }
        let x = x_of(s);
        let col = if s > *cut { warn.gamma_multiply(0.9) } else { vis.strong_text_color().gamma_multiply(0.6) };
        p.line_segment([Pos2::new(x, strip.top() - 2.0), Pos2::new(x, strip.bottom() + 2.0)], Stroke::new(1.5, col));
        if pointer.is_some_and(|q| (q.x - x).abs() <= SNAP) {
            near.push((i, s));
        }
    }
    // Fader cap: a small rounded block over the strip, with a groove
    // across its middle marking the exact cut.
    let hot = resp.hovered() || resp.dragged();
    let cap = Rect::from_center_size(Pos2::new(tx, mid), vec2(CAP_W, H));
    let fill = if hot { theme::ACCENT_BRIGHT } else { theme::ACCENT };
    p.rect_filled(cap.translate(vec2(0.0, 1.0)), 2.0, Color32::from_black_alpha(90));
    p.rect_filled(cap, 2.0, fill);
    p.rect_stroke(cap, 2.0, Stroke::new(1.0, theme::ACCENT_BRIGHT), egui::StrokeKind::Inside);
    for dx in [-2.0, 2.0] {
        p.line_segment(
            [Pos2::new(tx + dx, cap.top() + 4.0), Pos2::new(tx + dx, cap.bottom() - 4.0)],
            Stroke::new(1.0, theme::ON_ACCENT.gamma_multiply(0.35)),
        );
    }
    p.line_segment([Pos2::new(tx, cap.top() + 2.0), Pos2::new(tx, cap.bottom() - 2.0)], Stroke::new(1.0, theme::ON_ACCENT));

    if let Some(q) = pointer {
        let f = f_of(q.x);
        let mut text = format!(
            "{:.1} ms   (slot now {:.1} ms)\ncut here saves {:.1} ms per slot",
            ms(f),
            ms(len),
            ms(len - f.min(len)),
        );
        for (i, s) in near {
            text += &format!("\nslot {}: sound ends at {:.1} ms", i + 1, ms(s));
        }
        resp.on_hover_text_at_pointer(text);
    }
}
