//! Sample-slot context commands and hover text built from shared row facts.

use super::*;

pub(super) fn slot_menu(ui: &mut egui::Ui, st: &ElekState, row: &SlotRowInfo<'_>, acts: &mut Vec<RowAct>) {
    let Some(p) = st.project.as_ref() else { return };
    let SlotRowInfo { slot, from, entry: e, linked, changed, selected, .. } = *row;
    let n_sel = st.sel.len();
    if !e.is_empty() {
        let (play, open, tip) = if linked.is_some() {
            ("Play local copy  (Space)", "Open local copy in editor", "Nothing is fetched from the device")
        } else {
            ("Fetch and play  (Space)", "Fetch and open in editor", "Copied into the preview cache first; other fetches wait")
        };
        if ui.button(play).on_hover_text(tip).clicked() {
            acts.push(RowAct::Local(e.key(), false));
            ui.close();
        }
        if ui.button(open).on_hover_text(tip).clicked() {
            acts.push(RowAct::Local(e.key(), true));
            ui.close();
        }
        if changed && st.pool_editable() {
            let all: Vec<(u32, u32)> = st.changed_local.keys().copied().collect();
            if ui
                .button("⬆ Update on device from edited file")
                .on_hover_text("Upload the edited local file next to the +Drive original (which stays, other projects may use it), put it in this slot and push")
                .clicked()
            {
                acts.push(RowAct::UpdateChanged(vec![e.key()]));
                ui.close();
            }
            if all.len() > 1 && ui.button(format!("⬆ Update all {} edited samples", all.len())).clicked() {
                acts.push(RowAct::UpdateChanged(all));
                ui.close();
            }
        }
        // Uploaded from a local file: point at it, so it's clear which
        // of several same-named copies is the one on the device.
        if let Some((_, local, true)) = linked {
            if ui
                .button("Show original in source pane")
                .on_hover_text(format!("The file it was uploaded from:\n{}", local.display()))
                .clicked()
            {
                acts.push(RowAct::ShowOriginal(e.key()));
                ui.close();
            }
        }
        // Only once the device told us where the file lives.
        if let Some(path) = st.names.get(&e.key()) {
            if ui
                .button("Rename on +Drive…")
                .on_hover_text(format!(
                    "{path}\nProjects find samples by content hash, so every project using it keeps working"
                ))
                .clicked()
            {
                acts.push(RowAct::Rename(e.key()));
                ui.close();
            }
        }
        ui.separator();
    }
    // Cut / copy: the selection when right-clicking inside it, else this row.
    let pick: Vec<usize> = if selected { st.sel.iter().copied().collect() } else { vec![slot] };
    let filled = pick.iter().filter(|&&s| st.key_at(s).is_some()).count();
    if filled > 0 {
        let what = if filled == 1 { "sample".to_owned() } else { format!("{filled} samples") };
        if ui
            .button(format!("Cut {what}  (⌘X)"))
            .on_hover_text("Then paste into this pool to move them; the slots they leave become empty. Nothing changes until you paste")
            .clicked()
        {
            acts.push(RowAct::Clip(pick.clone(), true));
            ui.close();
        }
        if ui
            .button(format!("Copy {what}  (⌘C)"))
            .on_hover_text("Then paste into a local file pane to download them there, as when dragging slots out")
            .clicked()
        {
            acts.push(RowAct::Clip(pick, false));
            ui.close();
        }
    }
    let cut_n = st.clip.as_ref().filter(|c| c.cut).map_or(0, |_| st.clip_positions().len());
    if cut_n > 0 {
        if ui
            .button(format!("Paste {cut_n} into empty slots from here  (⌘V)"))
            .on_hover_text("Fills the empty slots from this one down, skipping filled ones; nothing else moves. The cut slots become empty")
            .clicked()
        {
            acts.push(RowAct::Paste(slot, false));
            ui.close();
        }
        if ui
            .button(format!("Paste {cut_n} here, shifting down  (⇧⌘V)"))
            .on_hover_text("Inserts at this slot; this and the samples below move down, each into the next empty slot further down. The cut slots become empty")
            .clicked()
        {
            acts.push(RowAct::Paste(slot, true));
            ui.close();
        }
        if ui.button("Cancel cut").clicked() {
            acts.push(RowAct::DropClip);
            ui.close();
        }
    }
    if filled > 0 || cut_n > 0 {
        ui.separator();
    }
    if ui
        .button("Insert empty slot here")
        .on_hover_text(
            "Shift this and the samples below down by one, up to the next empty slot",
        )
        .clicked()
    {
        acts.push(RowAct::InsertEmpty(slot, 1));
        ui.close();
    }
    // Several selected: make room for as many, above the selection.
    if selected && n_sel > 1 {
        let top = st.sel.first().copied().unwrap_or(slot);
        if ui
            .button(format!("Insert {n_sel} empty slots above selection"))
            .on_hover_text(format!(
                "Shift {} and everything below down by {n_sel}, using up the next {n_sel} empty slots further down",
                st.label(top)
            ))
            .clicked()
        {
            acts.push(RowAct::InsertEmpty(top, n_sel));
            ui.close();
        }
    }
    if st.free(from as u16)
        && ui
            .button("Remove this empty slot")
            .on_hover_text("Pull the samples below up by one, up to the next empty slot")
            .clicked()
    {
        acts.push(RowAct::CloseGap(slot));
        ui.close();
    }
    if n_sel > 0 && !(n_sel == 1 && selected) {
        ui.separator();
        if ui
            .button(format!("Move {n_sel} selected here"))
            .on_hover_text("Insert the selection above this slot")
            .clicked()
        {
            acts.push(RowAct::MoveHere(slot));
            ui.close();
        }
    }
    if selected {
        ui.separator();
        if ui.button("Move selection up  (⌥↑)").clicked() {
            acts.push(RowAct::Nudge(-1));
        }
        if ui.button("Move selection down  (⌥↓)").clicked() {
            acts.push(RowAct::Nudge(1));
        }
    }
    // Clearing: the selection when right-clicking inside it, else this row.
    let targets: Vec<usize> = if selected {
        st.sel.iter().copied().filter(|&s| !st.free(st.order[s])).collect()
    } else if !st.free(from as u16) {
        vec![slot]
    } else {
        Vec::new()
    };
    let counted = !p.partial;
    ui.separator();
    if !targets.is_empty() {
        let used = targets.iter().filter(|&&s| p.uses[st.order[s] as usize] > 0).count();
        let label = if targets.len() == 1 {
            "Clear sample from project".to_owned()
        } else {
            format!("Clear {} samples from project", targets.len())
        };
        let tip = if used > 0 {
            format!("Unused ones are cleared right away; {used} still used by kits, locks or sounds will ask first")
        } else {
            "Empties the slot in the pool; the file stays on the +Drive. Undo with ⌘Z".to_owned()
        };
        if ui.add_enabled(counted, egui::Button::new(label)).on_hover_text(tip).on_disabled_hover_text("Wait for the read to finish").clicked() {
            acts.push(RowAct::Clear(targets));
            ui.close();
        }
    }
    if ui
        .add_enabled(counted, egui::Button::new("Clear all unused samples"))
        .on_hover_text("Empties every slot nothing in the project points at; the files stay on the +Drive. Undo with ⌘Z")
        .on_disabled_hover_text("Wait for the read to finish")
        .clicked()
    {
        acts.push(RowAct::ClearUnused);
        ui.close();
    }
}

