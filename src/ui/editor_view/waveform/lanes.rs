//! Audio lane rendering, including gain, limiting and mono-fold cues.

use super::*;

/// Waveform lanes (one per channel), min/max bars or individual samples
/// depending on zoom. Shared by the main view and the scope strip.
pub(in super::super) fn draw_lanes(
    painter: &egui::Painter,
    ed: &Editor,
    m: &Map,
    lanes: Rect,
    vis: &egui::Visuals,
) {
    let Some(cur) = &ed.current else { return };
    let rect = m.rect;
    let n_ch = cur.sample.channels.len().max(1);
    let lane_h = lanes.height() / n_ch as f32;
    let gain = ed.gain();
    let lim = crate::audio::limit::Limiter::new(gain);
    // A kit's gain is baked into the samples, so judge those by what the
    // limiter wrote rather than running them through it again.
    let baked = (ed.is_kit && ed.sel_gain_db() > 0.0)
        .then(|| crate::audio::limit::Limiter::new(crate::audio::stats::from_db(ed.sel_gain_db())));
    let level = |s: f32| match &baked {
        Some(b) => b.level_out(s * gain),
        None => lim.level(s * gain),
    };
    let wave_in = crate::ui::theme::WAVE;
    // In a kit the other slices are content too, so keep them close to the
    // selected one instead of greying them out like a trimmed-away region.
    let wave_out = if ed.is_kit {
        wave_in.gamma_multiply(0.7)
    } else {
        vis.weak_text_color()
    };

    // Which channels make it into a mono export. Fold::Right takes the last
    // channel when there is only one, matching Fold::at.
    let fold = ed.fold();
    let kept = |c: usize| match fold {
        Some(Fold::Left) => c == 0,
        Some(Fold::Right) => c == 1.min(n_ch - 1),
        _ => true,
    };

    for (c, (data, peaks)) in cur.sample.channels.iter().zip(cur.peaks.iter()).enumerate() {
        let lane = Rect::from_min_size(
            Pos2::new(lanes.left(), lanes.top() + c as f32 * lane_h),
            vec2(lanes.width(), lane_h),
        );
        // A channel the mono fold throws away recedes into the background.
        let (wave_in, wave_out, clip_col, limit_col) = if kept(c) {
            (wave_in, wave_out, CLIP_COLOR, LIMIT_COLOR)
        } else {
            (
                wave_in.gamma_multiply(0.22),
                wave_out.gamma_multiply(0.35),
                CLIP_COLOR.gamma_multiply(0.3),
                LIMIT_COLOR.gamma_multiply(0.3),
            )
        };
        let level_col = |l: crate::audio::limit::Level| match l {
            crate::audio::limit::Level::Clipped => clip_col,
            crate::audio::limit::Level::Limited => limit_col,
            crate::audio::limit::Level::Clean => wave_in,
        };
        let mid = lane.center().y;
        let half = lane_h * 0.46;
        let y = |s: f32| (mid - lim.apply(s) * half).clamp(lane.top(), lane.bottom());
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
                let col = if inside {
                    level_col(level(hi).max(level(lo)))
                } else {
                    wave_out
                };
                let x = rect.left() + px as f32 + 0.5;
                let (ya, yb) = (y(hi.max(0.0)), y(lo.min(0.0)));
                painter.line_segment(
                    [Pos2::new(x, ya), Pos2::new(x, yb.max(ya + 1.0))],
                    Stroke::new(1.0, col),
                );
            }
        } else {
            // Few samples per pixel: a filled area between the zero line and
            // the signal, with an anti-aliased curve on top for a smooth edge.
            let f0 = (m.start.floor() - 1.0).max(0.0) as usize;
            let f1 = ((m.start + rect.width() as f64 * m.fpp).ceil() as usize + 2).min(data.len());
            if f1 <= f0 {
                continue;
            }
            let pts: Vec<Pos2> = (f0..f1)
                .map(|f| Pos2::new(m.x(f as f64), y(data[f])))
                .collect();
            let inside = |f: usize| f >= ed.sel.0 && f < ed.sel.1;
            let fill_col = |f: usize, s: f32| {
                let base = if !inside(f) {
                    wave_out
                } else {
                    level_col(level(s))
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
                    painter.line(
                        std::mem::take(&mut run),
                        Stroke::new(1.2, if run_in { wave_in } else { wave_out }),
                    );
                    run_in = now_in;
                }
                run.push(*p);
            }
            if run.len() > 1 {
                painter.line(
                    run,
                    Stroke::new(1.2, if run_in { wave_in } else { wave_out }),
                );
            }

            if 1.0 / m.fpp >= 6.0 {
                for (i, p) in pts.iter().enumerate() {
                    let f = f0 + i;
                    let col = if f >= ed.sel.0 && f < ed.sel.1 {
                        wave_in
                    } else {
                        wave_out
                    };
                    painter.circle_filled(*p, 2.2, col);
                }
            }
        }
    }

    if let Some(fold) = fold {
        mono_cues(painter, ed, m, lanes, lane_h, n_ch, fold, &kept, vis);
    }
}

