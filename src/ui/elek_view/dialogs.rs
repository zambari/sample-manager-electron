//! Device push/clear/close/stale confirmations and once-per-frame intent completion.

use super::*;

enum Stale {
    PushAnyway,
    Read,
    Cancel,
}

/// Requests and confirmations common to both layouts.
pub(crate) fn finish(ui: &mut egui::Ui, st: &mut ElekState, out: &mut Vec<ElekIntent>) {
    if let Some((k, open)) = st.want_local.take() {
        out.push(ElekIntent::Local(k, open));
    }
    if let Some(k) = st.want_original.take() {
        out.push(ElekIntent::ShowOriginal(k));
    }
    if let Some(keys) = st.want_update.take() {
        out.push(ElekIntent::UpdateChanged(keys));
    }
    if let Some(pairs) = st.want_files.take() {
        out.push(ElekIntent::AddFiles(pairs));
    }
    if std::mem::take(&mut st.clipped) {
        out.push(ElekIntent::Clipped);
    }
    if let Some(at) = st.want_paste_files.take() {
        out.push(ElekIntent::PasteFiles(at));
    }
    let pass = ui.ctx().cumulative_pass_nr() + 1;
    if st.dialogs_pass == pass {
        return;
    }
    st.dialogs_pass = pass;
    if let Some(r) = st.renaming.as_mut() {
        if let Some(ok) = rename_dialog(ui.ctx(), r) {
            let (key, from, to, _) = st.renaming.take().unwrap_or_default();
            if ok {
                out.push(ElekIntent::RenameFile { key, from, to });
            }
        }
    }
    if st.confirm_close {
        if let Some(yes) = confirm_close(ui.ctx(), st.changed()) {
            st.confirm_close = false;
            if yes {
                st.close_project();
            }
        }
    }
    if let Some(olds) = &st.confirm_clear {
        if let Some(yes) = confirm_clear(ui.ctx(), st, olds) {
            let olds = st.confirm_clear.take().unwrap_or_default();
            if yes {
                st.clear_olds(&olds);
                st.status = format!("Cleared {} used sample(s); what pointed at them now plays nothing", olds.len());
            } else {
                st.status = format!("Kept {} used sample(s)", olds.len());
            }
        }
    }
    if st.confirm_push {
        let whole = st.project.as_ref().is_some_and(|p| p.from_file.is_some());
        if let Some(yes) = confirm_push(ui.ctx(), st.pool_changed(), st.proj.changed(), st.lib.sound_changed(), whole) {
            st.confirm_push = false;
            if yes {
                out.push(ElekIntent::Push { force: false });
            }
        }
    }
    if st.confirm_stale {
        if let Some(a) = confirm_stale(ui.ctx()) {
            st.confirm_stale = false;
            match a {
                Stale::PushAnyway => out.push(ElekIntent::Push { force: true }),
                Stale::Read => out.push(ElekIntent::Read),
                Stale::Cancel => {}
            }
        }
    }
}

fn confirm_stale(ctx: &egui::Context) -> Option<Stale> {
    let mut answer = None;
    egui::Modal::new(egui::Id::unique("elek_stale_modal")).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.heading("The project on the device changed");
        ui.add_space(6.0);
        ui.label(
            "Something changed on the device since the project was read. Even turning a knob counts. \
             Nothing was sent.",
        );
        ui.add_space(4.0);
        ui.label(RichText::new(
            "Push anyway sends the changes as planned: any pattern, kit or pool data it sends replaces \
             what the device has there now, including edits made on the device since the read. \
             Read again first to keep those edits (your unpushed changes here will be lost).",
        ).weak());
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(RichText::new("Push anyway").color(Color32::from_rgb(230, 120, 90))).clicked() {
                answer = Some(Stale::PushAnyway);
            }
            if ui.button("Read again").clicked() {
                answer = Some(Stale::Read);
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                answer = Some(Stale::Cancel);
            }
        });
    });
    answer
}

