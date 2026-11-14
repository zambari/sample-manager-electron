//! Waveform painting: selections, trim/fade cues, ruler and markers.

use super::*;
use super::input::WaveHot;

pub(super) fn paint_waveform(
    ui: &mut Ui,
    ed: &Editor,
    pv: &PlayView,
    g: &WaveGeometry<'_>,
    hot: WaveHot,
    trim: Option<&crate::editor::TrimPlan>,
    lock_rect: Option<Rect>,
    lock_hovered: bool,
    focused: bool,
) {
    let m = &g.m;
    let rect = m.rect;
    let x_in = m.x(g.sel.0 as f64);
    let x_out = m.x(g.sel.1 as f64);
    let marks = &g.marks;
    let abrupt = g.abrupt;
    let slot_cut = &g.slot_cut;
    let off = g.off;
    let painter = ui.painter_at(rect);
    let vis = ui.visuals().clone();
    painter.rect_filled(rect, 2.0, vis.extreme_bg_color);

    let ruler = Rect::from_min_size(rect.min, vec2(rect.width(), RULER_H));
    let lanes = Rect::from_min_max(Pos2::new(rect.left(), ruler.bottom()), rect.max);

    // Region background.
    let reg = Rect::from_x_y_ranges(
        x_in.max(rect.left())..=x_out.min(rect.right()),
        lanes.y_range(),
    );
    if reg.width() > 0.0 {
        painter.rect_filled(reg, 0.0, vis.selection.bg_fill.gamma_multiply(0.12));
    }

    draw_ruler(&painter, m, ruler, ed.sample().map_or(1, |s| s.rate), &vis);

    // Range bar across the ruler between the markers.
    let bar = Rect::from_x_y_ranges(
        x_in.max(rect.left())..=x_out.min(rect.right()),
        ruler.y_range(),
    );
    if bar.width() > 0.0 {
        let a = if hot.bar { 0.45 } else { 0.25 };
        painter.rect_filled(bar, 0.0, vis.selection.bg_fill.gamma_multiply(a));
        // Grip dots in the middle hint that it can be dragged.
        let c = bar.center();
        if bar.width() > 30.0 {
            let col = vis
                .strong_text_color()
                .gamma_multiply(if hot.bar { 0.8 } else { 0.45 });
            for dx in [-6.0, -2.0, 2.0, 6.0] {
                for dy in [-2.5, 2.5] {
                    painter.circle_filled(Pos2::new(c.x + dx, c.y + dy), 1.0, col);
                }
            }
        }
    }

    draw_lanes(&painter, ed, m, lanes, &vis);

    // Grid: each slot's padding (the silence after its sound, up to the
    // slot end) in solid dark, so a cut that the grid pads back shows up.
    if let (Some(n), Some((len, sounds))) = (ed.grid, ed.slot_sounds()) {
        for (i, &snd) in sounds.iter().enumerate().take(n) {
            if snd >= len {
                continue;
            }
            let a = m.x((i * len + snd) as f64 - off as f64).max(rect.left());
            let b = m.x(((i + 1) * len) as f64 - off as f64).min(rect.right());
            if b > a {
                painter.rect_filled(
                    Rect::from_x_y_ranges(a..=b, lanes.y_range()),
                    0.0,
                    PAD_COLOR,
                );
            }
        }
    }

    // Dim outside the region on top of the wave.
    let dim = vis
        .extreme_bg_color
        .gamma_multiply(if ed.is_kit { 0.2 } else { 0.55 });
    if x_in > rect.left() {
        painter.rect_filled(
            Rect::from_x_y_ranges(rect.left()..=x_in.min(rect.right()), lanes.y_range()),
            0.0,
            dim,
        );
    }
    if x_out < rect.right() {
        painter.rect_filled(
            Rect::from_x_y_ranges(x_out.max(rect.left())..=rect.right(), lanes.y_range()),
            0.0,
            dim,
        );
    }

    let rate = ed.sample().map_or(1, |s| s.rate);
    if let Some(plan) = trim {
        // On a grid every slot shrinks to the same length.
        let uniform = ed.grid.map(|n| plan.new_total / n.max(1));
        let grip = slot_cut.as_ref().map(|(_, _, cut, _)| (*cut, hot.cut));
        trim_cues(&painter, m, lanes, plan, off, uniform, grip);
    }
    fade_cues(&painter, m, lanes, abrupt, hot.fade, rate, &vis);
    if let Some(tp) = ed.trim_preview() {
        let rate = ed.sample().map_or(1, |s| s.rate);
        trim_overlay(&painter, m, lanes, &tp, rate, &vis);
    }

    // The slice whose menu is open: a faint wash so it's clear which one.
    if let Some(f) = ed.slice_menu {
        let off = ed.crop.map_or(0, |c| c.0);
        if f >= off {
            let (a, b) = ed.slice_at(f - off);
            let r = Rect::from_x_y_ranges(
                m.x(a as f64).max(rect.left())..=m.x(b as f64).min(rect.right()),
                lanes.y_range(),
            );
            if r.width() > 0.0 {
                painter.rect_filled(r, 0.0, vis.strong_text_color().gamma_multiply(0.05));
            }
        }
    }

    slice_marks(&painter, m, rect, RULER_H, marks);
    if let Some(r) = lock_rect {
        let hov = lock_hovered || hot.mark;
        padlock(&painter, r, ed.markers_locked, hov, &vis);
    }
    marker(&painter, rect, x_in, IN_COLOR, true);
    marker(&painter, rect, x_out, OUT_COLOR, false);

    // Parked playhead: thin line + small triangle in the ruler.
    let xp = m.x(ed.playhead as f64);
    if rect.x_range().contains(xp) {
        painter.line_segment(
            [Pos2::new(xp, ruler.bottom()), Pos2::new(xp, rect.bottom())],
            Stroke::new(1.0, vis.strong_text_color().gamma_multiply(0.7)),
        );
        let t = ruler.bottom();
        painter.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(xp - 5.0, t - 7.0),
                Pos2::new(xp + 5.0, t - 7.0),
                Pos2::new(xp, t),
            ],
            vis.strong_text_color(),
            Stroke::NONE,
        ));
    }
    if pv.playing {
        let x = m.x(pv.pos as f64);
        if rect.x_range().contains(x) {
            painter.line_segment(
                [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
                Stroke::new(1.5, PLAY_COLOR),
            );
        }
    }

    if focused {
        painter.rect_stroke(
            rect,
            2.0,
            Stroke::new(1.0, crate::ui::theme::ACCENT_BRIGHT.gamma_multiply(0.6)),
            egui::StrokeKind::Inside,
        );
    }
}

