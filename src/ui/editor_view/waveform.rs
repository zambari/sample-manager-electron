//! The main waveform: lanes, ruler, in/out and slice markers, and its mouse handling.

use super::*;

mod geometry;
mod input;
mod lanes;
mod paint;

use geometry::WaveGeometry;
use input::{handle_wave_mouse, update_wave_view};
use paint::paint_waveform;

pub(super) use lanes::draw_lanes;
pub(super) use paint::{draw_ruler, marker, slice_marks};

/// Radius of a fade knob, shared by hit detection and painting.
const HANDLE_R: f32 = 5.0;

pub(super) fn waveform(
    ui: &mut Ui,
    ed: &mut Editor,
    pv: &PlayView,
    rect: Rect,
    focused: bool,
    abrupt: &[crate::editor::FadeEnd],
    trim: Option<&crate::editor::TrimPlan>,
    homes: &[super::SliceHome],
    out: &mut Vec<EdIntent>,
) {
    // When the pane is resized, keep the same stretch of time on screen
    // (rescale) rather than showing more or less of it at the old zoom.
    if let Some(prev) = ed.wave_rect {
        let (old_w, new_w) = (prev.width() as f64, rect.width() as f64);
        if old_w > 1.0 && new_w > 1.0 && (old_w - new_w).abs() > 0.5 {
            ed.view.fpp *= old_w / new_w;
        }
    }
    ed.wave_rect = Some(rect);
    let frames = ed.frames();
    if ed.view.fit_pending && rect.width() > 1.0 {
        ed.view.fpp = frames as f64 / rect.width() as f64;
        ed.view.start = 0.0;
        ed.view.fit_pending = false;
    }

    let id = ui.scope_id().with("wave");
    let resp = ui.interact(rect, id, Sense::click_and_drag());
    // The padlock, top right in the ruler: registered after the wave so it
    // takes its own clicks.
    let lock_rect = (ed.is_kit && ed.has_markers()).then(|| {
        Rect::from_min_size(
            Pos2::new(rect.right() - 20.0, rect.top()),
            vec2(20.0, RULER_H),
        )
    });
    let lock_resp = lock_rect.map(|r| {
        let tip = if ed.markers_locked {
            "Markers locked: cuts keep the grid's slots (padded with silence, shown dark). Click to unlock: \
             no grid, cuts close up, markers can be dragged (onto zero crossings with Snap to 0)."
        } else {
            "Markers unlocked: drag a marker (its tab in the ruler, or its line) to move it. \
             Click to lock: the grid comes back if the markers still split the file evenly."
        };
        ui.interact(r, id.with("lock"), Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text(tip)
    });
    if lock_resp.as_ref().is_some_and(|r| r.clicked()) {
        out.push(EdIntent::TookFocus);
        let lock = !ed.markers_locked;
        ed.set_markers_locked(lock);
        ed.notice = Some(match (lock, ed.grid) {
            (false, _) => {
                "Markers unlocked — grid off, cuts close up, drag markers to move them".to_owned()
            }
            (true, Some(n)) => format!("Markers locked — on {n} equal slots"),
            (true, None) => {
                "Markers locked — not an even split, so no grid (pick one to lay the slices out)"
                    .to_owned()
            }
        });
    }
    update_wave_view(ui, ed, pv, rect, &resp);
    let g = WaveGeometry::new(ed, rect, frames, abrupt, trim.is_some());
    let hot = handle_wave_mouse(ui, ed, pv, &resp, &g, homes, out);
    paint_waveform(
        ui,
        ed,
        pv,
        &g,
        hot,
        trim,
        lock_rect,
        lock_resp.as_ref().is_some_and(|r| r.hovered()),
        focused,
    );
}
