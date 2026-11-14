//! Waveform navigation, mouse dispatch, hover feedback and slice menus.

use super::*;

/// Wheel navigation and playback following run before the frame's map is captured.
pub(super) fn update_wave_view(
    ui: &mut Ui,
    ed: &mut Editor,
    pv: &PlayView,
    rect: Rect,
    resp: &egui::Response,
) {
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
}

pub(super) struct WaveHot {
    pub(super) cut: bool,
    pub(super) fade: Option<usize>,
    pub(super) bar: bool,
    pub(super) mark: bool,
}

pub(super) fn handle_wave_mouse(
    ui: &mut Ui,
    ed: &mut Editor,
    pv: &PlayView,
    resp: &egui::Response,
    g: &WaveGeometry<'_>,
    homes: &[super::super::SliceHome],
    out: &mut Vec<EdIntent>,
) -> WaveHot {
    let m = &g.m;
    let rect = m.rect;
    let mut hot = WaveHot {
        cut: matches!(ed.drag, Some(Drag::SlotCut(..))),
        fade: match ed.drag {
            Some(Drag::Fade(i, ..)) => Some(i),
            _ => None,
        },
        bar: matches!(ed.drag, Some(Drag::Move(..))),
        mark: matches!(ed.drag, Some(Drag::Marker(_))),
    };
    if let Some(p) = resp.hover_pos() {
        if ed.drag.is_none() {
            match g.drag_at(ed, p, false, false) {
                Drag::Fade(slice, ..) => {
                    ui.ctx().set_cursor_icon(CursorIcon::Move);
                    hot.fade = Some(slice);
                }
                Drag::Marker(_) => {
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeColumn);
                    hot.mark = true;
                }
                Drag::In | Drag::Out => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
                Drag::SlotCut(_) => {
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                    hot.cut = true;
                }
                Drag::Move(..) => {
                    ui.ctx().set_cursor_icon(CursorIcon::Grab);
                    hot.bar = true;
                }
                _ => {}
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
        ed.drag =
            origin.map(|o| g.drag_at(ed, o, resp.dragged_by(egui::PointerButton::Middle), alt));
    }
    if resp.dragged() {
        g.update_drag(ui, ed, resp);
    }
    if resp.drag_stopped() {
        if matches!(ed.drag, Some(Drag::Marker(_))) {
            ed.markers_moved();
        }
        ed.drag = None;
    }
    if resp.clicked() {
        out.push(EdIntent::TookFocus);
        if let Some(p) = resp.interact_pointer_pos() {
            // On the in/out point: land exactly on it; else snap to a slice
            // marker or a file end when close.
            ed.playhead = g.endpoint_at(p).unwrap_or_else(|| g.snapped(p.x));
            if pv.playing {
                out.push(EdIntent::Seek(ed.playhead));
            }
        }
    }
    if resp.double_clicked() {
        // On a marker: park the playhead exactly on it. Elsewhere: select
        // the kit slice there, or everything.
        let p = resp.interact_pointer_pos();
        let fade = p.and_then(|p| g.handle_at(p));
        let target = p.and_then(|p| g.endpoint_at(p).or_else(|| g.mark_at(p)));
        if let Some(e) = fade {
            // Its own shape goes first, back to the shared one; then the
            // shared one back to the default.
            if e.own {
                ed.reset_fade(e.slice);
            } else {
                ed.reset_all_fades();
            }
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
        ed.slice_menu = resp
            .interact_pointer_pos()
            .filter(|_| ed.is_kit)
            .map(|p| m.frame(p.x) + ed.crop.map_or(0, |c| c.0));
    }
    // Kit: right-click a slice to find where it came from.
    let menu = if ed.is_kit && ed.slice_menu.is_some() {
        resp.context_menu(|ui| slice_menu(ui, ed, homes, out))
    } else {
        None
    };
    if menu.is_none() {
        ed.slice_menu = None;
    }

    hot
}

/// A kit slice's right-click menu: show the file it came from in the source
/// pane, or the pool slot holding the same device file.
fn slice_menu(ui: &mut Ui, ed: &Editor, homes: &[super::super::SliceHome], out: &mut Vec<EdIntent>) {
    let Some(f) = ed.slice_menu else { return };
    let off = ed.crop.map_or(0, |c| c.0);
    let shown = f.saturating_sub(off);
    let i = ed.slice_index_at(shown);
    let home = homes.get(i).cloned().unwrap_or_default();
    let title = match ed.slice_name_at(shown) {
        Some((n, name)) => format!("Slice {n}: {name}"),
        None => format!("Slice {}", i + 1),
    };
    ui.label(RichText::new(title).weak());
    ui.separator();
    let tip = match (&home.local, &home.saved_as) {
        (Some(p), _) if home.by_audio => format!("{}\nFound by its audio: the SliceKit didn't say where this slice came from", p.display()),
        (Some(p), _) => p.display().to_string(),
        (None, Some(s)) => format!("Saved as \"{s}\" only: the full path wasn't kept, so it can't be found"),
        (None, None) => "This slice doesn't say where it came from".to_owned(),
    };
    let src = ui
        .add_enabled(
            home.local.is_some(),
            egui::Button::new(if home.by_audio { "Reveal in source pane (found by audio)" } else { "Reveal in source pane" }),
        )
        .on_hover_text(&tip)
        .on_disabled_hover_text(&tip);
    if src.clicked() {
        if let Some(p) = home.local {
            out.push(EdIntent::ShowInSource(p));
        }
        ui.close();
    }
    let pool_tip = match (&home.pool, home.pool_same, home.pool_by_name) {
        (Some(slot), Some(true), _) => format!("Slot {slot} holds the same audio (matched by content, not name)"),
        (Some(slot), Some(false), true) => {
            format!("Slot {slot} has a file of the same name, but its audio differs from this slice's source")
        }
        (Some(slot), Some(false), false) => format!("Slot {slot} came from this slice's source, but the source changed since"),
        (Some(slot), None, _) => format!("Slot {slot} has a file of the same name. Not compared yet: fetch it for preview to check"),
        (None, ..) => "No pool slot holds this file: it was never sent to or fetched from the device, it changed since, or no project is loaded".to_owned(),
    };
    let label = match (&home.pool, home.pool_same) {
        (Some(slot), Some(true)) => format!("Reveal in sample pool ({slot})"),
        (Some(slot), Some(false)) => format!("Reveal in sample pool ({slot}, not identical)"),
        (Some(slot), None) => format!("Reveal in sample pool ({slot}?, by name)"),
        (None, _) => "Reveal in sample pool".to_owned(),
    };
    let pool = ui
        .add_enabled(home.pool.is_some(), egui::Button::new(label))
        .on_hover_text(&pool_tip)
        .on_disabled_hover_text(&pool_tip);
    if pool.clicked() {
        out.push(EdIntent::SliceInPool(i));
        ui.close();
    }
}
