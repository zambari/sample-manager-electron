//! Shared file/slot ping scrolling and transient save/attention painting.

use super::*;

/// Animate toward a pinged row with a quintic S-curve and no overshoot.
pub fn ping_scroll(ui: &Ui, id: egui::Id, current: f32, target: Option<f32>) -> Option<f32> {
    let now = ui.input(|i| i.time);
    if let Some(target) = target {
        ui.data_mut(|d| d.insert_temp(id, (now, current, target)));
    } else if ui.input(|i| i.smooth_scroll_delta != egui::Vec2::ZERO || i.pointer.any_pressed()) {
        ui.data_mut(|d| d.remove::<(f64, f32, f32)>(id));
    }
    let (start, from, to) = ui.data(|d| d.get_temp::<(f64, f32, f32)>(id))?;
    let t = ((now - start) as f32 / 0.9).clamp(0.0, 1.0);
    if t >= 1.0 {
        ui.data_mut(|d| d.remove::<(f64, f32, f32)>(id));
    } else {
        ui.ctx().request_repaint();
    }
    Some(from + (to - from) * ping_curve(t))
}

fn ping_curve(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// A steady accent that fades gently with no repeated pulses.
pub fn paint_ping(ui: &Ui, rect: egui::Rect, at: std::time::Instant, color: egui::Color32) {
    let t = at.elapsed().as_secs_f32();
    if t >= PING_SECS {
        return;
    }
    let a = 1.0 - ping_curve((t / PING_SECS).clamp(0.0, 1.0));
    ui.painter().rect_filled(rect, 2.0, color.gamma_multiply(0.35 * a));
    ui.painter()
        .rect_stroke(rect, 2.0, Stroke::new(2.0, color.gamma_multiply(a)), StrokeKind::Inside);
    ui.ctx().request_repaint();
}

/// Amber pulse while the file is being written, then a green flash that
/// fades once it's on disk.
pub(super) fn paint_save_mark(ui: &Ui, rect: egui::Rect, done: Option<std::time::Instant>) {
    let (color, alpha) = match done {
        None => {
            let t = ui.input(|i| i.time) as f32;
            (
                egui::Color32::from_rgb(245, 190, 60),
                0.25 + 0.2 * (t * 6.0).sin(),
            )
        }
        Some(at) => {
            let k = 1.0 - at.elapsed().as_secs_f32() / SAVE_FLASH_SECS;
            if k <= 0.0 {
                return;
            }
            (egui::Color32::from_rgb(90, 210, 120), 0.5 * k)
        }
    };
    ui.painter()
        .rect_filled(rect, 2.0, color.gamma_multiply(alpha));
    ui.painter().rect_stroke(
        rect,
        2.0,
        Stroke::new(1.0, color.gamma_multiply(alpha * 1.6)),
        StrokeKind::Inside,
    );
    ui.ctx().request_repaint();
}
