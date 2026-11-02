//! Modal dialogs: the pre-flight conflict prompt and the move-to-Trash
//! confirmation.

use crate::fs_model::human_bytes;
use crate::ops::{Plan, Policy};
use egui::{Context, Id, Modal, RichText};

pub enum Decision {
    Proceed(Policy),
    /// Merge conflicting folders into the existing ones.
    Merge,
    Cancel,
}

/// Ask once, up front, how to handle every destination that already exists.
///
/// Asking per-file mid-transfer is the thing that makes other file managers
/// miserable, so the plan is built first and the whole question posed once.
/// `can_merge`: offer "Merge" (some conflicts are folders, and this kind of
/// transfer supports it).
pub fn conflicts(ctx: &Context, plan: &Plan, can_merge: bool) -> Option<Decision> {
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
                let keep_tip = if n == 1 { "Keep both, then rename the new one" } else { "Keep both; the new ones get a number" };
                if ui.button(Policy::KeepBoth.label()).on_hover_text(keep_tip).clicked() {
                    decision = Some(Decision::Proceed(Policy::KeepBoth));
                }
                if can_merge
                    && ui
                        .button("Merge")
                        .on_hover_text(
                            "Combine folders: new files are added, files with the same name and size \
                             count as duplicates (skipped, or on a move sent to the Trash), \
                             same name but different size is left where it is",
                        )
                        .clicked()
                {
                    decision = Some(Decision::Merge);
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

    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        decision = Some(Decision::Cancel);
    }

    decision
}

/// Confirm moving `paths` to the Trash (`device`: the device's /trash
/// folder), or with `permanent`, deleting them outright. `Some(true)` = go
/// ahead, `Some(false)` = cancelled, `None` = still open.
pub fn confirm_trash(ctx: &Context, paths: &[std::path::PathBuf], device: bool, permanent: bool) -> Option<bool> {
    let mut answer = None;
    let bin = if device { "/trash" } else { "Trash" };
    let verb = |what: &str| if permanent { format!("Delete {what} now?") } else { format!("Move {what} to {bin}?") };
    Modal::new(Id::unique("trash_modal")).show(ctx, |ui| {
        ui.set_width(420.0);
        let n = paths.len();
        if n == 1 {
            let name = paths[0]
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            ui.heading(verb(&format!("\"{name}\"")));
        } else {
            ui.heading(verb(&format!("{n} items")));
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
        let note = if permanent {
            "They are deleted from the device straight away, folders with everything in them. This can't be undone."
        } else if device {
            "They go to the /trash folder on the device; move them back from there."
        } else {
            "Undo (⌘Z / Ctrl+Z) puts them back."
        };
        ui.label(RichText::new(note).weak());
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                answer = Some(false);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(RichText::new(if permanent { "Delete".to_owned() } else { format!("Move to {bin}") }).color(ui.visuals().error_fg_color))
                    .on_hover_text(if permanent { "" } else { "↩" })
                    .clicked()
                {
                    answer = Some(true);
                }
            });
        });
    });
    // Keys are consumed, so the app's own Enter (commit) doesn't also fire
    // once the dialog has closed.
    ctx.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            answer = Some(false);
        } else if !permanent && i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
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
    // Keys are consumed, so the app's own Enter (commit) doesn't also fire
    // once the dialog has closed.
    ctx.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            answer = Some(false);
        } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            answer = Some(true);
        }
    });
    answer
}

/// Confirm reading a sample off the device into the editor. Over USB MIDI
/// that is slow, so say so first. `Some(true)` = go, `Some(false)` =
/// cancelled, `None` = still open.
pub fn confirm_device_load(ctx: &Context, path: &std::path::Path, bytes: u64) -> Option<bool> {
    let mut answer = None;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Modal::new(Id::unique("device_load_modal")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.heading(format!("Load \"{name}\" into the editor?"));
        ui.add_space(6.0);
        let size = if bytes > 0 { format!(" ({})", human_bytes(bytes)) } else { String::new() };
        ui.label(format!(
            "The sample{size} has to be read from the device over USB MIDI first, \
             which is slow — this can take a while. You can cancel it from the status bar."
        ));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                answer = Some(false);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Load").on_hover_text("↩").clicked() {
                    answer = Some(true);
                }
            });
        });
    });
    // Keys are consumed, so the app's own Enter (commit) doesn't also fire
    // once the dialog has closed.
    ctx.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            answer = Some(false);
        } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            answer = Some(true);
        }
    });
    answer
}

pub enum LeaveChoice {
    /// Stay on the edited file.
    Cancel,
    Discard,
    SaveInPlace,
    Commit,
}

/// The editor's audio was cut or pasted into and is about to be left (another
/// file is loading, or the app is closing). `quitting` changes the wording;
/// `dest` names the destination folder, `None` when it can't take a commit.
pub fn leave_editor(ctx: &Context, name: &str, quitting: bool, dest: Option<&str>) -> Option<LeaveChoice> {
    let mut choice = None;
    Modal::new(Id::unique("leave_editor_modal")).show(ctx, |ui| {
        ui.set_width(460.0);
        ui.heading(format!("\"{name}\" has unsaved edits"));
        ui.add_space(6.0);
        ui.label(if quitting {
            "Audio was cut or pasted into this file. Quitting now throws those edits away."
        } else {
            "Audio was cut or pasted into this file. Opening another one throws those edits away \
             (the clipboard is kept, so you can still paste into the next file)."
        });
        ui.add_space(6.0);
        ui.label(
            RichText::new("Saving writes what you hear: the trimmed region, with gain. Files that aren't WAV are saved as a WAV beside the original.")
                .weak(),
        );
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").on_hover_text("Esc — keep editing").clicked() {
                choice = Some(LeaveChoice::Cancel);
            }
            if ui
                .button(RichText::new("Discard edits").color(ui.visuals().error_fg_color))
                .on_hover_text(if quitting { "Quit without saving" } else { "Open the other file without saving" })
                .clicked()
            {
                choice = Some(LeaveChoice::Discard);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let commit = ui
                    .add_enabled(dest.is_some(), egui::Button::new("Commit to destination"))
                    .on_hover_text(dest.map_or("The destination tab is busy".to_owned(), |d| format!("Export into {d}, leaving the original untouched")));
                if commit.clicked() {
                    choice = Some(LeaveChoice::Commit);
                }
                if ui.button("Save in place").on_hover_text("Overwrite the original file").clicked() {
                    choice = Some(LeaveChoice::SaveInPlace);
                }
            });
        });
    });
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        choice = Some(LeaveChoice::Cancel);
    }
    choice
}