/// Trim fades: each drawn as its gain curve over the slice.
fn fade_cues(
    painter: &egui::Painter,
    m: &Map,
    lanes: Rect,
    ends: &[crate::editor::FadeEnd],
    hot: Option<usize>,
    rate: u32,
    vis: &egui::Visuals,
) {
    let col = vis.warn_fg_color;
    for e in ends {
        let is_hot = hot == Some(e.slice);
        let (xa, xb) = (m.x(e.start as f64), m.x(e.end as f64));
        if xb < lanes.left() || xa > lanes.right() {
            continue;
        }
        let top = lanes.top() + 4.0;
        let h = lanes.bottom() - top;
        let steps = ((xb - xa) / 2.0).clamp(2.0, 200.0) as usize;
        let pts: Vec<Pos2> = (0..=steps)
            .map(|i| {
                let t = i as f32 / steps as f32;
                let g = crate::editor::fade_gain(t, e.k);
                Pos2::new(xa + (xb - xa) * t, top + (1.0 - g) * h)
            })
            .collect();
        painter.line(pts, Stroke::new(if is_hot { 2.0 } else { 1.5 }, col));
        // Shade what the fade takes away, so its reach reads at a glance.
        if xb - xa > 1.0 {
            painter.rect_filled(
                Rect::from_x_y_ranges(xa.max(lanes.left())..=xb.min(lanes.right()), top..=lanes.bottom()),
                0.0,
                col.gamma_multiply(if is_hot { 0.10 } else { 0.05 }),
            );
        }
        // The knob: middle of the curve.
        let knob = Pos2::new(
            (xa + xb) / 2.0,
            top + (1.0 - crate::editor::fade_gain(0.5, e.k)) * h,
        );
        if lanes.x_range().contains(knob.x) {
            painter.circle(
                knob,
                if is_hot { HANDLE_R + 1.0 } else { HANDLE_R },
                if is_hot { col } else { vis.extreme_bg_color },
                Stroke::new(1.5, col),
            );
            if is_hot {
                let ms = (e.end - e.start) as f64 * 1000.0 / rate.max(1) as f64;
                let curve = if (e.k - 1.0).abs() < 0.05 {
                    "linear".to_string()
                } else if e.k < 1.0 {
                    format!("holds ×{:.1}", 1.0 / e.k)
                } else {
                    format!("decays ×{:.1}", e.k)
                };
                let text = if e.own {
                    format!(
                        "fade {ms:.0} ms · {curve} · own shape\n↔ length  ↕ curve\ndouble-click: back to shared"
                    )
                } else {
                    format!(
                        "fade {ms:.0} ms · {curve} · all fades\n↔ length  ↕ curve  ⌥ this one only\ndouble-click: reset"
                    )
                };
                let galley = painter.layout(text, FontId::monospace(10.0), col, 400.0);
                let mut pos = Pos2::new(knob.x + 10.0, knob.y - galley.size().y - 6.0);
                if pos.x + galley.size().x > lanes.right() {
                    pos.x = knob.x - 10.0 - galley.size().x;
                }
                pos.y = pos.y.max(lanes.top() + 2.0);
                let bg = Rect::from_min_size(pos, galley.size()).expand(3.0);
                painter.rect_filled(bg, 3.0, vis.extreme_bg_color.gamma_multiply(0.9));
                painter.galley(pos, galley, col);
            }
        }
        // A tick at the hard stop, visible even zoomed far out.
        painter.line_segment(
            [Pos2::new(xb, lanes.bottom() - 10.0), Pos2::new(xb, lanes.bottom())],
            Stroke::new(2.0, col),
        );
    }
}

