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
pub fn confirm_trash(
    ctx: &Context,
    paths: &[std::path::PathBuf],
    device: bool,
    permanent: bool,
) -> Option<bool> {
    let mut answer = None;
    let bin = if device { "/trash" } else { "Trash" };
    let verb = |what: &str| {
        if permanent {
            format!("Delete {what} now?")
        } else {
            format!("Move {what} to {bin}?")
        }
    };
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
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
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
        egui::ScrollArea::vertical()
            .max_height(300.0)
            .show(ui, |ui| {
                for g in groups {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("keep").color(egui::Color32::from_rgb(90, 210, 110)),
                        );
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
        ui.label(
            RichText::new(
                "Contents were compared byte for byte. You can restore them from the Trash.",
            )
            .weak(),
        );
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

/// Reading the device's factory samples failed: it doesn't allow copying
/// them off. `true` once dismissed.
pub fn factory_locked(ctx: &Context) -> bool {
    let mut done = false;
    Modal::new(Id::unique("factory_locked_modal")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.heading("Failed: copying factory samples is not allowed");
        ui.add_space(6.0);
        ui.label(
            "The device won't hand out the files under /factory, so they were skipped. \
             Anything else in the request is handled as usual.",
        );
        ui.add_space(10.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("OK").on_hover_text("↩").clicked() {
                done = true;
            }
        });
    });
    ctx.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
            || i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
        {
            done = true;
        }
    });
    done
}

/// Confirm reading a sample off the device into the editor. Over USB MIDI
/// that is slow, so say so first. `Some(true)` = go, `Some(false)` =
/// cancelled, `None` = still open.
pub fn confirm_device_load(ctx: &Context, path: &std::path::Path, bytes: u64) -> Option<bool> {
    let mut answer = None;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Modal::new(Id::unique("device_load_modal")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.heading(format!("Load \"{name}\" into the editor?"));
        ui.add_space(6.0);
        let size = if bytes > 0 {
            format!(" ({})", human_bytes(bytes))
        } else {
            String::new()
        };
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

/// The device came back (replugged, switched on, or another machine)
/// while `edits` unpushed project edits were open. `Some(true)` = drop them
/// and read the device, `Some(false)` = keep them, `None` = still open.
pub fn device_reconnected(ctx: &Context, edits: usize) -> Option<bool> {
    let mut answer = None;
    Modal::new(Id::unique("device_reconnected_modal")).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.heading("Device reconnected");
        ui.add_space(6.0);
        ui.label(
            "The device was disconnected, so its project may have changed meanwhile \
             (edited on the machine, another project loaded, or a different unit plugged in).",
        );
        ui.add_space(4.0);
        ui.label(format!(
            "You have {edits} unpushed edit(s). Reading the project again drops them. \
             Keeping them shows a project that may not match the device; a push still checks \
             first and holds back if the device's project changed."
        ));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Keep my edits").on_hover_text("Esc").clicked() {
                answer = Some(false);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Read the device").on_hover_text("↩").clicked() {
                    answer = Some(true);
                }
            });
        });
    });
    ctx.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            answer = Some(false);
        } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            answer = Some(true);
        }
    });
    answer
}

/// A device file was renamed and has a local copy: rename that too?
/// `Some(true)` = rename, `Some(false)` = leave it, `None` = still open.
pub fn rename_local(ctx: &Context, from: &std::path::Path, to: &std::path::Path) -> Option<bool> {
    let mut answer = None;
    let name = |p: &std::path::Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Modal::new(Id::unique("rename_local_modal")).show(ctx, |ui| {
        ui.set_width(460.0);
        ui.heading("Rename the local copy too?");
        ui.add_space(6.0);
        ui.label("The renamed device file has a local copy:");
        ui.label(RichText::new(from.display().to_string()).weak());
        ui.add_space(4.0);
        ui.label(format!("Rename \"{}\" to \"{}\"?", name(from), name(to)));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Keep local name").on_hover_text("Esc").clicked() {
                answer = Some(false);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Rename local copy").on_hover_text("↩").clicked() {
                    answer = Some(true);
                }
            });
        });
    });
    ctx.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            answer = Some(false);
        } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            answer = Some(true);
        }
    });
    answer
}