pub(super) fn slot_tooltip(st: &ElekState, row: &SlotRowInfo<'_>) -> String {
    let SlotRowInfo { slot, from, entry: e, linked, moved, uses, .. } = *row;
    let mut tip = st
        .names
        .get(&e.key())
        .cloned()
        .unwrap_or_else(|| "(not found on +Drive)".into());
    if let Some((_, local, original)) = linked {
        let cached = crate::links::pool_cache_dir().is_some_and(|c| local.starts_with(c));
        tip.push_str(&format!(
            "\n{}: {}",
            if *original {
                "uploaded from"
            } else if cached {
                "in the preview cache"
            } else {
                "fetched to"
            },
            local.display()
        ));
    } else if st.fetching.contains(&e.key()) {
        tip.push_str("\nqueued for the preview cache");
    }
    if st.fills.contains_key(&(from as u16)) {
        tip.push_str("\nnew in this slot: goes to the device with the next push");
    }
    tip.push_str(&format!("\npool slot {slot}"));
    if moved {
        tip.push_str(&format!(", was {}", st.label(from)));
    }
    if from != slot && uses > 0 {
        tip.push_str("\npatterns using it get renumbered on push");
    }
    tip.push_str(&format!(
        "\nused {uses}× (once per pattern track, plus pool sounds)"
    ));
    tip
}