/// Tail trim: shade what each slice would lose, with a line at the cut.
/// `uniform`: a grid kit, where every slot shrinks to this length. `grip`:
/// trim by length, the slot cut (from each slot start) to draw as a
/// draggable line, and whether it's hot.
fn trim_cues(
    painter: &egui::Painter,
    m: &Map,
    lanes: Rect,
    plan: &crate::editor::TrimPlan,
    off: usize,
    uniform: Option<usize>,
    grip: Option<(usize, bool)>,
) {
    let col = super::super::strip::SAVE_COLOR;
    for s in &plan.slices {
        if s.b <= off {
            continue;
        }
        let x_at = |f: usize| m.x(f.saturating_sub(off) as f64);
        let cut = s.a + uniform.unwrap_or(s.keep);
        if cut < s.b {
            let (xa, xb) = (x_at(cut), x_at(s.b));
            if xb >= lanes.left() && xa <= lanes.right() {
                let r = Rect::from_x_y_ranges(xa.max(lanes.left())..=xb.min(lanes.right()), lanes.y_range());
                painter.rect_filled(r, 0.0, col.gamma_multiply(0.10));
                if grip.is_none() && lanes.x_range().contains(xa) {
                    painter.line_segment([Pos2::new(xa, lanes.top()), Pos2::new(xa, lanes.bottom())], Stroke::new(1.5, col));
                }
            }
        }
        // The slot cut, in the theme's orange with a grip at the top: drag
        // any to move them all.
        if let Some((c, hot)) = grip {
            let x = x_at(s.a + c);
            if lanes.x_range().contains(x) {
                let gc = crate::ui::theme::ACCENT_BRIGHT;
                let w = if hot { 2.5 } else { 1.5 };
                painter.line_segment([Pos2::new(x, lanes.top()), Pos2::new(x, lanes.bottom())], Stroke::new(w, gc));
                let (t, r) = (lanes.top(), if hot { 6.0 } else { 4.5 });
                painter.add(egui::Shape::convex_polygon(
                    vec![Pos2::new(x - r, t), Pos2::new(x + r, t), Pos2::new(x, t + r * 1.4)],
                    gc,
                    Stroke::NONE,
                ));
            }
        }
    }
}