/// Ask before a save overwrites something that isn't the open file (the
/// original a device copy came from, a filled pool slot). Enter is not a
/// yes here: overwriting takes a click. `Some(true)` = go.
pub fn confirm_overwrite(ctx: &Context, title: &str, lines: &[String], ok: &str) -> Option<bool> {
    let mut answer = None;
    Modal::new(Id::unique("confirm_overwrite_modal")).show(ctx, |ui| {
        ui.set_width(480.0);
        ui.heading(title);
        ui.add_space(6.0);
        for (i, l) in lines.iter().enumerate() {
            if i == 0 {
                ui.label(l);
            } else {
                ui.label(RichText::new(l).weak());
            }
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                answer = Some(false);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let b = egui::Button::new(RichText::new(ok).color(crate::ui::theme::ON_ACCENT))
                    .fill(crate::ui::theme::ACCENT);
                if ui.add(b).clicked() {
                    answer = Some(true);
                }
            });
        });
    });
    ctx.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            answer = Some(false);
        }
    });
    answer
}

/// What to do with pool samples dragged to a local pane when some already
/// have a local copy.
pub enum SlotDownloadChoice {
    Cancel,
    /// Highlight the local copies in the pane they were dropped on.
    Show,
    /// Download every one, making another copy.
    All,
    /// Download just the ones without a local copy.
    Missing,
}

/// `have`: the local copies already on disk; `total`: samples dragged.
pub fn slot_download(
    ctx: &Context,
    have: &[&std::path::Path],
    total: usize,
    pane: &str,
) -> Option<SlotDownloadChoice> {
    let mut answer = None;
    let one = have.len() == 1;
    Modal::new(Id::unique("slot_download_modal")).show(ctx, |ui| {
        ui.set_width(500.0);
        ui.heading(if one { "You already have this sample" } else { "You already have some of these" });
        ui.add_space(6.0);
        ui.label(if one && total == 1 {
            "Downloading it makes a second copy. The local file:".to_owned()
        } else {
            format!("{} of the {total} samples already have a local copy:", have.len())
        });
        egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
            for p in have {
                ui.label(RichText::new(p.display().to_string()).weak());
            }
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                answer = Some(SlotDownloadChoice::Cancel);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let show = if one { "Show local file" } else { "Show local files" };
                if ui
                    .button(show)
                    .on_hover_text(format!("Select it in the {pane} pane instead (↩)"))
                    .clicked()
                {
                    answer = Some(SlotDownloadChoice::Show);
                }
                if total > have.len() && ui.button("Download only the missing").clicked() {
                    answer = Some(SlotDownloadChoice::Missing);
                }
                let all = if total == 1 { "Download a copy anyway" } else { "Download all anyway" };
                if ui.button(all).clicked() {
                    answer = Some(SlotDownloadChoice::All);
                }
            });
        });
    });
    ctx.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            answer = Some(SlotDownloadChoice::Cancel);
        } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            answer = Some(SlotDownloadChoice::Show);
        }
    });
    answer
}

/// Whether pool samples being downloaded get their slot numbers in front of
/// their file names.
pub enum SlotPrefixChoice {
    Cancel,
    /// Keep the +Drive names.
    Plain,
    /// `060_kick.wav`.
    Numbered,
}

/// `examples`: the file names with slot numbers, in slot order; `total`:
/// samples being downloaded.
pub fn slot_prefix(ctx: &Context, examples: &[String], total: usize) -> Option<SlotPrefixChoice> {
    let mut answer = None;
    Modal::new(Id::unique("slot_prefix_modal")).show(ctx, |ui| {
        ui.set_width(460.0);
        ui.heading("Add slot numbers to the file names?");
        ui.add_space(6.0);
        ui.label(format!(
            "Puts each sample's pool slot in front of its name, so the {total} copies show where they sat on the device:"
        ));
        egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
            for n in examples {
                ui.label(RichText::new(n).weak());
            }
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                answer = Some(SlotPrefixChoice::Cancel);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Add slot numbers").on_hover_text("↩").clicked() {
                    answer = Some(SlotPrefixChoice::Numbered);
                }
                if ui.button("Keep names").clicked() {
                    answer = Some(SlotPrefixChoice::Plain);
                }
            });
        });
    });
    ctx.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            answer = Some(SlotPrefixChoice::Cancel);
        } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            answer = Some(SlotPrefixChoice::Numbered);
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
/// Why the edited file is being left.
#[derive(Clone, Copy, PartialEq)]
pub enum LeaveWhy {
    Open,
    Quit,
    CloseKit,
}

