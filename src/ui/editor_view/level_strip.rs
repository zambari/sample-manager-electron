//! The Histogram panel: how many frames of the region sit at each level once
//! the gain is applied, so you can see how much of the audio a gain push
//! drives into the limiter (amber) or through it (red), and how much is
//! still down in the quiet end. Beside it, a black box of stats for nerds:
//! on the left, nearly silent lead-in / tail, peak, RMS and the energy the limiter takes.
//!
//! The histogram and the lead / tail scan are cached in the editor (see
//! `editor::level_hist`), so the per-frame repaints during playback only
//! redraw; only moving the in/out points, an edit or a crop rescans.

use super::*;
use crate::audio::limit::{Level, Limiter};
use crate::editor::level_hist::{BINS, LevelHist};
use crate::ui::theme::{TINY, tiny_pick, tiny_row};

/// Right edge of the strip, dB after gain: room to see the limiter work.
const HI_DB: f32 = 12.0;
const STATS_W: f32 = 210.0;
const MIN_HIST_W: f32 = 120.0;
/// Fits the stats box: header, two silence rows, pk/rms, lim.
const H: f32 = 58.0;
const LINE_H: f32 = 11.0;

fn pct(n: u64, total: u64) -> String {
    if n == 0 || total == 0 {
        "0%".into()
    } else {
        let p = n as f64 * 100.0 / total as f64;
        if p < 0.01 { "<0.01%".into() } else if p < 10.0 { format!("{p:.2}%") } else { format!("{p:.1}%") }
    }
}

fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn db_str(d: f32) -> String {
    if d.is_finite() { format!("{d:.1}") } else { "-inf".into() }
}