/// Grid kit, part of a slot selected: shade the padding each slot would
/// lose if the selection were deleted, mark where the next-longest slice
/// ends in the selected slot, and say how much time it saves.
fn trim_overlay(
    painter: &egui::Painter,
    m: &Map,
    lanes: Rect,
    tp: &crate::editor::TrimPreview,
    rate: u32,
    vis: &egui::Visuals,
) {
    let secs = |f: usize| f as f64 / rate.max(1) as f64;
    let vline = |x: f32, col: Color32| {
        if lanes.x_range().contains(x) {
            painter.add(egui::Shape::dashed_line(
                &[Pos2::new(x, lanes.top()), Pos2::new(x, lanes.bottom())],
                Stroke::new(1.0, col),
                4.0,
                3.0,
            ));
        }
    };
    let per = tp.len - tp.new_len;
    if per > 0 {
        for i in 0..tp.n {
            let (a, b) = (i * tp.len + tp.new_len, (i + 1) * tp.len);
            let (xa, xb) = (m.x(a as f64), m.x(b as f64));
            let r = Rect::from_x_y_ranges(
                xa.max(lanes.left())..=xb.min(lanes.right()),
                lanes.y_range(),
            );
            if r.width() > 0.0 {
                painter.rect_filled(r, 0.0, OUT_COLOR.gamma_multiply(0.16));
            }
            vline(xa, OUT_COLOR.gamma_multiply(0.8));
        }
    }
    // Where the slot length bottoms out: cutting further saves nothing more.
    if tp.floor < tp.len {
        vline(m.x((tp.slot * tp.len + tp.floor) as f64), MARK_COLOR);
    }

    let text = if per > 0 {
        let mut t = format!(
            "Delete: {} slots {:.3}s → {:.3}s  ·  −{:.3}s each, −{:.2}s in all",
            tp.n,
            secs(tp.len),
            secs(tp.new_len),
            secs(per),
            secs(tp.saved()),
        );
        if tp.binding != tp.slot {
            t.push_str(&format!(
                "  ·  slot {} now sets the length — cutting more saves nothing",
                tp.binding + 1
            ));
        }
        t
    } else {
        format!(
            "Slot {} is {:.3}s long and sets the slot length — cutting here saves nothing",
            tp.binding + 1,
            secs(tp.len)
        )
    };
    let font = FontId::monospace(11.0);
    let col = if per > 0 { OUT_COLOR } else { vis.weak_text_color() };
    let galley = painter.layout(text, font, col, lanes.width() - 12.0);
    let pos = Pos2::new(lanes.left() + 6.0, lanes.top() + 16.0);
    let bg = Rect::from_min_size(pos, galley.size()).expand(3.0);
    painter.rect_filled(bg, 3.0, vis.extreme_bg_color.gamma_multiply(0.85));
    painter.galley(pos, galley, col);
}

/// The marker padlock: a body and a shackle, swung open when unlocked.
fn padlock(painter: &egui::Painter, r: Rect, locked: bool, hot: bool, vis: &egui::Visuals) {
    painter.rect_filled(r, 0.0, vis.extreme_bg_color);
    let col = if locked {
        vis.strong_text_color().gamma_multiply(if hot { 0.95 } else { 0.6 })
    } else {
        MARK_COLOR.gamma_multiply(if hot { 1.0 } else { 0.85 })
    };
    let c = r.center();
    let body = Rect::from_center_size(Pos2::new(c.x, c.y + 3.0), vec2(10.0, 7.0));
    painter.rect_filled(body, 1.5, col);
    // Shackle: an upside-down U over the body; open, its left leg lifts out.
    let (l, rr, top) = (body.left() + 2.0, body.right() - 2.0, body.top() - 5.5);
    let lift = if locked { 0.0 } else { 3.0 };
    let stroke = Stroke::new(1.6, col);
    let pts: Vec<Pos2> = (0..=8)
        .map(|k| {
            let t = std::f32::consts::PI * k as f32 / 8.0;
            Pos2::new(c.x - (rr - l) / 2.0 * t.cos(), top - lift - 2.5 * t.sin() + 2.5)
        })
        .collect();
    painter.add(egui::Shape::line(pts, stroke));
    painter.line_segment([Pos2::new(rr, top + 2.5 - lift), Pos2::new(rr, body.top())], stroke);
    painter.line_segment(
        [Pos2::new(l, top + 2.5 - lift), Pos2::new(l, body.top() - lift)],
        stroke,
    );
}

