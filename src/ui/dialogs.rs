//! Modal dialogs. Currently just the pre-flight conflict prompt.

use crate::fs_model::human_bytes;
use crate::ops::{Plan, Policy};
use egui::{Context, Id, Modal, RichText};

pub enum Decision {
    Proceed(Policy),
    Cancel,
}

/// Ask once, up front, how to handle every destination that already exists.
///
/// Asking per-file mid-transfer is the thing that makes other file managers
/// miserable, so the plan is built first and the whole question posed once.
pub fn conflicts(ctx: &Context, plan: &Plan) -> Option<Decision> {
    let mut decision = None;

    Modal::new(Id::unique("conflict_modal")).show(ctx, |ui| {
        ui.set_width(460.0);
        let n = plan.conflicts();
        ui.heading(format!(
            "{n} item{} already exist{} at the destination",
            if n == 1 { "" } else { "s" },
            if n == 1 { "s" } else { "" }
        ));
        ui.add_space(6.0);

        egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
            for op in plan.ops.iter().filter(|o| o.conflict) {
                let name = op
                    .dst
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label(if op.is_dir { "📁" } else { "🎵" });
                    ui.label(RichText::new(name).monospace());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(human_bytes(op.approx_bytes)).weak());
                    });
                });
            }
        });

        if !plan.rejected.is_empty() {
            ui.add_space(6.0);
            ui.separator();
            for (path, why) in &plan.rejected {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("skipped {}: {why}", path.display()),
                );
            }
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                decision = Some(Decision::Cancel);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // "Keep both" first and default: it is the only
                // non-destructive answer, so it should be the easy one to hit.
                if ui.button(Policy::KeepBoth.label()).clicked() {
                    decision = Some(Decision::Proceed(Policy::KeepBoth));
                }
                if ui.button(Policy::Skip.label()).clicked() {
                    decision = Some(Decision::Proceed(Policy::Skip));
                }
                if ui
                    .button(RichText::new(Policy::Replace.label()).color(ui.visuals().error_fg_color))
                    .clicked()
                {
                    decision = Some(Decision::Proceed(Policy::Replace));
                }
            });
        });
    });

    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        decision = Some(Decision::Cancel);
    }

    decision
}
