//! Fades section (kits only): the threshold for cut-off slice ends, a strip
//! showing where each slice's end level sits against it, and the button
//! that applies the fades.

use super::*;
use crate::editor::FadeEnd;

const STRIP_W: f32 = 300.0;
/// The level strip itself; the widget around it is taller to grab easily.
const STRIP_H: f32 = 4.0;
const WIDGET_H: f32 = 18.0;
/// Freed sample memory.
pub(super) const SAVE_COLOR: Color32 = Color32::from_rgb(110, 210, 140);
/// Threshold limits, dBFS.
pub(super) const DB_RANGE: (f32, f32) = (-96.0, 0.0);
/// The strip never shows less than this many dB.
const MIN_SPAN: f32 = 24.0;

pub(super) fn fades_section(
    ui: &mut Ui,
    ed: &mut Editor,
    tails: &[FadeEnd],
    out: &mut Vec<EdIntent>,
) {
    section_header(ui, "Fades");
    ui.horizontal_wrapped(|ui| {
        ui.label("Cut off above").on_hover_text(
            "A slice whose very end is louder than this stops while still sounding (it clicks) and gets an orange fade on the wave.\n\
             The strip has one tick per slice at its end level: orange ticks get a fade, grey ones are left alone, faint ones already die away.\n\
             Put the line in the gap left of the loud cluster. Drag or scroll on the strip.",
        );
        threshold_strip(ui, &mut ed.prefs.tail_db, tails, None, None);
        ui.add(
            egui::DragValue::new(&mut ed.prefs.tail_db)
                .range(DB_RANGE.0..=DB_RANGE.1)
                .speed(0.25)
                .max_decimals(1)
                .suffix(" dB"),
        );
        let db = ed.prefs.tail_db;
        let n = tails.iter().filter(|t| t.abrupt(db)).count();
        ui.separator();
        let label = match n {
            0 => RichText::new("Nothing to fade"),
            1 => RichText::new("Apply 1 fade").color(ui.visuals().warn_fg_color),
            n => RichText::new(format!("Apply {n} fades")).color(ui.visuals().warn_fg_color),
        };
        if ui
            .add_enabled(n > 0, egui::Button::new(label))
            .on_hover_text(
                "Fade out every orange curve on the wave, as shaped. By default the last 100 ms (a fifth of the slice when it's under 0.5 s).\n\
                 Drag a curve's knob first to shape it: sideways for length, up / down for the curve. Double-click a knob to apply just that one. ⌘Z undoes.",
            )
            .clicked()
        {
            out.push(EdIntent::FadeEnds(None));
        }
        ui.label(RichText::new(format!("of {} slices", tails.len())).weak());
    });
}

/// dB range the strip spans: around the slice end levels that a threshold
/// can actually flag (ones that already decay never are), so the useful
/// part fills the width.
fn strip_range(tails: &[FadeEnd], cur: f32) -> (f32, f32) {
    let cut: Vec<f32> = tails.iter().filter(|t| !t.decays).map(|t| t.end_db).collect();
    let pool = if cut.is_empty() {
        tails.iter().map(|t| t.end_db).collect()
    } else {
        cut
    };
    if pool.is_empty() {
        return DB_RANGE;
    }
    let lo = pool.iter().copied().fold(f32::MAX, f32::min).min(cur);
    let hi = pool.iter().copied().fold(f32::MIN, f32::max).max(cur);
    let mut lo = ((lo - 6.0) / 6.0).floor() * 6.0;
    let hi = (((hi + 3.0) / 6.0).ceil() * 6.0).min(DB_RANGE.1);
    if hi - lo < MIN_SPAN {
        lo = hi - MIN_SPAN;
    }
    (lo.max(DB_RANGE.0), hi)
}

