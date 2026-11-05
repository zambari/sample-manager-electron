//! The overview strip: the whole file, with the visible range boxed.

use super::*;

pub(super) fn overview(ui: &mut Ui, ed: &mut Editor, pv: &PlayView, rect: Rect, out: &mut Vec<EdIntent>) {
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
