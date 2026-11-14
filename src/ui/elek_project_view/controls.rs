//! Pattern-page entry point, toolbar controls and keyboard dispatch.

use super::*;

pub fn show(ui: &mut egui::Ui, st: &mut ElekState) -> Vec<ElekIntent> {
    let mut out = Vec::new();
    if let Some(ctl) = &st.reading {
        ctl.open.store(st.proj.bank_mask(), std::sync::atomic::Ordering::Relaxed);
    }
    elek_view::top(ui, st, &mut out, |ui, st| {
        let busy = st.edit_locked() || st.proj.cells.is_empty();
        let ed = &mut st.proj;
        if ui.add_enabled(!busy && !ed.undo.is_empty(), egui::Button::new("↶ Undo")).on_hover_text("Undo the last pattern edit (⌘Z)").clicked() {
            ed.undo();
        }
        let changed = ed.changed();
        if ui.add_enabled(!busy && changed > 0, egui::Button::new("↺ Reset")).on_hover_text("Back to the patterns as read from the device").clicked() {
            ed.edit(|c| *c = (0..PATTERNS).map(Cell::as_read).collect());
        }
    });
    if st.project.is_none() {
        elek_view::load_screen(ui, st, &mut out);
        elek_view::finish(ui, st, &mut out);
        return out;
    }
    let busy = st.edit_locked();

    let msg = match st.project.as_ref().map(|p| &p.patterns) {
        Some(Err(e)) if st.project.as_ref().is_some_and(|p| p.partial) => Some(format!("Patterns: {e}…")),
        Some(Err(e)) => Some(format!("Pattern editing unavailable: {e}")),
        _ => None,
    };
    if let Some(m) = msg {
        ui.add_space(8.0);
        ui.colored_label(WARN, m);
        elek_view::finish(ui, st, &mut out);
        return out;
    }
    let Some(p) = st.project.as_ref().and_then(|p| p.patterns.as_deref().ok()) else {
        elek_view::finish(ui, st, &mut out);
        return out;
    };
    if let Some(why) = &p.read_only {
        ui.colored_label(WARN, format!("Read-only: {why}. Details in the log (lines starting elek-pat:)."));
    }
    let n = p.loaded_count();
    if n < PATTERNS {
        let text = if st.reading.is_some() {
            format!("Loading patterns: {n} of {PATTERNS}. Folded-out banks load first; a bank can be edited once all of it is here.")
        } else {
            format!("The read was stopped with {n} of {PATTERNS} patterns: only banks that loaded completely can be edited and pushed. Reload for the rest.")
        };
        ui.label(RichText::new(text).weak());
    }
    let audio = st.project.as_ref().map_or(8, |p| p.model.kit_tracks());
    if let Some(c) = &st.proj.confirm {
        if let Some(yes) = confirm_tracks(ui.ctx(), c) {
            let c = st.proj.confirm.take().expect("checked above");
            st.status = if yes && !busy { apply_tracks(&mut st.proj, &c.into, c.part, audio) } else { "Paste cancelled".into() };
        }
    }
    keys(ui.ctx(), st);
    action_bar(ui, st, busy);
    ui.label(
        RichText::new("Click / ⇧click / ⌘click to select · drag to move (swaps with what's there) · ⌥ while dropping copies · ⌘C / ⌘V · click a track name in a folded-out row to select tracks (⌘ / ⇧ for more), ⌘C, then ⌘V or right-click a pattern to paste them in · ⌫ clears · ⏎ or double-click renames · ←/→ fold · renaming sets the pattern name and the kit name · patterns in songs aren't updated when moved")
            .weak()
            .small(),
    );
    list(ui, st, busy);
    finish_sound_actions(st, ui.ctx());
    out.append(&mut st.proj.want);
    if let Some(m) = st.proj.refused.take() {
        st.status = m;
    }
    elek_view::finish(ui, st, &mut out);
    out
}