pub fn leave_editor(
    ctx: &Context,
    name: &str,
    why: LeaveWhy,
    dest: Option<&str>,
) -> Option<LeaveChoice> {
    let mut choice = None;
    Modal::new(Id::unique("leave_editor_modal")).show(ctx, |ui| {
        ui.set_width(460.0);
        ui.heading(format!("\"{name}\" has unsaved edits"));
        ui.add_space(6.0);
        ui.label(match why {
            LeaveWhy::Quit => "Audio was cut or pasted into this file. Quitting now throws those edits away.",
            LeaveWhy::CloseKit => "This SliceKit has slices that haven't been saved. Closing it now throws them away.",
            LeaveWhy::Open => {
                "Audio was cut or pasted into this file. Opening another one throws those edits away \
                 (the clipboard is kept, so you can still paste into the next file)."
            }
        });
        if why != LeaveWhy::CloseKit {
            ui.add_space(6.0);
            ui.label(
                RichText::new("Saving writes what you hear, with gain (you'll be asked whether to trim to the selection). Files that aren't WAV are saved as a WAV beside the original.")
                    .weak(),
            );
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").on_hover_text("Esc — keep editing").clicked() {
                choice = Some(LeaveChoice::Cancel);
            }
            if ui
                .button(RichText::new("Discard edits").color(ui.visuals().error_fg_color))
                .on_hover_text(match why {
                    LeaveWhy::Quit => "Quit without saving",
                    LeaveWhy::CloseKit => "Close the SliceKit without saving",
                    LeaveWhy::Open => "Open the other file without saving",
                })
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

#[derive(Clone, Copy, PartialEq)]
pub enum SaveScope {
    Cancel,
    Trim,
    Whole,
}

/// Saving over the open file while in / out (or a crop) leave part of it
/// out: write just the selection, or the whole file with the gain?
/// `sel` and `whole` are lengths in seconds.
pub fn save_scope(ctx: &Context, name: &str, sel: f64, whole: f64) -> Option<SaveScope> {
    let mut choice = None;
    Modal::new(Id::unique("save_scope_modal")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.heading(format!("Save \"{name}\""));
        ui.add_space(6.0);
        ui.label(format!(
            "The selection ({sel:.3}s) is only part of the file ({whole:.3}s). Write just the selection, or the whole file?"
        ));
        ui.add_space(6.0);
        ui.label(RichText::new("Either way gain is applied and the original file is overwritten.").weak());
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                choice = Some(SaveScope::Cancel);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Save whole file").on_hover_text("W — ignore in / out, keep all of it").clicked() {
                    choice = Some(SaveScope::Whole);
                }
                if ui.button("Trim to selection").on_hover_text("↩ / T — keep only the in / out region").clicked() {
                    choice = Some(SaveScope::Trim);
                }
            });
        });
    });
    ctx.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            choice = Some(SaveScope::Cancel);
        } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
            || i.consume_key(egui::Modifiers::NONE, egui::Key::T)
        {
            choice = Some(SaveScope::Trim);
        } else if i.consume_key(egui::Modifiers::NONE, egui::Key::W) {
            choice = Some(SaveScope::Whole);
        }
    });
    choice
}

/// Export ("save as") found a file of that name in `dir`: pick another.
/// `name` has no extension. `Some(true)` = go, `Some(false)` = cancelled,
/// `None` = still open.
pub fn name_export(
    ctx: &Context,
    name: &mut String,
    dir: &std::path::Path,
    taken: &str,
) -> Option<bool> {
    let mut answer = None;
    let clean = name.trim();
    let valid = !clean.is_empty() && !clean.contains('/');
    let exists = valid && dir.join(format!("{clean}.wav")).exists();
    let taken_stem = std::path::Path::new(taken)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let is_taken = clean == taken_stem;
    let mut taken_stem = Some(taken_stem);
    Modal::new(Id::unique("name_export_modal")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.heading("Export as");
        ui.add_space(6.0);
        ui.label(format!(
            "\"{taken}\" is already in {}. Save the export under a new name:",
            dir.display()
        ));
        ui.add_space(8.0);
        let id = Id::unique("name_export_field");
        let resp = ui
            .horizontal(|ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(name)
                        .id(id)
                        .desired_width(ui.available_width() - 40.0),
                );
                ui.label(".wav");
                r
            })
            .inner;
        if !resp.has_focus() && !resp.lost_focus() {
            resp.request_focus();
        }
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && valid {
            answer = Some(true);
        }
        if exists {
            ui.add_space(4.0);
            ui.label(
                RichText::new("That file exists too — exporting will overwrite it.")
                    .color(ui.visuals().warn_fg_color),
            );
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                answer = Some(false);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if exists { "Overwrite" } else { "Export" };
                if ui
                    .add_enabled(valid, egui::Button::new(label))
                    .on_hover_text("↩")
                    .clicked()
                {
                    answer = Some(true);
                }
                // Skip renaming: write over the file that was already there.
                if !is_taken
                    && ui
                        .button(format!("Overwrite \"{taken}\""))
                        .on_hover_text("Replace the existing file instead of renaming")
                        .clicked()
                {
                    *name = taken_stem.take().unwrap_or_default();
                    answer = Some(true);
                }
            });
        });
    });
    ctx.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            answer = Some(false);
        } else if answer == Some(true) {
            i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
        }
    });
    answer
}