pub(super) fn histogram_panel(ui: &mut Ui, ed: &mut Editor) {
    section_header(ui, "Histogram");
    let hp = &mut ed.prefs.hist;
    tiny_row(ui, |ui| {
        tiny_pick(ui, "bucket", &mut hp.bucket_db, &[(1.0, "1 dB"), (2.0, "2 dB"), (3.0, "3 dB"), (6.0, "6 dB")]);
        tiny_pick(ui, "floor", &mut hp.floor_db, &[(-48.0, "-48"), (-60.0, "-60"), (-72.0, "-72"), (-96.0, "-96")]);
        tiny_pick(ui, "zero≈", &mut hp.zero_db, &[(-36.0, "-36"), (-48.0, "-48"), (-60.0, "-60"), (-72.0, "-72"), (-90.0, "-90")])
            ;
        tiny_pick(ui, "zero≈2", &mut hp.zero2_db, &[(-18.0, "-18"), (-24.0, "-24"), (-30.0, "-30"), (-36.0, "-36"), (-42.0, "-42")]);
        tiny_pick(ui, "scale", &mut hp.log, &[(true, "log"), (false, "lin")]);
    });
    let hp = ed.prefs.hist;
    let Some((h, db)) = ed.level_hist() else {
        ui.label(RichText::new("no audio").size(TINY).weak());
        return;
    };
    let edges = ed.edges(hp.zero_db);
    let edges2 = ed.edges(hp.zero2_db);

    let g = stats::from_db(db);
    let lim = Limiter::new(g);
    let zero_lin = stats::from_db(hp.zero_db);
    let floor = hp.floor_db;
    let bucket = hp.bucket_db.max(0.5);
    let cols = ((HI_DB - floor) / bucket).ceil().max(1.0) as usize;
    let mut col = vec![0f64; cols];
    let (mut limited, mut clipped, mut quiet) = (0u64, 0u64, 0u64);
    let (mut e_in, mut e_out, mut peak) = (0f64, 0f64, 0f32);
    for k in 0..BINS {
        let n = h.counts[k];
        if n == 0 {
            continue;
        }
        if k == 0 {
            quiet += n; // silence
            col[0] += n as f64;
            continue;
        }
        let x = LevelHist::center(k) * g;
        let y = lim.shape(x);
        e_in += n as f64 * (x as f64).powi(2);
        e_out += n as f64 * (y as f64).powi(2);
        peak = peak.max(y.abs());
        match lim.level(x) {
            Level::Clipped => clipped += n,
            Level::Limited => limited += n,
            Level::Clean => {}
        }
        if x < zero_lin {
            quiet += n;
        }
        // Spread the bin over the columns it overlaps, so a bucket that
        // isn't a whole number of bins doesn't comb.
        let (lo, hi) = LevelHist::bounds(k);
        let u0 = (stats::to_db(lo * g) - floor) / bucket;
        let u1 = (stats::to_db(hi * g) - floor) / bucket;
        let top = cols as f32;
        if u1 <= 0.0 || !u1.is_finite() {
            col[0] += n as f64;
        } else if u0 >= top {
            col[cols - 1] += n as f64;
        } else {
            let (u0, u1) = (u0.max(0.0), u1.min(top));
            let span = (u1 - u0).max(1e-6);
            let mut c = u0.floor() as usize;
            while c < cols && (c as f32) < u1 {
                let ov = (u1.min(c as f32 + 1.0) - u0.max(c as f32)).max(0.0);
                col[c] += n as f64 * (ov / span) as f64;
                c += 1;
            }
        }
    }

    let full = ui.available_width();
    let hist_w = (full - STATS_W - 6.0).max(MIN_HIST_W);
    let region = if ed.is_kit { "selection" } else { "in/out region" };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;

        // Stats for nerds, on the left.
        let (rect, resp) = ui.allocate_exact_size(vec2(STATS_W, H), Sense::hover());
        if ui.is_rect_visible(rect) {
            let p = ui.painter_at(rect);
            p.rect_filled(rect, 0.0, Color32::BLACK);
            let font = FontId::monospace(TINY);
            let dim = Color32::from_gray(110);
            let txt = Color32::from_gray(205);
            let total = h.total.max(1);
            let rms = if h.total > 0 { (e_out / h.total as f64).sqrt() as f32 } else { 0.0 };
            let loss = if e_out > 0.0 && e_in > 0.0 { 10.0 * (e_in / e_out).log10() } else { 0.0 };
            let row = |a: String, b: String| format!("{a:>12} {b:>12}");
            let mut lines: Vec<(String, String, Color32)> = vec![
                (String::new(), row("lead".into(), "tail".into()), dim),
                (
                    format!("<{:.0}", hp.zero_db),
                    row(pct(edges.lead_quiet as u64, total), pct(edges.tail_quiet as u64, total)),
                    txt,
                ),
                (
                    format!("<{:.0}", hp.zero2_db),
                    row(pct(edges2.lead_quiet as u64, total), pct(edges2.tail_quiet as u64, total)),
                    txt,
                ),
                ("pk/rms".into(), format!("{} / {} dB", db_str(stats::to_db(peak)), db_str(stats::to_db(rms))), txt),
            ];
            // Only while the limiter is actually taking something off.
            if loss >= 0.0005 {
                lines.push(("lim".into(), format!("-{loss:.3} dB energy"), if loss > 0.1 { LIMIT_COLOR } else { txt }));
            }
            let lh = LINE_H;
            for (i, (k, v, c)) in lines.iter().enumerate() {
                let y = rect.top() + 2.0 + i as f32 * lh;
                p.text(Pos2::new(rect.left() + 4.0, y), Align2::LEFT_TOP, k, font.clone(), dim);
                p.text(Pos2::new(rect.left() + 40.0, y), Align2::LEFT_TOP, v, font.clone(), *c);
            }
        }
        resp.on_hover_text(format!(
            "<{:.0} / <{:.0}: share of the {region} before the first / after the last frame reaching that level, \
             after gain (the zero≈ and zero≈2 picks).\n\
             pk/rms: peak and RMS after gain and limiter (from the histogram, ~0.2 dB).\n\
             lim (shown while the limiter works): energy it takes off, compared with plain gain.",
            hp.zero_db,
            hp.zero2_db,
        ));

        let (rect, resp) = ui.allocate_exact_size(vec2(hist_w, H), Sense::hover());
        if ui.is_rect_visible(rect) {
            draw_hist(ui, rect, &col, floor, bucket, &lim, zero_lin, hp.log);
            let p = ui.painter_at(rect);
            let font = FontId::monospace(TINY);
            let mut tp = Pos2::new(rect.left() + 3.0, rect.top() + 1.0);
            let r = p.text(tp, Align2::LEFT_TOP, format!("quiet {}", pct(quiet, h.total)), font.clone(), ui.visuals().weak_text_color());
            tp.x = r.right() + 8.0;
            if limited > 0 {
                let r = p.text(tp, Align2::LEFT_TOP, format!("limited {}", pct(limited, h.total)), font.clone(), LIMIT_COLOR);
                tp.x = r.right() + 8.0;
            }
            if clipped > 0 {
                p.text(tp, Align2::LEFT_TOP, format!("clipped {}", pct(clipped, h.total)), font, CLIP_COLOR);
            }
        }
        // Hovering a bar: what's in it.
        if let Some(pos) = resp.hover_pos() {
            let c = (((pos.x - rect.left()) / rect.width() * (HI_DB - floor)) / bucket).floor();
            let c = (c.max(0.0) as usize).min(cols - 1);
            let n = col[c].round() as u64;
            let lo = floor + c as f32 * bucket;
            let hi = (lo + bucket).min(HI_DB);
            let range = if c == 0 {
                format!("below {hi:.0} dB")
            } else if c == cols - 1 {
                format!("above {lo:.0} dB")
            } else {
                format!("between {lo:.0} and {hi:.0} dB")
            };
            // Highlight the hovered column.
            let cw = rect.width() * bucket / (HI_DB - floor);
            let x0 = rect.left() + c as f32 * cw;
            ui.painter_at(rect).rect_stroke(
                Rect::from_min_max(Pos2::new(x0, rect.top()), Pos2::new(x0 + cw, rect.bottom())),
                0.0,
                Stroke::new(1.0, ui.visuals().strong_text_color()),
                egui::StrokeKind::Inside,
            );
            resp.on_hover_text(format!(
                "{} frames ({}) of the {region} peak {range}, gain applied.\n\
                 (Per-frame sample level of the loudest channel, not RMS.)",
                group(n),
                pct(n, h.total),
            ));
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn draw_hist(ui: &Ui, rect: Rect, col: &[f64], floor: f32, bucket: f32, lim: &Limiter, zero_lin: f32, log: bool) {
    let p = ui.painter_at(rect);
    let vis = ui.visuals();
    p.rect_filled(rect, 0.0, vis.extreme_bg_color);
    let x_of = |d: f32| rect.left() + (d - floor) / (HI_DB - floor) * rect.width();
    let base = rect.bottom();
    let top = rect.top() + 12.0;
    let mut t = 0.0 - 12.0 * ((0.0 - floor) / 12.0).floor();
    while t <= HI_DB {
        let c = if t == 0.0 { vis.text_color() } else { vis.weak_text_color().gamma_multiply(0.5) };
        p.line_segment([Pos2::new(x_of(t), top), Pos2::new(x_of(t), base)], Stroke::new(1.0, c));
        t += 12.0;
    }
    p.text(Pos2::new(x_of(0.0) + 2.0, base - 1.0), Align2::LEFT_BOTTOM, "0", FontId::monospace(TINY), vis.weak_text_color());
    let max = col.iter().copied().fold(0f64, f64::max);
    if max <= 0.0 {
        return;
    }
    let scale = |n: f64| if log { ((1.0 + n).ln() / (1.0 + max).ln()) as f32 } else { (n / max) as f32 };
    let cw = (bucket / (HI_DB - floor)) * rect.width();
    for (i, &n) in col.iter().enumerate() {
        if n < 0.5 {
            continue;
        }
        let d = floor + (i as f32 + 0.5) * bucket;
        let x = stats::from_db(d);
        let color = match lim.level(x) {
            Level::Clipped => CLIP_COLOR,
            Level::Limited => LIMIT_COLOR,
            Level::Clean if x < zero_lin => crate::ui::theme::WAVE.gamma_multiply(0.45),
            Level::Clean => crate::ui::theme::WAVE,
        };
        let hgt = (scale(n) * (base - top)).max(1.0);
        let x0 = rect.left() + i as f32 * cw;
        let x1 = (x0 + cw - 1.0).max(x0 + 1.0).min(rect.right());
        p.rect_filled(Rect::from_min_max(Pos2::new(x0, base - hgt), Pos2::new(x1, base)), 0.0, color);
    }
}