fn keys(ctx: &egui::Context, st: &mut ElekState) {
    if ctx.egui_wants_keyboard_input() || st.edit_locked() || st.proj.confirm.is_some() {
        return;
    }
    let audio = st.project.as_ref().map_or(8, |p| p.model.kit_tracks());
    let Some(p) = st.project.as_ref().and_then(|p| p.patterns.as_deref().ok()) else { return };
    if p.read_only.is_some() {
        return;
    }
    let ed = &mut st.proj;
    let (m, l, r, u, d, esc, z, a, del, enter, c, v, ev_copy, ev_paste) = ctx.input(|i| {
        (
            i.modifiers,
            i.key_pressed(egui::Key::ArrowLeft),
            i.key_pressed(egui::Key::ArrowRight),
            i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::ArrowDown),
            i.key_pressed(egui::Key::Escape),
            i.key_pressed(egui::Key::Z),
            i.key_pressed(egui::Key::A),
            i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace),
            i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::F2),
            i.key_pressed(egui::Key::C),
            i.key_pressed(egui::Key::V),
            i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
            i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))),
        )
    });
    let mut status = None;
    let vis = ed.visible(p);
    if m.command && z {
        ed.undo();
    }
    if m.command && a {
        ed.sel = vis.iter().copied().collect();
    }
    let tracks_on = !ed.tsel.is_empty();
    if esc {
        ed.sel.clear();
        ed.clear_tracks();
    }
    // With tracks selected, ⌫ and ⏎ would hit the whole pattern: leave them.
    if del && !tracks_on && !ed.targets().is_empty() {
        status = Some(clear(ed, p));
    }
    if ev_copy || (m.command && c) {
        let msg = if let Some(target) = ed.sound_selected {
            ed.sound_copy = Some(target);
            "Copying track sound…".into()
        } else if tracks_on { copy_tracks(ed, Part::Track, audio) } else { copy(ed) };
        // ⌘V only arrives when the system clipboard holds text.
        ctx.copy_text(msg.clone());
        status = Some(msg);
    }
    if ev_paste || (m.command && v) {
        status = Some(if ed.pool_clip.is_some() {
            match ed.tsel_pat.filter(|_| tracks_on) {
                Some(i) => {
                    let ts: Vec<u8> = ed.tsel.iter().copied().collect();
                    request_sound_paste(ed, i, &ts)
                }
                None => "Select the track(s) to paste the copied sound onto (click a track name in a folded-out pattern), then ⌘V".into(),
            }
        } else if ed.sound_selected.is_some() {
            "Copy a single sound on Sounds, Presets, or Patterns first".into()
        } else if !ed.has_tclip() {
            paste(ed)
        } else {
            let into = ed.track_targets();
            paste_tracks(ed, p, into, Part::Track, audio)
        });
    }
    if enter && !tracks_on {
        match ed.targets().as_slice() {
            [i] => ed.start_inline(p, *i),
            [] => {}
            _ => ed.focus_name = true,
        }
    }
    if l || r {
        for i in ed.targets() {
            if r {
                ed.open.insert(i);
            } else {
                ed.open.remove(&i);
            }
        }
    }
    let step: isize = if u { -1 } else if d { 1 } else { 0 };
    if step != 0 && !m.command && !vis.is_empty() {
        let pos = ed.cursor.and_then(|c| vis.iter().position(|&v| v >= c)).unwrap_or(0) as isize;
        let pos = if ed.cursor.is_some() { pos + step } else { 0 };
        let next = vis[pos.clamp(0, vis.len() as isize - 1) as usize];
        if m.shift {
            let anchor = ed.cursor.unwrap_or(next);
            ed.sel.insert(anchor);
            ed.sel.insert(next);
            ed.cursor = Some(next);
        } else {
            ed.select_only(next);
        }
        ed.scroll_to = Some(next);
    }
    if let Some(s) = status {
        st.status = s;
    }
}

/// View options, beside the Samples / Patterns tabs.
pub(crate) fn view_tools(ui: &mut egui::Ui, st: &mut ElekState) {
    if st.project.as_ref().and_then(|p| p.patterns.as_deref().ok()).is_none() {
        return;
    }
    let ed = &mut st.proj;
    ui.toggle_value(&mut ed.used_only, "Hide empty").on_hover_text("Hide empty slots (selected and changed ones stay), tracks without trigs, and their sounds");
    ui.toggle_value(&mut ed.bytes, "Bytes").on_hover_text(
        "Folded-out patterns show raw bytes instead of steps: the last 32 bytes of each track, and the bytes after the pattern name. \
         Offsets are from the end of the track (-32..-1) and from the start of the pattern payload",
    );
    let all_open = ed.banks.iter().all(|&b| b);
    let all_folded = ed.banks.iter().all(|&b| !b);
    if ui.add_enabled(!all_open, egui::Button::new("Open all banks").small()).clicked() {
        ed.banks = [true; BANKS];
    }
    if ui.add_enabled(!all_folded, egui::Button::new("Fold all banks").small()).clicked() {
        ed.banks = [false; BANKS];
    }
}