/// Name a new slice kit, saved as `<name>.wav` in `dir`. A name that's
/// already a file there adds to that file instead. `Some(true)` = go,
/// `Some(false)` = cancelled, `None` = still open.
pub fn name_kit(
    ctx: &Context,
    name: &mut String,
    dir: &std::path::Path,
    grid: &mut Option<usize>,
) -> Option<bool> {
    let mut answer = None;
    Modal::new(Id::unique("name_kit_modal")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.heading("New SliceKit");
        ui.add_space(6.0);
        ui.label(format!(
            "Each K adds the selection to the kit. It goes in {}. \
             Use the name of a file already there to add to that one.",
            dir.display()
        ));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Layout:");
            ui.selectable_value(grid, None, "Free").on_hover_text("Slices end to end, markers wherever they fall");
            ui.selectable_value(grid, Some(crate::editor::AUTO_GRID), "Auto")
                .on_hover_text("Equal slots: 4, growing to 8 then 16 as you add slices");
            for n in crate::editor::GRID_SIZES {
                ui.selectable_value(grid, Some(n), format!("{n} slots")).on_hover_text(format!(
                    "{n} equal slots (Digitakt 1 style): each slice starts a slot, padded with silence to the longest"
                ));
            }
        });
        ui.add_space(8.0);
        let id = Id::unique("name_kit_field");
        let resp = ui.add(egui::TextEdit::singleline(name).id(id).desired_width(f32::INFINITY));
        if !resp.has_focus() && !resp.lost_focus() {
            resp.request_focus();
        }
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            answer = Some(true);
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                answer = Some(false);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Start New SliceKit").on_hover_text("↩").clicked() {
                    answer = Some(true);
                }
            });
        });
    });
    ctx.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            answer = Some(false);
        } else if answer == Some(true) {
            i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
        }
    });
    answer
}

/// "Send to device as…": an edited sample goes up as a new file next to the
/// device file it came from.
pub struct SendBack {
    /// Device folder it goes into.
    pub dir: std::path::PathBuf,
    /// The device file it came from, for the message.
    pub from: String,
    /// New name, without extension.
    pub name: String,
    /// Folder of the local original, when there is one to save beside.
    pub local_dir: Option<std::path::PathBuf>,
    pub keep_local: bool,
}

/// `taken_device`: a device file of that name is listed in `dir`.
/// `local_exists`: the local copy would overwrite a file.
pub fn send_back(
    ctx: &Context,
    f: &mut SendBack,
    taken_device: bool,
    local_exists: bool,
) -> Option<bool> {
    let mut answer = None;
    let clean = f.name.trim().to_owned();
    let valid = !clean.is_empty() && !clean.contains(['/', '\\']);
    Modal::new(Id::unique("send_back_modal")).show(ctx, |ui| {
        ui.set_width(460.0);
        ui.heading("Send to device as");
        ui.add_space(6.0);
        ui.label(format!(
            "Uploads what you hear as a new file in {}, next to \"{}\".",
            crate::device::dev_str(&f.dir),
            f.from
        ));
        ui.label(
            RichText::new(
                "The original stays as it is: projects find samples by their contents, \
                 so replacing it would leave them without the sample.",
            )
            .weak(),
        );
        ui.add_space(8.0);
        let id = Id::unique("send_back_field");
        let resp = ui.add(
            egui::TextEdit::singleline(&mut f.name)
                .id(id)
                .desired_width(ui.available_width()),
        );
        if !resp.has_focus() && !resp.lost_focus() {
            resp.request_focus();
        }
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && valid {
            answer = Some(true);
        }
        if taken_device {
            ui.label(
                RichText::new("That name is taken on the device: a number gets added.")
                    .color(ui.visuals().warn_fg_color),
            );
        }
        if let Some(dir) = &f.local_dir {
            ui.add_space(6.0);
            ui.checkbox(&mut f.keep_local, "Also save it next to the original")
                .on_hover_text(dir.display().to_string());
            if f.keep_local && local_exists {
                ui.label(
                    RichText::new(format!("{clean}.wav exists there and will be overwritten."))
                        .color(ui.visuals().warn_fg_color),
                );
            }
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                answer = Some(false);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(valid, egui::Button::new("⬆ Send"))
                    .on_hover_text("↩")
                    .clicked()
                {
                    answer = Some(true);
                }
            });
        });
    });
    ctx.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            answer = Some(false);
        } else if answer == Some(true) {
            i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
        }
    });
    answer
}