/// What a mono fold does to the lanes: a slim bar beside each channel that
/// ends up in the export, a quiet note on the one that's dropped, and for a
/// mix-down a warm band on the seam showing (heavily exaggerated) how much
/// level L+R lose to phase cancellation at each point.
#[allow(clippy::too_many_arguments)]
fn mono_cues(
    painter: &egui::Painter,
    ed: &Editor,
    m: &Map,
    lanes: Rect,
    lane_h: f32,
    n_ch: usize,
    fold: Fold,
    kept: &dyn Fn(usize) -> bool,
    vis: &egui::Visuals,
) {
    let Some(cur) = &ed.current else { return };
    let lane = |c: usize| {
        Rect::from_min_size(
            Pos2::new(lanes.left(), lanes.top() + c as f32 * lane_h),
            vec2(lanes.width(), lane_h),
        )
    };

    for c in 0..n_ch {
        let r = lane(c);
        if kept(c) {
            // Inset a little so neighbouring kept lanes read as separate bars.
            let (t, b) = (r.top() + 3.0, r.bottom() - 3.0);
            if b > t {
                painter.rect_filled(
                    Rect::from_x_y_ranges(r.left()..=r.left() + 2.0, t..=b),
                    1.0,
                    MONO_COLOR.gamma_multiply(0.85),
                );
            }
        } else {
            painter.text(
                Pos2::new(r.right() - 8.0, r.top() + 5.0),
                Align2::RIGHT_TOP,
                "not exported",
                FontId::proportional(10.0),
                vis.weak_text_color().gamma_multiply(0.7),
            );
        }
    }

    if fold != Fold::Mix || n_ch != 2 {
        return;
    }
    let (l, r) = (&cur.sample.channels[0], &cur.sample.channels[1]);
    let len = l.len().min(r.len());
    if len == 0 {
        return;
    }
    let seam = lanes.top() + lane_h;
    let reach = lane_h * 0.42;
    let warm = crate::ui::theme::ACCENT_BRIGHT;
    // Measure over at least this many frames so a zoomed-in view doesn't
    // flicker sample by sample.
    const MIN_WIN: f64 = 48.0;
    // At most this many frames looked at per column, spread evenly.
    const PROBES: usize = 64;

    let mut mesh = egui::Mesh::default();
    let w = m.rect.width().ceil() as usize;
    for px in 0..w {
        let a = m.start + px as f64 * m.fpp;
        let b = a + m.fpp;
        let pad = ((MIN_WIN - m.fpp) * 0.5).max(0.0);
        let f0 = (a - pad).max(0.0) as usize;
        let f1 = ((b + pad).ceil() as usize).min(len);
        if f0 >= f1 {
            if f0 >= len {
                break;
            }
            continue;
        }
        let step = ((f1 - f0) / PROBES).max(1);
        let (mut el, mut er, mut em, mut n) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        let mut i = f0;
        while i < f1 {
            let (x, y) = (l[i], r[i]);
            let mid = (x + y) * 0.5;
            el += x * x;
            er += y * y;
            em += mid * mid;
            n += 1.0;
            i += step;
        }
        // Level the mix loses: average channel RMS minus mono RMS. Zero for
        // identical channels, the full level for channels in opposite phase.
        let side = ((el / n).sqrt() + (er / n).sqrt()) * 0.5;
        let loss = (side - (em / n).sqrt()).max(0.0);
        // Exaggerated: a few percent of lost level already shows.
        let amt = (loss * 6.0).sqrt().min(1.0);
        if amt < 0.02 {
            continue;
        }
        let h = amt * reach;
        let x0 = m.rect.left() + px as f32;
        let x1 = x0 + 1.0;
        let core = warm.gamma_multiply(0.5 * (0.4 + 0.6 * amt));
        let edge = warm.gamma_multiply(0.0);
        // Two quads per column, brightest on the seam and fading outwards.
        for y_edge in [seam - h, seam + h] {
            let i = mesh.vertices.len() as u32;
            mesh.colored_vertex(Pos2::new(x0, seam), core);
            mesh.colored_vertex(Pos2::new(x1, seam), core);
            mesh.colored_vertex(Pos2::new(x1, y_edge), edge);
            mesh.colored_vertex(Pos2::new(x0, y_edge), edge);
            mesh.add_triangle(i, i + 1, i + 2);
            mesh.add_triangle(i, i + 2, i + 3);
        }
    }
    if !mesh.is_empty() {
        painter.add(egui::Shape::mesh(mesh));
    }
}
