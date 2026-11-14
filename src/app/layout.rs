//! Toolbar, status bar, resizable columns and divider painting.

use super::*;

impl App {
    /// Draws the bars and resizable source/editor/destination layout.
    pub(super) fn show_layout(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        if crate::device::presence().available() {
            if let Some(Err(reason)) = self.device_status.clone() {
                egui::Panel::top("device_connection_error").show(ui, |ui| {
                    ui.colored_label(ui.visuals().error_fg_color, format!("Device not connected: {reason}"));
                    if ui.button("Retry connection").clicked() {
                        self.device_status = None;
                        self.device.list(PathBuf::from("/"), 0);
                    }
                });
            }
        }

        // Three columns: Source | Sample Editor | Destination.
        // The side columns are drag-resizable; the editor takes whatever is
        // left, but never less than MIN_EDITOR_W.
        const MIN_SIDE_W: f32 = 220.0;
        const MIN_EDITOR_W: f32 = 360.0;
        let w = ui.available_width();
        let default_w = w * 0.27;
        let prev_r = if self.side_widths[1] > 0.0 {
            self.side_widths[1]
        } else {
            default_w
        };
        let max_for = |other: f32| (w - other - MIN_EDITOR_W).max(MIN_SIDE_W);

        // Elek Sample Order mode takes the whole window: the file panes have
        // nothing to do with it.
        if self.elek_mode && self.elek.hide_files {
            egui::CentralPanel::default().show(ui, |ui| self.show_elek(ui));
        } else {
            let left = egui::Panel::left("source_col")
                .resizable(true)
                .default_size(default_w)
                .size_range(MIN_SIDE_W..=max_for(prev_r))
                .show(ui, |ui| self.show_pane(ui, Role::Source));
            self.side_widths[0] = left.response.rect.width();
            // The pool in the right column snaps it to DOCK_W the first
            // time this run, and caps it at DOCK_MAX_W while it shows.
            const DOCK_W: f32 = 420.0;
            const DOCK_MAX_W: f32 = 680.0;
            let mut max_r = max_for(self.side_widths[0]);
            if self.pool_in_col() {
                max_r = max_r.min(DOCK_MAX_W).max(MIN_SIDE_W);
            }
            let mut right = egui::Panel::right("dest_col")
                .resizable(true)
                .default_size(default_w)
                .size_range(MIN_SIDE_W..=max_r);
            if self.pool_in_col() && !self.pool_snapped {
                self.pool_snapped = true;
                right = right.exact_size(DOCK_W.clamp(MIN_SIDE_W, max_r));
            }
            let right = right.show(ui, |ui| self.show_dest_col(ui));
            self.side_widths[1] = right.response.rect.width();
            draw_divider(ui, left.response.rect.right(), left.response.rect.y_range());
            draw_divider(
                ui,
                right.response.rect.left(),
                right.response.rect.y_range(),
            );

            if self.elek_mode {
                egui::CentralPanel::default().show(ui, |ui| self.show_elek(ui));
            } else {
                egui::CentralPanel::default().show(ui, |ui| self.show_editor(ui));
            }

            // A click anywhere decides whether the docked pool keeps the keyboard.
            let press = ui.input(|i| i.pointer.primary_pressed().then(|| i.pointer.interact_pos()).flatten());
            if let Some(pos) = press {
                let inside = self.pool_in_col() && self.pool_rect.is_some_and(|r| r.contains(pos));
                self.pool_focused = inside;
                if inside {
                    self.editor_focused = false;
                }
            }
        }
    }
}

/// A visible column divider with a small grip in the middle, so the
/// draggable panel edge is easy to find even when not hovered.
fn draw_divider(ui: &egui::Ui, x: f32, ys: egui::Rangef) {
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::unique("column_dividers"),
    ));
    let visuals = &ui.style().visuals;
    let line = egui::Stroke::new(
        1.0,
        visuals.widgets.inactive.bg_stroke.color.gamma_multiply(1.6),
    );
    painter.vline(x, ys, line);

    // Grip: three dots centred on the divider.
    let cy = ys.center();
    let dot = visuals.widgets.inactive.fg_stroke.color;
    for dy in [-8.0, 0.0, 8.0] {
        painter.circle_filled(egui::pos2(x, cy + dy), 1.8, dot);
    }
}