fn confirm_push(ctx: &egui::Context, pool: usize, patterns: usize, sounds: usize, whole: bool) -> Option<bool> {
    let mut answer = None;
    egui::Modal::new(egui::Id::unique("elek_push_modal")).show(ctx, |ui| {
        ui.set_width(440.0);
        let mut parts = Vec::new();
        if pool > 0 {
            parts.push(format!("{pool} sample slot(s)"));
        }
        if patterns > 0 {
            parts.push(format!("{patterns} pattern slot(s)"));
        }
        if sounds > 0 {
            parts.push(format!("{sounds} pool sound(s)"));
        }
        let what = if whole { "the whole project from the file".to_owned() } else { parts.join(", ") };
        ui.heading(format!("Push {what}?"));
        ui.add_space(6.0);
        if whole {
            ui.label(
                "This replaces the project loaded on the device (its memory) with the one from the file: every pattern, \
                 kit, pool sound and the sample pool. Whatever is in the device's memory now is overwritten, saved or not. \
                 The saved copies on +Drive are not touched.",
            );
            ui.add_space(4.0);
            ui.colored_label(
                Color32::from_rgb(230, 120, 90),
                "Samples are not sent: the pool points at +Drive files by their hash. Any the device doesn't have will play nothing. \
                 Experimental: if the device was holding something you want, save it there (PROJECT › SAVE) before pushing.",
            );
        } else {
            ui.label(
                "This rewrites the project currently loaded on the device: only the patterns, kits and pool sounds \
                 that change are sent, plus the sample pool if it changed. Everything else on the device is left as is. \
                 The saved copy on +Drive is not touched.",
            );
        }
        if sounds > 0 && !whole {
            ui.add_space(4.0);
            ui.colored_label(
                Color32::from_rgb(230, 120, 90),
                "Sound pool editing is experimental. Moved sounds take their sound locks with them in every pattern; \
                 locks on a sound you deleted are removed. Kit tracks keep their own copies of sounds and don't change.",
            );
        }
        if patterns > 0 && !whole {
            ui.add_space(4.0);
            ui.colored_label(
                Color32::from_rgb(230, 120, 90),
                "Pattern editing is experimental (Digitakt 1). Each changed slot is sent as a whole pattern with its kit, \
                 replacing what the device has there now, including edits made on the device since the read.",
            );
        }
        ui.add_space(4.0);
        ui.label(RichText::new("Wait for the device's own progress bar to finish. If it sounds right, save via PROJECT › SAVE. If anything is off, don't save: reload the project from +Drive.").weak());
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(RichText::new("Push to device").color(theme::DEVICE_ACCENT_BRIGHT)).clicked() {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                answer = Some(false);
            }
        });
    });
    answer
}

fn confirm_clear(ctx: &egui::Context, st: &ElekState, olds: &[u16]) -> Option<bool> {
    let Some(p) = &st.project else { return Some(false) };
    let mut answer = None;
    egui::Modal::new(egui::Id::unique("elek_clear_modal")).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.heading(format!("Clear {} used sample(s)?", olds.len()));
        ui.add_space(6.0);
        if !st.status.is_empty() {
            ui.label(RichText::new(format!("{}.", st.status)).weak());
        }
        ui.label(
            "These samples are still used by kit tracks, sample locks or pool sounds. \
             Clearing them disconnects those sounds: they will play nothing after the push.",
        );
        ui.add_space(4.0);
        egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
            for &o in olds {
                let slot = st.order.iter().position(|&x| x == o).unwrap_or(o as usize);
                let e = st.entry(o as usize);
                let name = st
                    .names
                    .get(&e.key())
                    .map(|n| n.rsplit('/').next().unwrap_or(n).to_owned())
                    .unwrap_or_else(|| format!("#{:08x}", e.hash));
                ui.label(format!("{}  {name}  (used {}×)", st.label(slot), p.uses[o as usize]));
            }
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(RichText::new("Clear anyway").color(Color32::from_rgb(230, 120, 90))).clicked() {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                answer = Some(false);
            }
        });
    });
    answer
}

/// `Some(true)` to rename, `Some(false)` to cancel.
fn rename_dialog(ctx: &egui::Context, r: &mut ((u32, u32), String, String, bool)) -> Option<bool> {
    let (_, path, name, focus) = r;
    let mut answer = None;
    egui::Modal::new(egui::Id::unique("elek_rename_modal")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.heading("Rename on +Drive");
        ui.add_space(4.0);
        ui.label(RichText::new(path.as_str()).weak());
        ui.add_space(6.0);
        let te = ui.add(egui::TextEdit::singleline(name).desired_width(f32::INFINITY));
        if std::mem::take(focus) {
            te.request_focus();
            // Select the stem, leaving the extension alone.
            let stem = name.rfind('.').unwrap_or(name.len());
            let stem = name[..stem].chars().count();
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), te.id) {
                state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(stem),
                )));
                state.store(ui.ctx(), te.id);
            }
        }
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "Only the file's name changes. Projects refer to samples by content hash, \
                 so this one and every other project using the file keep working.",
            )
            .weak(),
        );
        ui.add_space(8.0);
        let enter = te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.horizontal(|ui| {
            let ok = !name.trim().is_empty() && !name.contains('/');
            if ui.add_enabled(ok, egui::Button::new("Rename")).clicked() || (ok && enter) {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                answer = Some(false);
            }
        });
    });
    answer
}

fn confirm_close(ctx: &egui::Context, changed: usize) -> Option<bool> {
    let mut answer = None;
    egui::Modal::new(egui::Id::unique("elek_close_modal")).show(ctx, |ui| {
        ui.set_width(400.0);
        ui.heading("Discard unpushed changes?");
        ui.add_space(6.0);
        ui.label(format!("{changed} slot(s) have been moved but not pushed to the device. Closing the project drops these changes."));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(RichText::new("Discard and close").color(Color32::from_rgb(230, 120, 90))).clicked() {
                answer = Some(true);
            }
            if ui.button("Keep editing").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                answer = Some(false);
            }
        });
    });
    answer
}
