//! The scope strip: a fixed 80 ms window around the playhead.

use super::*;

/// Fixed 80 ms window centred on the work playhead; it never follows the
/// play cursor (which is drawn only when it passes through). No zoom: drag the wave to nudge the playhead, drag an in/out
/// line that falls inside the window to move it, click to park there.
pub(super) fn scope(
    ui: &mut Ui,
    ed: &mut Editor,
    pv: &PlayView,
    rect: Rect,
    out: &mut Vec<EdIntent>,
) {
    let frames = ed.frames();
    let Some(rate) = ed.sample().map(|s| s.rate) else {
        return;
    };
    if frames == 0 || rect.width() < 2.0 {
        return;
    }
    let fpp = SCOPE_SECS * rate as f64 / rect.width() as f64;

    let id = ui.scope_id().with("scope");
    let resp = ui.interact(rect, id, Sense::click_and_drag());

    let centre = ed.playhead;
    let half_w = rect.width() as f64 * 0.5 * fpp;
    let mut m = Map {
        rect,
        start: centre as f64 - half_w,
        fpp,
        frames,
    };
    let x_in = m.x(ed.sel.0 as f64);
    let x_out = m.x(ed.sel.1 as f64);

    // ---- mouse ----
    let near = |x: f32, p: Pos2| (p.x - x).abs() <= GRAB_PX;
    if let Some(p) = resp.hover_pos() {
        if ed.drag.is_none() {
            let icon = if near(x_in, p) || near(x_out, p) {
                CursorIcon::ResizeHorizontal
            } else {
                CursorIcon::Grab
            };
            ui.ctx().set_cursor_icon(icon);
        }
    }
    if resp.drag_started() {
        out.push(EdIntent::TookFocus);
        let origin = ui
            .input(|i| i.pointer.press_origin())
            .or(resp.interact_pointer_pos());
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
        painter.rect_filled(
            Rect::from_x_y_ranges(rect.left()..=x0, lanes.y_range()),
            0.0,
            void,
        );
    }
    if x1 < rect.right() {
        painter.rect_filled(
            Rect::from_x_y_ranges(x1..=rect.right(), lanes.y_range()),
            0.0,
            void,
        );
    }

    let reg = Rect::from_x_y_ranges(
        x_in.max(rect.left())..=x_out.min(rect.right()),
        lanes.y_range(),
    );
    if reg.width() > 0.0 {
        painter.rect_filled(reg, 0.0, vis.selection.bg_fill.gamma_multiply(0.12));
    }
    draw_ruler(&painter, &m, ruler, rate, &vis);
    draw_lanes(&painter, ed, &m, lanes, &vis);

    let dim = vis.extreme_bg_color.gamma_multiply(0.55);
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
    if ed.is_kit {
        slice_marks(&painter, &m, rect, 0.0, &ed.marks());
    }
    marker(&painter, rect, x_in, IN_COLOR, true);
    marker(&painter, rect, x_out, OUT_COLOR, false);

    // Work playhead (always centred) and the play cursor.
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
    painter.text(
        Pos2::new(rect.right() - 4.0, ruler.bottom() + 2.0),
        Align2::RIGHT_TOP,
        "80 ms",
        FontId::proportional(10.0),
        vis.weak_text_color(),
    );
}
