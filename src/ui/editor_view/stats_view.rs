//! The Stats panel under the editor: phase meter on top, then three columns:
//! levels (region vs file), loudness (LUFS / true peak), and an isometric
//! waterfall of the spectrum: the region at rest, a rolling one while
//! playing. Analysis only runs while this panel is drawn (see
//! [`crate::editor::Editor::region_loudness`]).

use super::*;
use crate::audio::spectrum::{self, FLOOR_DB};
use crate::editor::{LoudnessView, WATERFALL_BANDS};

/// Height of the three-column block (not counting the phase meter).
pub(super) const STATS_H: f32 = 150.0;
/// Narrower than this, the waterfall is left out.
const MIN_WATERFALL_W: f32 = 140.0;
/// Waterfall level range, dBFS after gain and tilt.
const WF_TOP_DB: f32 = 0.0;
const WF_BOTTOM_DB: f32 = -84.0;

pub(super) fn stats_panel(ui: &mut Ui, ed: &mut Editor, pv: &PlayView) {
    section_header(ui, "Stats");
    if ed.is_multichannel() {
        phase_meter(ui, ed, pv);
    }
    let w = ui.available_width();
    ui.allocate_ui_with_layout(
        vec2(w, STATS_H),
        egui::Layout::left_to_right(egui::Align::Min),
        |ui| {
            ui.set_min_height(STATS_H);
            ui.vertical(|ui| levels_table(ui, ed));
            ui.separator();
            ui.vertical(|ui| loudness_table(ui, ed));
            ui.separator();
            let ww = ui.available_width();
            if ww >= MIN_WATERFALL_W {
                let (rect, resp) = ui.allocate_exact_size(vec2(ww, STATS_H), Sense::hover());
                let live = pv.playing;
                let rows: Vec<Vec<f32>> = if live {
                    ed.live_waterfall(pv.pos)
                } else {
                    ed.region_waterfall().map(|r| r.to_vec()).unwrap_or_default()
                };
                let rate = ed.sample().map_or(48_000, |s| s.rate);
                waterfall(ui, rect, &resp, &rows, ed.gain_db, rate, live);
            }
        },
    );
}

// ---- levels -----------------------------------------------------------------