/// The threshold over a strip of slice end levels. With `heat` (dB →
/// share of the kit a trim there frees), the strip also glows green where
/// a threshold would free more.
pub(super) fn threshold_strip(
    ui: &mut Ui,
    db: &mut f32,
    tails: &[FadeEnd],
    heat: Option<&dyn Fn(f32) -> f32>,
    range: Option<(f32, f32)>,
) {
    let (rect, resp) = ui.allocate_exact_size(vec2(STRIP_W, WIDGET_H), Sense::click_and_drag());
    let (lo, hi) = match range {
        Some((lo, hi)) => (lo.min(*db - 3.0).max(DB_RANGE.0), hi.max(*db + 3.0).min(DB_RANGE.1)),
        None => strip_range(tails, *db),
    };
    let x_of = |d: f32| rect.left() + (d - lo) / (hi - lo) * rect.width();
    let db_of = |x: f32| lo + (x - rect.left()) / rect.width() * (hi - lo);

    // Drag / click to set, scroll to nudge (⇧ for fine).
    if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.dragged() || resp.clicked()) {
        *db = (db_of(p.x) * 2.0).round() / 2.0;
    }
    if resp.hovered() {
        let (scroll, fine) = ui.input(|i| (i.smooth_scroll_delta, i.modifiers.shift));
        let s = if scroll.y != 0.0 { scroll.y } else { scroll.x };
        if s != 0.0 {
            // About 1 dB per wheel notch; ⇧ for a quarter of that.
            *db += s * if fine { 0.0125 } else { 0.05 };
            ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
        }
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
    }
    *db = db.clamp(DB_RANGE.0, DB_RANGE.1);

    let vis = ui.visuals().clone();
    let warn = vis.warn_fg_color;
    let p = ui.painter_at(rect.expand(1.0));
    let mid = rect.center().y;
    let strip = Rect::from_x_y_ranges(rect.x_range(), mid - STRIP_H / 2.0..=mid + STRIP_H / 2.0);
    let tx = x_of(*db);

    p.rect_filled(strip, 0.0, vis.extreme_bg_color);
    if let Some(heat) = heat {
        // Brighter where a threshold frees more of the kit.
        for px in 0..rect.width() as usize {
            let x = rect.left() + px as f32;
            let f = heat(db_of(x + 0.5)).clamp(0.0, 1.0);
            if f > 0.0 {
                p.line_segment(
                    [Pos2::new(x + 0.5, strip.top()), Pos2::new(x + 0.5, strip.bottom())],
                    Stroke::new(1.0, SAVE_COLOR.gamma_multiply(0.15 + 0.85 * f)),
                );
            }
        }
    } else {
        // What gets faded: everything right of the line.
        p.rect_filled(
            Rect::from_x_y_ranges(tx..=strip.right(), strip.y_range()),
            0.0,
            warn.gamma_multiply(0.18),
        );
    }
    // A dot every 6 dB under the strip, a longer one every 24.
    let mut d = (lo / 6.0).ceil() * 6.0;
    while d <= hi {
        let x = x_of(d);
        let len = if (d / 24.0).fract() == 0.0 { 3.0 } else { 1.0 };
        p.line_segment(
            [Pos2::new(x, strip.bottom() + 2.0), Pos2::new(x, strip.bottom() + 2.0 + len)],
            Stroke::new(1.0, vis.weak_text_color()),
        );
        d += 6.0;
    }
    // One tick per slice at its end level. Overlapping ticks add up, so
    // a cluster reads brighter.
    let pointer = resp.hover_pos();
    let mut near = Vec::new();
    for t in tails {
        let x = x_of(t.end_db.clamp(lo, hi));
        let (col, a) = if t.decays {
            (vis.weak_text_color(), 0.35)
        } else if t.abrupt(*db) {
            (warn, 0.85)
        } else {
            (vis.strong_text_color(), 0.6)
        };
        let grow = if t.decays { 0.0 } else { 2.0 };
        p.line_segment(
            [Pos2::new(x, strip.top() - grow), Pos2::new(x, strip.bottom() + grow)],
            Stroke::new(1.5, col.gamma_multiply(a)),
        );
        if pointer.is_some_and(|q| (q.x - x).abs() <= 3.0) {
            near.push(*t);
        }
    }
    // The threshold line, with a notch on top.
    p.line_segment(
        [Pos2::new(tx, rect.top() + 3.0), Pos2::new(tx, rect.bottom())],
        Stroke::new(1.5, vis.strong_text_color()),
    );
    p.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(tx - 4.0, rect.top()),
            Pos2::new(tx + 4.0, rect.top()),
            Pos2::new(tx, rect.top() + 4.0),
        ],
        vis.strong_text_color(),
        Stroke::NONE,
    ));

    if let Some(q) = pointer {
        let mut text = format!("{:.1} dB   (strip {lo:.0} … {hi:.0} dB)", db_of(q.x));
        if let Some(heat) = heat {
            text += &format!("\ntrim here frees {:.0}% of the kit", heat(db_of(q.x)) * 100.0);
        }
        near.sort_by(|a, b| b.end_db.total_cmp(&a.end_db));
        for t in &near {
            let what = if t.decays {
                "dies away on its own"
            } else if t.abrupt(*db) {
                "cut off — gets a fade"
            } else {
                "below the line — left alone"
            };
            text += &format!("\nslice {}: ends at {:.1} dB, {what}", t.slice + 1, t.end_db);
        }
        resp.on_hover_text_at_pointer(text);
    }
}
