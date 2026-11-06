//! The main waveform: lanes, ruler, in/out and slice markers, and its mouse handling.

use super::*;

pub(super) fn waveform(
    ui: &mut Ui,
    ed: &mut Editor,
    pv: &PlayView,
    rect: Rect,
    focused: bool,
    abrupt: &[crate::editor::FadeEnd],
    trim: Option<&crate::editor::TrimPlan>,
    out: &mut Vec<EdIntent>,
) {
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

    let m = Map {
        rect,
        start: ed.view.start,
        fpp: ed.view.fpp,
        frames,
    };
    let x_in = m.x(ed.sel.0 as f64);
    let x_out = m.x(ed.sel.1 as f64);
    let marks = if ed.is_kit { ed.marks() } else { Vec::new() };

    // Fade handles: a knob at the middle of each trim fade's curve.
    let fade_top = rect.top() + RULER_H + 4.0;
    let fade_h = rect.bottom() - fade_top;
    let handle_pos = |e: &crate::editor::FadeEnd| {
        let x = m.x(e.start as f64 + (e.end - e.start) as f64 / 2.0);
        let y = fade_top + (1.0 - crate::editor::fade_gain(0.5, e.k)) * fade_h;
        Pos2::new(x, y)
    };
    let handle_at = |p: Pos2| {
        abrupt
            .iter()
            .find(|e| handle_pos(e).distance(p) <= HANDLE_R + 4.0)
            .copied()
    };
    // Grid trim by length: the cut line in every slot, any of them
    // draggable. (slot count, slot length, cut, each slot's sound length)
    let off = ed.crop.map_or(0, |c| c.0);
    let slot_cut = match (trim.is_some() && ed.trims_by_length(), ed.grid, ed.slot_sounds()) {
        (true, Some(n), Some((len, sounds))) => Some((n, len, ed.slot_cut_len().unwrap_or(len), sounds)),
        _ => None,
    };
    let cut_at = |p: Pos2| {
        let (n, len, cut, _) = slot_cut.as_ref()?;
        if p.y <= rect.top() + RULER_H {
            return None;
        }
        (0..*n).find(|i| (m.x((i * len + cut) as f64 - off as f64) - p.x).abs() <= GRAB_PX)
    };
    let mut hot_cut = matches!(ed.drag, Some(Drag::SlotCut(..)));
    let mut hot_fade = match ed.drag {
        Some(Drag::Fade(i, ..)) => Some(i),
        _ => None,
    };

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
    // Forgive small misclicks: a point within SNAP_PX of a slice marker or
    // either end of the file lands exactly on it (nearest wins), so a
    // selection never loses a few frames at an edge by accident.
    let snapped = |x: f32| {
        marks
            .iter()
            .copied()
            .chain([0, frames])
            .map(|f| (f, (m.x(f as f64) - x).abs()))
            .filter(|&(_, d)| d <= SNAP_PX)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map_or_else(|| m.frame(x), |(f, _)| f)
    };
    // The in or out point under the pointer (where the cursor shows the
    // resize icon), nearest wins.
    let sel = ed.sel;
    let endpoint_at = |p: Pos2| {
        let (di, dout) = ((p.x - x_in).abs(), (p.x - x_out).abs());
        if near(x_in, p) && (!near(x_out, p) || di <= dout) {
            Some(sel.0)
        } else if near(x_out, p) {
            Some(sel.1)
        } else {
            None
        }
    };
    // Range bar: the ruler strip between the markers drags the whole region.
    let on_bar = |p: Pos2| {
        p.y >= rect.top()
            && p.y <= rect.top() + RULER_H
            && p.x > x_in + GRAB_PX
            && p.x < x_out - GRAB_PX
    };
    let mut bar_hot = matches!(ed.drag, Some(Drag::Move(..)));
    if let Some(p) = resp.hover_pos() {
        if ed.drag.is_none() {
            if let Some(e) = handle_at(p) {
                ui.ctx().set_cursor_icon(CursorIcon::Move);
                hot_fade = Some(e.slice);
            } else if near(x_in, p) || near(x_out, p) {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            } else if cut_at(p).is_some() {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                hot_cut = true;
            } else if on_bar(p) {
                ui.ctx().set_cursor_icon(CursorIcon::Grab);
                bar_hot = true;
            }
        }
    }
    // Kit: the ruler and slice-number row name the file a slice came from.
    if ed.is_kit && ed.drag.is_none() {
        if let Some(p) = resp.hover_pos() {
            if p.y <= rect.top() + RULER_H + 16.0 {
                if let Some((n, name)) = ed.slice_name_at(m.frame(p.x)) {
                    resp.clone()
                        .on_hover_text_at_pointer(format!("Slice {n}: {name}"));
                }
            }
        }
    }
    if matches!(ed.drag, Some(Drag::Move(..))) {
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    }
    if resp.drag_started() {
        out.push(EdIntent::TookFocus);
        let origin = ui
            .input(|i| i.pointer.press_origin())
            .or(resp.interact_pointer_pos());
        let alt = ui.input(|i| i.modifiers.alt);
        ed.drag = origin.map(|o| {
            if resp.dragged_by(egui::PointerButton::Middle) || alt {
                Drag::Pan
            } else if let Some(e) = handle_at(o) {
                Drag::Fade(e.slice, if e.tail { e.start } else { e.end }, e.tail)
            } else if near(x_in, o)
                && (!near(x_out, o) || (o.x - x_in).abs() <= (o.x - x_out).abs())
            {
                Drag::In
            } else if near(x_out, o) {
                Drag::Out
            } else if let Some(i) = cut_at(o) {
                Drag::SlotCut(i)
            } else if on_bar(o) {
                Drag::Move(ed.sel, m.frame(o.x))
            } else {
                Drag::Select(snapped(o.x))
            }
        });
    }
    if resp.dragged() {
        if let (Some(d), Some(p)) = (ed.drag, resp.interact_pointer_pos()) {
            let px = p.x.clamp(rect.left(), rect.right());
            let f = snapped(px);
            match d {
                Drag::In => ed.set_in(f),
                Drag::Out => ed.set_out(f),
                Drag::Select(a) => ed.set_region(a, f),
                Drag::Move(orig, grab) => ed.move_region(orig, m.frame(px) as i64 - grab as i64),
                Drag::Pan => ed.view.start -= resp.drag_delta().x as f64 * ed.view.fpp,
                Drag::Scrub(..) => {}
                Drag::Fade(slice, fixed, tail) => {
                    // Sideways: the knob is the fade's middle, so the fade
                    // runs twice that far from its fixed end. Up / down:
                    // the gain at the middle, which sets the curve.
                    let len = if tail {
                        2 * (m.frame(px).max(fixed + 1) - fixed)
                    } else {
                        2 * (fixed - m.frame(px).min(fixed.saturating_sub(1)))
                    };
                    let g = (1.0 - (p.y - fade_top) / fade_h).clamp(0.5f32.powf(8.0), 0.5);
                    ed.shape_fade(slice, len, g.ln() / 0.5f32.ln());
                }
                Drag::SlotCut(i) => {
                    if let Some((_, len, _, sounds)) = &slot_cut {
                        let rel = (m.frame(px) + off).saturating_sub(i * len);
                        // Snap onto where a sound ends: cutting right
                        // there frees the most without touching it.
                        let rel = sounds
                            .iter()
                            .copied()
                            .filter(|&s| s > 0 && (s.abs_diff(rel) as f64 / m.fpp) <= SNAP_PX as f64)
                            .min_by_key(|&s| s.abs_diff(rel))
                            .unwrap_or(rel)
                            .clamp(1, *len);
                        ed.slot_cut = (rel < *len).then_some(rel);
                    }
                }
            }
            // Dragging a marker past the edge scrolls the view along.
            if !matches!(d, Drag::Pan | Drag::Fade(..) | Drag::SlotCut(..)) {
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
            // On the in/out point: land exactly on it; else snap to a slice
            // marker or a file end when close.
            ed.playhead = endpoint_at(p).unwrap_or_else(|| snapped(p.x));
            if pv.playing {
                out.push(EdIntent::Seek(ed.playhead));
            }
        }
    }
    if resp.double_clicked() {
        // On a marker: park the playhead exactly on it. Elsewhere: select
        // the kit slice there, or everything.
        let p = resp.interact_pointer_pos();
        let fade = p.and_then(handle_at);
        let target = p.and_then(|p| endpoint_at(p).or_else(|| mark_at(p)));
        if let Some(e) = fade {
            ed.reset_fade(e.slice);
        } else {
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
    let reg = Rect::from_x_y_ranges(
        x_in.max(rect.left())..=x_out.min(rect.right()),
        lanes.y_range(),
    );
    if reg.width() > 0.0 {
        painter.rect_filled(reg, 0.0, vis.selection.bg_fill.gamma_multiply(0.12));
    }

    draw_ruler(&painter, &m, ruler, ed.sample().map_or(1, |s| s.rate), &vis);

    // Range bar across the ruler between the markers.
    let bar = Rect::from_x_y_ranges(
        x_in.max(rect.left())..=x_out.min(rect.right()),
        ruler.y_range(),
    );
    if bar.width() > 0.0 {
        let a = if bar_hot { 0.45 } else { 0.25 };
        painter.rect_filled(bar, 0.0, vis.selection.bg_fill.gamma_multiply(a));
        // Grip dots in the middle hint that it can be dragged.
        let c = bar.center();
        if bar.width() > 30.0 {
            let col = vis
                .strong_text_color()
                .gamma_multiply(if bar_hot { 0.8 } else { 0.45 });
            for dx in [-6.0, -2.0, 2.0, 6.0] {
                for dy in [-2.5, 2.5] {
                    painter.circle_filled(Pos2::new(c.x + dx, c.y + dy), 1.0, col);
                }
            }
        }
    }

    draw_lanes(&painter, ed, &m, lanes, &vis);

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
        let grip = slot_cut.as_ref().map(|(_, _, cut, _)| (*cut, hot_cut));
        trim_cues(&painter, &m, lanes, plan, off, uniform, grip);
    }
    fade_cues(&painter, &m, lanes, abrupt, hot_fade, rate, &vis);
    if let Some(tp) = ed.trim_preview() {
        let rate = ed.sample().map_or(1, |s| s.rate);
        trim_overlay(&painter, &m, lanes, &tp, rate, &vis);
    }

    slice_marks(&painter, &m, rect, RULER_H, &marks);
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

/// Waveform lanes (one per channel), min/max bars or individual samples
/// depending on zoom. Shared by the main view and the scope strip.
pub(super) fn draw_lanes(
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
    let wave_in = crate::ui::theme::WAVE;
    // In a kit the other slices are content too, so keep them close to the
    // selected one instead of greying them out like a trimmed-away region.
    let wave_out = if ed.is_kit {
        wave_in.gamma_multiply(0.7)
    } else {
        vis.weak_text_color()
    };

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
                let col = if clip && inside {
                    CLIP_COLOR
                } else if inside {
                    wave_in
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
                let text = format!(
                    "fade {ms:.0} ms · curve {:.1}\n↔ length  ↕ curve\ndouble-click: reset",
                    e.k
                );
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
    let col = super::strip::SAVE_COLOR;
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

/// Radius of a fade knob.
const HANDLE_R: f32 = 5.0;

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

/// Kit slice starts: a line down the whole view, a tab in the ruler and the
/// slice number beside it.
pub(super) fn slice_marks(
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

pub(super) fn marker(painter: &egui::Painter, rect: Rect, x: f32, col: Color32, is_in: bool) {
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

pub(super) fn draw_ruler(
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