/// Kit slice starts: a line down the whole view, a tab in the ruler and the
/// slice number beside it.
pub(in super::super) fn slice_marks(
    painter: &egui::Painter,
    m: &Map,
    rect: Rect,
    ruler_h: f32,
    marks: &[usize],
) {
    for (i, &f) in marks.iter().enumerate() {
        let x = m.x(f as f64);
        if !(rect.left() - 1.0..=rect.right() + 1.0).contains(&x) {
            continue;
        }
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.0, MARK_COLOR.gamma_multiply(0.85)),
        );
        if ruler_h > 0.0 {
            let r = Rect::from_min_size(Pos2::new(x, rect.top()), vec2(4.0, ruler_h));
            painter.rect_filled(r, 0.0, MARK_COLOR);
            painter.text(
                Pos2::new(x + 6.0, rect.top() + ruler_h + 2.0),
                Align2::LEFT_TOP,
                format!("{}", i + 2),
                FontId::monospace(10.0),
                MARK_COLOR,
            );
        }
    }
}

pub(in super::super) fn marker(painter: &egui::Painter, rect: Rect, x: f32, col: Color32, is_in: bool) {
    if !(rect.left() - 1.0..=rect.right() + 1.0).contains(&x) {
        return;
    }
    painter.line_segment(
        [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
        Stroke::new(1.5, col),
    );
    // A flag pointing into the region, so in and out can't be confused.
    let top = rect.top() + RULER_H;
    let dx = if is_in { 9.0 } else { -9.0 };
    painter.add(egui::Shape::convex_polygon(
        if is_in {
            vec![
                Pos2::new(x, top),
                Pos2::new(x + dx, top + 5.0),
                Pos2::new(x, top + 10.0),
            ]
        } else {
            vec![
                Pos2::new(x + dx, top + 5.0),
                Pos2::new(x, top),
                Pos2::new(x, top + 10.0),
            ]
        },
        col,
        Stroke::NONE,
    ));
}

pub(in super::super) fn draw_ruler(
    painter: &egui::Painter,
    m: &Map,
    r: Rect,
    rate: u32,
    vis: &egui::Visuals,
) {
    painter.rect_filled(r, 0.0, vis.faint_bg_color);
    let pps = rate as f64 / m.fpp; // pixels per second
    const STEPS: &[f64] = &[
        0.0001, 0.0002, 0.0005, 0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0,
        5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0,
    ];
    let step = STEPS
        .iter()
        .copied()
        .find(|s| s * pps >= 80.0)
        .unwrap_or(600.0);
    let decimals = (-step.log10()).ceil().max(0.0) as usize;
    let t0 = (m.start / rate as f64 / step).floor() * step;
    let t1 = (m.start + r.width() as f64 * m.fpp) / rate as f64;
    let mut t = t0;
    let col = vis.weak_text_color();
    while t <= t1 + step {
        let x = m.x(t * rate as f64);
        if x >= r.left() - 1.0 {
            painter.line_segment(
                [Pos2::new(x, r.bottom() - 6.0), Pos2::new(x, r.bottom())],
                Stroke::new(1.0, col),
            );
            // Minor ticks.
            for k in 1..5 {
                let xm = m.x((t + step * k as f64 / 5.0) * rate as f64);
                painter.line_segment(
                    [Pos2::new(xm, r.bottom() - 3.0), Pos2::new(xm, r.bottom())],
                    Stroke::new(1.0, col.gamma_multiply(0.5)),
                );
            }
            painter.text(
                Pos2::new(x + 3.0, r.top() + 1.0),
                Align2::LEFT_TOP,
                format!("{t:.decimals$}"),
                FontId::monospace(10.0),
                col,
            );
        }
        t += step;
    }
}
