//! Modal dialogs: the pre-flight conflict prompt, the mono-export warning and
//! the move-to-Trash confirmation.

use crate::audio::stats::{self, Phase};
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

pub enum MonoChoice {
    SaveMono,
    SaveStereo,
    /// Turn mono preview on and go listen before exporting.
    Listen,
    Cancel,
}

/// "Save mono" is on but you've been listening in stereo: say what folding
/// will throw away before doing it.
pub fn mono_export(ctx: &Context, ph: &Phase) -> Option<MonoChoice> {
    let mut choice = None;
    Modal::new(Id::unique("mono_export_modal")).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.heading("Export as mono?");
        ui.add_space(6.0);
        ui.label(
            "You're previewing in stereo, but Save mono is on. Folding the channels into one \
             loses the stereo image: anything panned or wide ends up in the centre, and parts \
             that are out of phase between left and right cancel out.",
        );
        ui.add_space(6.0);
        let corr = ph.corr.map_or("—".to_owned(), |c| format!("{c:+.2}"));
        let line = format!(
            "This region: correlation {corr} · mono sum {} vs stereo",
            stats::fmt_db(ph.mono_loss_db)
        );
        let risky = ph.corr.is_some_and(|c| c < 0.3) || ph.mono_loss_db < -4.0;
        if risky {
            ui.colored_label(ui.visuals().warn_fg_color, format!("⚠ {line} — noticeable cancellation"));
        } else {
            ui.label(RichText::new(line).weak());
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                choice = Some(MonoChoice::Cancel);
            }
            if ui.button("🎧 Listen in mono").on_hover_text("Turn on mono preview; nothing is exported").clicked() {
                choice = Some(MonoChoice::Listen);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Save mono").on_hover_text("↩").clicked() {
                    choice = Some(MonoChoice::SaveMono);
                }
                if ui.button("Save stereo").clicked() {
                    choice = Some(MonoChoice::SaveStereo);
                }
            });
        });
    });
    ctx.input(|i| {
        if i.key_pressed(egui::Key::Escape) {
            choice = Some(MonoChoice::Cancel);
        } else if i.key_pressed(egui::Key::Enter) {
            choice = Some(MonoChoice::SaveMono);
        }
    });
    choice
}

/// Confirm moving `paths` to the Trash (`device`: the Digitakt's /trash
/// folder). `Some(true)` = go ahead, `Some(false)` = cancelled, `None` =
/// still open.
pub fn confirm_trash(ctx: &Context, paths: &[std::path::PathBuf], device: bool) -> Option<bool> {
    let mut answer = None;
    let bin = if device { "/trash" } else { "Trash" };
    Modal::new(Id::unique("trash_modal")).show(ctx, |ui| {
        ui.set_width(420.0);
        let n = paths.len();
        if n == 1 {
            let name = paths[0]
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            ui.heading(format!("Move \"{name}\" to {bin}?"));
        } else {
            ui.heading(format!("Move {n} items to {bin}?"));
            ui.add_space(6.0);
            egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                for p in paths {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    ui.horizontal(|ui| {
                        ui.label(if !device && p.is_dir() { "📁" } else { "🎵" });
                        ui.label(RichText::new(name).monospace());
                    });
                }
            });
        }
        ui.add_space(6.0);
        let note = if device {
            "They go to the /trash folder on the Digitakt; move them back from there."
        } else {
            "You can restore them from the Trash."
        };
        ui.label(RichText::new(note).weak());
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                answer = Some(false);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(RichText::new(format!("Move to {bin}")).color(ui.visuals().error_fg_color))
                    .on_hover_text("↩")
                    .clicked()
                {
                    answer = Some(true);
                }
            });
        });
    });
    ctx.input(|i| {
        if i.key_pressed(egui::Key::Escape) {
            answer = Some(false);
        } else if i.key_pressed(egui::Key::Enter) {
            answer = Some(true);
        }
    });
    answer
}

/// Confirm trashing duplicate files. `Some(true)` = go ahead,
/// `Some(false)` = cancelled, `None` = still open.
pub fn confirm_duplicates(ctx: &Context, groups: &[crate::jobs::DupGroup]) -> Option<bool> {
    let mut answer = None;
    let n: usize = groups.iter().map(|g| g.remove.len()).sum();
    let freed: u64 = groups.iter().map(|g| g.bytes * g.remove.len() as u64).sum();
    let name = |p: &std::path::Path| {
        p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    };
    Modal::new(Id::unique("dupes_modal")).show(ctx, |ui| {
        ui.set_width(480.0);
        ui.heading(format!("Move {n} duplicate(s) to Trash?"));
        ui.label(format!(
            "{} group(s) of identical files · frees {}",
            groups.len(),
            human_bytes(freed)
        ));
        ui.add_space(6.0);
        egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
            for g in groups {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("keep").color(egui::Color32::from_rgb(90, 210, 110)));
                    ui.label(RichText::new(name(&g.keep)).monospace().strong());
                    ui.label(RichText::new(human_bytes(g.bytes)).weak());
                });
                for r in &g.remove {
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        ui.label(RichText::new("trash").color(ui.visuals().error_fg_color));
                        ui.label(RichText::new(name(r)).monospace().strikethrough());
                    });
                }
                ui.add_space(4.0);
            }
        });
        ui.add_space(6.0);
        ui.label(RichText::new("Contents were compared byte for byte. You can restore them from the Trash.").weak());
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                answer = Some(false);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(RichText::new("Move to Trash").color(ui.visuals().error_fg_color))
                    .on_hover_text("↩")
                    .clicked()
                {
                    answer = Some(true);
                }
            });
        });
    });
    ctx.input(|i| {
        if i.key_pressed(egui::Key::Escape) {
            answer = Some(false);
        } else if i.key_pressed(egui::Key::Enter) {
            answer = Some(true);
        }
    });
    answer
}
