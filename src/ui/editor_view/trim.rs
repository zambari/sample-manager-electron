//! Trim section (kits only): cut every slice's tail where it drops below
//! the threshold, with a live readout of the sample memory that frees.

use super::fades::{DB_RANGE, SAVE_COLOR, threshold_strip};
use super::*;
use crate::editor::FadeEnd;

const BAR_W: f32 = 300.0;
const BAR_H: f32 = 4.0;

pub(super) fn trim_section(ui: &mut Ui, ed: &mut Editor, tails: &[FadeEnd], out: &mut Vec<EdIntent>) {
    section_header(ui, "Trim");
    let Some(rate) = ed.sample().map(|s| s.rate.max(1) as f64) else { return };
    let nch = ed.sample().map_or(1, |s| s.channels.len().max(1));

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

    ui.horizontal_wrapped(|ui| {
        ui.label("Cut below").on_hover_text(
            "Each slice keeps its sound up to the last moment it's louder than this, then a short fade, and the rest goes. \
             The slices close up (a grid kit gets shorter slots), so the kit takes less sample memory.\n\
             The strip glows green where a threshold frees more. Same threshold as Fades.\n\
             A slice that's quieter than this all the way through is left as it is.",
        );
        threshold_strip(ui, &mut ed.prefs.tail_db, tails, Some(&heat), Some(range));
        ui.add(
            egui::DragValue::new(&mut ed.prefs.tail_db)
                .range(DB_RANGE.0..=DB_RANGE.1)
                .speed(0.25)
                .max_decimals(1)
                .suffix(" dB"),
        );
        ui.label("fade");
        ui.add(
            egui::DragValue::new(&mut ed.prefs.trim_fade_ms)
                .range(0.0..=200.0)
                .speed(0.25)
                .max_decimals(0)
                .suffix(" ms"),
        )
        .on_hover_text("Fade-out after each cut, over sound that's already below the threshold");
    });

    let Some(plan) = ed.trim_plan(ed.prefs.tail_db) else { return };
    let saved = plan.saved();
    let secs = |f: usize| f as f64 / rate;
    // As the sampler stores it: 16-bit.
    let bytes = |f: usize| f * nch * 2;
    ui.horizontal_wrapped(|ui| {
        let label = if saved > 0 {
            RichText::new(format!("✂ Trim tails  −{:.2} s", secs(saved))).color(SAVE_COLOR)
        } else {
            RichText::new("Nothing to trim")
        };
        if ui
            .add_enabled(saved > 0, egui::Button::new(label))
            .on_hover_text("Cut every slice's tail as shaded on the wave. ⌘Z undoes.")
            .clicked()
        {
            out.push(EdIntent::TrimTails);
        }
        // Kept vs freed, to scale.
        let (rect, _) = ui.allocate_exact_size(vec2(BAR_W, 12.0), Sense::hover());
        let bar = Rect::from_center_size(rect.center(), vec2(BAR_W, BAR_H));
        let p = ui.painter_at(rect);
        let kept = plan.new_total as f32 / plan.total.max(1) as f32;
        let split = bar.left() + bar.width() * kept.min(1.0);
        p.rect_filled(Rect::from_x_y_ranges(bar.left()..=split, bar.y_range()), 0.0, ui.visuals().strong_text_color().gamma_multiply(0.6));
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