/// What to do with the selection: copy / paste / clear / pack / rename.
fn action_bar(ui: &mut egui::Ui, st: &mut ElekState, busy: bool) {
    let audio = st.project.as_ref().map_or(8, |p| p.model.kit_tracks());
    let Some(p) = st.project.as_ref().and_then(|p| p.patterns.as_deref().ok()) else { return };
    let editable = p.read_only.is_none() && !busy;
    let ed = &mut st.proj;
    let t = ed.targets();
    // Refill the template field when the selection changes.
    if ed.name_for != t {
        ed.name_for = t.clone();
        ed.name_edit = if t.len() > 1 { "{name}".into() } else { String::new() };
    }
    let mut status = None;
    ui.horizontal_wrapped(|ui| {
        let what = match t.as_slice() {
            [] => "Nothing selected".to_owned(),
            [i] => format!("{} selected", patterns::label(*i)),
            _ => format!("{} selected", t.len()),
        };
        ui.label(RichText::new(what).strong());
        let any = !t.is_empty();
        if ui.add_enabled(any, egui::Button::new("Copy")).clicked() {
            status = Some(copy(ed));
        }
        if ui.add_enabled(editable && !ed.clip.is_empty(), egui::Button::new(format!("Paste ({})", ed.clip.len()))).clicked() {
            status = Some(paste(ed));
        }
        if ed.has_tclip() {
            let into = ed.track_targets();
            ui.add_enabled_ui(editable, |ui| {
                ui.menu_button("Paste tracks ▾", |ui| {
                    if let Some(s) = paste_items(ui, ed, p, &into, editable, audio) {
                        status = Some(s);
                    }
                })
                .response
                .on_hover_text("Paste copied tracks into the selected pattern(s)");
            });
        }
        if ui.add_enabled(editable && any && p.can_clear(), egui::Button::new("Clear")).clicked() {
            status = Some(clear(ed, p));
        }
        if ui.add_enabled(editable && any, egui::Button::new("Pack bank")).on_hover_text("Move the used patterns of the selected bank(s) to the front, in order").clicked() {
            status = Some(pack(ed, p));
        }
        match t.as_slice() {
            [i] => {
                if ui.add_enabled(editable, egui::Button::new("Rename ⏎")).on_hover_text(format!("Rename the kit (up to {NAME_MAX} characters, upper case)")).clicked() {
                    ed.start_inline(p, *i);
                }
            }
            [] => {}
            _ => {
                ui.separator();
                ui.label("Rename all");
                let preview: Vec<String> = t
                    .iter()
                    .take(3)
                    .enumerate()
                    .map(|(k, &i)| patterns::clean_name(&ed.name_edit.replace("{n}", &(k + 1).to_string()).replace("{slot}", &patterns::label(i)).replace("{name}", &view(p, &ed.cells[i]).1)))
                    .collect();
                let te = egui::TextEdit::singleline(&mut ed.name_edit).char_limit(64).font(oled_font(13.0)).desired_width(140.0);
                let r = ui
                    .add_enabled(editable, te)
                    .on_hover_text(format!("{{n}} = 1, 2, 3…   {{slot}} = A01…   {{name}} = current name\n→ {}{}", preview.join(", "), if t.len() > 3 { ", …" } else { "" }));
                if ed.focus_name {
                    ed.focus_name = false;
                    r.request_focus();
                }
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.add_enabled(editable, egui::Button::new("Apply")).clicked() || (enter && editable) {
                    let text = ed.name_edit.clone();
                    status = Some(rename(ed, p, &t, &text));
                    ed.name_for.clear();
                }
            }
        }
    });
    if let Some(s) = status {
        st.status = s;
    }
}