fn levels_table(ui: &mut Ui, ed: &mut Editor) {
    let Some(s) = ed.sample().cloned() else {
        return;
    };
    let gain = ed.gain();
    let r: Stats = ed.region_stats();
    let f: Stats = ed.file_stats();
    let dbv = |lin: f32| stats::fmt_db(stats::to_db(lin));
    let crest = |st: &Stats| {
        if st.rms > 0.0 {
            format!("{:.1} dB", stats::to_db(st.peak / st.rms))
        } else {
            "—".to_owned()
        }
    };
    let len = |n: usize| format!("{:.3} s", s.seconds(n));

    egui::Grid::new("editor_stats")
        .num_columns(3)
        .spacing([18.0, 3.0])
        .striped(true)
        .show(ui, |ui| {
            ui.label("");
            ui.label(
                RichText::new(if ed.mono_active() {
                    "Region (mono, with gain)"
                } else {
                    "Region (with gain)"
                })
                .weak(),
            );
            ui.label(RichText::new("File").weak());
            ui.end_row();

            ui.label("Length");
            ui.monospace(len(r.frames));
            ui.monospace(len(f.frames));
            ui.end_row();

            ui.label("Peak");
            let peak_after = r.peak * gain;
            let txt = RichText::new(dbv(peak_after)).monospace();
            ui.label(if peak_after >= 1.0 {
                txt.color(CLIP_COLOR)
            } else {
                txt
            });
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
            ui.label(if r.clipped > 0 {
                txt.color(CLIP_COLOR)
            } else {
                txt
            });
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

// ---- loudness ---------------------------------------------------------------

fn loudness_table(ui: &mut Ui, ed: &mut Editor) {
    let LoudnessView { value, pending } = ed.region_loudness();
    let g = ed.gain_db;
    let warn = ui.visuals().warn_fg_color;
    // A stale reading (the region moved, the new one is on its way) is dimmed.
    let cell = |text: String| {
        let t = RichText::new(text).monospace();
        if pending { t.weak() } else { t }
    };
    let lufs = |v: Option<f32>| v.map_or("—".to_owned(), |v| format!("{:.1} LUFS", v + g));

    egui::Grid::new("editor_loudness")
        .num_columns(2)
        .spacing([14.0, 3.0])
        .striped(true)
        .show(ui, |ui| {
            ui.label("");
            ui.label(RichText::new(if pending { "Loudness …" } else { "Loudness" }).weak())
                .on_hover_text("BS.1770 / EBU R128, over the region, with gain.");
            ui.end_row();

            let Some(l) = value else {
                ui.label("");
                ui.label(RichText::new(if pending { "measuring" } else { "—" }).weak());
                ui.end_row();
                return;
            };

            ui.label("Integrated").on_hover_text(
                "Gated average loudness of the region. Prefixed ≈ when the region is \
                 shorter than one 400 ms block and was measured as a single block.",
            );
            let approx = if l.short { "≈" } else { "" };
            ui.label(cell(format!("{approx}{}", lufs(l.integrated))));
            ui.end_row();

            // Needs a region of 3 s or more; left out otherwise.
            if let Some(st) = l.short_term_max {
                ui.label("Short-term").on_hover_text("Loudest 3 s window.");
                ui.label(cell(lufs(Some(st))));
                ui.end_row();
            }

            ui.label("Momentary").on_hover_text("Loudest 400 ms window.");
            ui.label(cell(lufs(l.momentary_max)));
            ui.end_row();

            // Needs several 3 s windows; left out otherwise.
            if let Some(lra) = l.lra {
                ui.label("Range").on_hover_text(
                    "Loudness range (LRA): spread of the short-term loudness, 10th to 95th percentile.",
                );
                ui.label(cell(format!("{lra:.1} LU")));
                ui.end_row();
            }

            let tp = stats::to_db(l.true_peak) + g;
            ui.label("True peak").on_hover_text(
                "Estimated peak between samples (4× oversampled): what a DAC or a \
                 lossy encoder may see. Above −1 dBTP risks clipping downstream.",
            );
            let txt = if tp.is_finite() { format!("{tp:+.1} dBTP") } else { "−∞ dBTP".to_owned() };
            let t = cell(txt);
            ui.label(if tp >= 0.0 {
                t.color(CLIP_COLOR)
            } else if tp > -1.0 {
                t.color(warn)
            } else {
                t
            });
            ui.end_row();

            ui.label("PLR").on_hover_text("Peak to loudness ratio: true peak minus integrated. Low means heavily limited.");
            let plr = l.integrated.filter(|_| tp.is_finite()).map(|i| tp - (i + g));
            ui.label(cell(plr.map_or("—".to_owned(), |v| format!("{v:.1} LU"))));
            ui.end_row();
        });
}

// ---- phase ------------------------------------------------------------------

/// L/R correlation bar: −1 (cancels in mono) … 0 (unrelated) … +1 (mono).
/// Live around the play cursor while playing, else over the region.
fn phase_meter(ui: &mut Ui, ed: &mut Editor, pv: &PlayView) {
    let live = pv.playing;
    let ph = if live {
        ed.phase_at(pv.pos)
    } else {
        ed.region_phase()
    };
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

// ---- waterfall --------------------------------------------------------------

/// Isometric waterfall: one spectrum per row, oldest at the back (up and to
/// the right), newest at the front. Frequency runs left to right on a warped
/// axis (see [`spectrum::axis_pos`]); height and colour are level.
fn waterfall(
    ui: &Ui,
    rect: Rect,
    resp: &egui::Response,
    rows: &[Vec<f32>],
    gain_db: f32,
    rate: u32,
    live: bool,
) {
    let vis = ui.visuals();
    let bg = vis.extreme_bg_color;
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, bg);

    let f_max = rate as f32 / 2.0;
    let label_h = 11.0;
    let plot = Rect::from_min_max(rect.min + vec2(4.0, 4.0), Pos2::new(rect.right() - 4.0, rect.bottom() - label_h));
    let depth_x = plot.width() * 0.28;
    let depth_y = plot.height() * 0.5;
    let amp_h = plot.height() - depth_y;
    let plane_w = plot.width() - depth_x;
    let total = crate::editor::WATERFALL_ROWS.max(rows.len()).max(2);

    let level = |db: f32| {
        if db <= FLOOR_DB {
            0.0
        } else {
            ((db + gain_db - WF_BOTTOM_DB) / (WF_TOP_DB - WF_BOTTOM_DB)).clamp(0.0, 1.0)
        }
    };
    let bands = WATERFALL_BANDS;
    let band_x = |j: usize| (j as f32 + 0.5) / bands as f32 * plane_w;

    for (r, row) in rows.iter().enumerate() {
        // 0 at the front row, 1 at the back.
        let back = (rows.len() - 1 - r) as f32 / (total - 1) as f32;
        let origin = Pos2::new(plot.left() + depth_x * back, plot.bottom() - depth_y * back);
        // Older rows sink into the dark background quickly.
        let fade = (1.0 - back).powf(2.5);
        let mut mesh = egui::Mesh::default();
        let mut line = Vec::with_capacity(row.len());
        for (j, &db) in row.iter().enumerate() {
            let lv = level(db);
            let x = origin.x + band_x(j);
            let top = Pos2::new(x, origin.y - lv * amp_h);
            let col = lerp_color(bg, heat(lv), fade);
            let i = mesh.vertices.len() as u32;
            mesh.colored_vertex(Pos2::new(x, origin.y), bg);
            mesh.colored_vertex(top, col);
            if j > 0 {
                mesh.add_triangle(i - 2, i - 1, i);
                mesh.add_triangle(i - 1, i, i + 1);
            }
            line.push(top);
        }
        p.add(mesh);
        let edge = lerp_color(bg, theme_wave(), 0.06 + 0.7 * fade);
        p.add(egui::Shape::line(line, Stroke::new(1.0, edge)));
    }

    // Frequency ticks along the front baseline.
    let weak = vis.weak_text_color();
    let font = FontId::proportional(8.5);
    for (f, label) in [
        (50.0, None),
        (100.0, Some("100")),
        (200.0, None),
        (500.0, None),
        (1000.0, Some("1k")),
        (2000.0, None),
        (5000.0, None),
        (10_000.0, Some("10k")),
        (20_000.0, None),
    ] {
        if f >= f_max {
            continue;
        }
        let x = plot.left() + spectrum::axis_pos(f, f_max) * plane_w;
        p.line_segment([Pos2::new(x, plot.bottom()), Pos2::new(x, plot.bottom() + 3.0)], Stroke::new(1.0, weak));
        if let Some(t) = label {
            p.text(Pos2::new(x, plot.bottom() + 3.0), Align2::CENTER_TOP, t, font.clone(), weak);
        }
    }
    p.text(
        rect.right_top() + vec2(-4.0, 3.0),
        Align2::RIGHT_TOP,
        if live { "live" } else { "region" },
        font.clone(),
        weak,
    );

    // Pointer: frequency under the cursor, read on the front row.
    if let Some(pos) = resp.hover_pos() {
        let u = (pos.x - plot.left()) / plane_w;
        if (0.0..=1.0).contains(&u) {
            let f = spectrum::axis_freq(u, f_max);
            let txt = if f >= 1000.0 { format!("{:.2} kHz", f / 1000.0) } else { format!("{f:.0} Hz") };
            p.line_segment(
                [Pos2::new(pos.x, plot.bottom() - amp_h), Pos2::new(pos.x, plot.bottom())],
                Stroke::new(1.0, PLAY_COLOR.gamma_multiply(0.6)),
            );
            p.text(Pos2::new(rect.left() + 4.0, rect.top() + 3.0), Align2::LEFT_TOP, txt, font, PLAY_COLOR);
        }
    }
}

fn theme_wave() -> Color32 {
    crate::ui::theme::WAVE
}

/// Level 0..1 to a dark-blue → cyan → amber → red ramp.
fn heat(v: f32) -> Color32 {
    const STOPS: [(f32, [u8; 3]); 5] = [
        (0.0, [18, 28, 56]),
        (0.35, [36, 92, 172]),
        (0.6, [52, 190, 190]),
        (0.82, [240, 200, 70]),
        (1.0, [245, 84, 48]),
    ];
    let v = v.clamp(0.0, 1.0);
    for w in STOPS.windows(2) {
        let ((a, ca), (b, cb)) = (w[0], w[1]);
        if v <= b {
            let t = (v - a) / (b - a);
            let c = |i: usize| (ca[i] as f32 + (cb[i] as f32 - ca[i] as f32) * t) as u8;
            return Color32::from_rgb(c(0), c(1), c(2));
        }
    }
    let [r, g, b] = STOPS[4].1;
    Color32::from_rgb(r, g, b)
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let c = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(c(a.r(), b.r()), c(a.g(), b.g()), c(a.b(), b.b()))
}
