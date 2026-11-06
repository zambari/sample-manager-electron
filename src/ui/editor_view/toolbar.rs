//! The editor's header, slot bar, toolbar and position readouts.

use super::*;

pub(super) fn header(ui: &mut Ui, ed: &mut Editor, focused: bool, out: &mut Vec<EdIntent>) {
    ui.horizontal(|ui| {
        if ui.add(crate::ui::theme::pane_title("Sample Editor", focused)).clicked() {
            out.push(EdIntent::TookFocus);
        }
        ui.separator();
        if let Some(p) = ed.path() {
            let mut name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            if ed.is_dirty() {
                name.push_str("  ● unsaved");
            } else if ed.is_edited() {
                name.push_str("  •");
            }
            ui.add(egui::Label::new(RichText::new(name).strong()).truncate())
                .on_hover_text(p.display().to_string());
        }
        if let (Some((a, b)), Some(s)) = (ed.crop, ed.sample()) {
            ui.label(RichText::new(format!("✂ cropped view {:.3}–{:.3}s", s.seconds(a), s.seconds(b))).weak())
                .on_hover_text("Only part of the file is loaded for editing. ⇧C or Uncrop shows the whole file again.");
        }
        if let Some(f) = ed.fold() {
            let txt = if f == Fold::Mix { "◐ MONO".to_owned() } else { format!("◐ MONO {}", f.short()) };
            ui.label(RichText::new(txt).color(MONO_COLOR).strong())
                .on_hover_text("Playing folded to mono. M toggles.");
        }
        if ed.loading.is_some() {
            ui.spinner();
        }
    });
    panel_toggles(ui, &mut ed.prefs.panels);
}

/// Sample / Kit switch, plus the kit's own buttons.
pub(super) fn slot_bar(ui: &mut Ui, ed: &Editor, s: &Slots, out: &mut Vec<EdIntent>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let sample = s.sample_name.as_deref().unwrap_or("—");
        if ui
            .selectable_label(!s.showing_kit, format!("Sample: {}", crate::app::short_name(sample, 28)))
            .on_hover_text("B — the sample you're browsing. Moving the file cursor always loads here.")
            .clicked()
        {
            out.push(EdIntent::ShowKit(false));
        }
        // No kit yet: nothing to show until K starts one.
        if let Some(k) = &s.kit_name {
            kit_tab(ui, k, s, out);
            kit_switcher(ui, ed, s, out);
            grid_picker(ui, s.kit_grid, out);
            if !s.showing_kit
                && ui
                    .add_enabled(ed.current.is_some(), egui::Button::new("→").small())
                    .on_hover_text(format!("K — add the selection (as you hear it) to the end of {k}"))
                    .clicked()
            {
                out.push(EdIntent::AddSlice);
            }
            if ui
                .add_enabled(!s.kit_dirty, egui::Button::new("✕").small())
                .on_hover_text(if s.kit_dirty { "Save the kit (⌘S in kit view) before closing it" } else { "Close the kit (the file stays)" })
                .clicked()
            {
                out.push(EdIntent::CloseKit);
            }
        }
        if !s.showing_kit && ed.current.is_some() {
            ui.separator();
            if ui
                .small_button("Use as kit")
                .on_hover_text("Make this file the slice kit, to add slices to its end")
                .clicked()
            {
                out.push(EdIntent::UseAsKit);
            }
        }
    });
}

/// The Kit tab. Clicking it while the kit is showing (or double-clicking it)
/// turns it into a name field: ↩ or clicking away renames, Esc cancels.
fn kit_tab(ui: &mut Ui, k: &str, s: &Slots, out: &mut Vec<EdIntent>) {
    let buf_id = egui::Id::unique("kit_rename_buf");
    let field_id = egui::Id::unique("kit_rename_field");
    let mut buf: Option<String> = ui.data(|d| d.get_temp(buf_id));
    if !s.showing_kit {
        buf = None;
    }
    if let Some(text) = &mut buf {
        ui.label("Kit:");
        let resp = ui.add(egui::TextEdit::singleline(text).id(field_id).desired_width(180.0));
        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
        if !resp.has_focus() && !resp.lost_focus() {
            resp.request_focus();
        }
        if resp.lost_focus() || esc {
            // Keep ↩ / Esc from also reaching the editor's keymap (export).
            ui.ctx().input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
            });
            if !esc {
                out.push(EdIntent::RenameKit(text.clone()));
            }
            buf = None;
        }
    } else {
        let mut label = format!("Kit: {}", crate::app::short_name(k, 28));
        if s.kit_dirty {
            label.push_str(" ●");
        }
        let resp = ui.selectable_label(s.showing_kit, label).on_hover_text(
            "B — the slice kit. K adds the sample's selection to its end. ⌘S here saves the whole kit.\n\
             Click again (or double-click) to rename it — a saved kit's file is renamed too.",
        );
        if resp.double_clicked() || (resp.clicked() && s.showing_kit) {
            let stem = k.strip_suffix(".wav").or_else(|| k.strip_suffix(".WAV")).unwrap_or(k);
            buf = Some(stem.to_owned());
        }
        if resp.clicked() && !s.showing_kit {
            out.push(EdIntent::ShowKit(true));
        }
    }
    ui.data_mut(|d| match buf {
        Some(b) => {
            d.insert_temp(buf_id, b);
        }
        None => {
            d.remove::<String>(buf_id);
        }
    });
}

/// ▾ menu next to the Kit tab: the open kits, to pick which one K adds to,
/// and "New kit" to start another.
fn kit_switcher(ui: &mut Ui, ed: &Editor, s: &Slots, out: &mut Vec<EdIntent>) {
    let label = if s.kits.len() > 1 { format!("▾ {}", s.kits.len()) } else { "▾".to_owned() };
    ui.menu_button(label, |ui| {
        for k in &s.kits {
            let mut text = k.name.clone();
            if k.dirty {
                text.push_str(" ●");
            }
            let resp = ui.add(egui::Button::selectable(k.parked.is_none(), text));
            if resp.clicked() {
                if let Some(i) = k.parked {
                    out.push(EdIntent::SwitchKit(i));
                }
                ui.close();
            }
        }
        ui.separator();
        let can_new = !s.showing_kit && ed.current.is_some();
        if ui
            .add_enabled(can_new, egui::Button::new("New kit from selection…"))
            .on_hover_text("Start another kit with this selection; the open ones stay in this list")
            .on_disabled_hover_text("Go to the sample (B) and select something first")
            .clicked()
        {
            out.push(EdIntent::NewKit);
            ui.close();
        }
    })
    .response
    .on_hover_text("Open kits — pick the one K adds to. Right-click a file → Use as kit to open another.");
}

/// Free / 4 / 8 / 16 switch for the kit's layout.
fn grid_picker(ui: &mut Ui, cur: Option<usize>, out: &mut Vec<EdIntent>) {
    let label = |g: Option<usize>| g.map_or("Free".to_owned(), |n| format!("{n} slots"));
    egui::ComboBox::from_id_salt("kit_grid")
        .selected_text(label(cur))
        .width(70.0)
        .show_ui(ui, |ui| {
            for g in std::iter::once(None).chain(crate::editor::GRID_SIZES.map(Some)) {
                if ui.selectable_label(cur == g, label(g)).clicked() && cur != g {
                    out.push(EdIntent::SetGrid(g));
                }
            }
        })
        .response
        .on_hover_text(
            "Free: slices go end to end, wherever they fall.\n\
             N slots: the kit is split into N equal parts (what Digitakt 1 can slice), each slice at the start of \
             its slot, padded with silence to the longest one. K fills the next free slot; cuts and pastes snap back onto the grid.",
        );
}

/// Right-aligned chips that show / hide each editor panel. Saved with the prefs.
pub(super) fn panel_toggles(ui: &mut Ui, p: &mut crate::editor::Panels) {
    section_header(ui, "Show");
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let items: [(&mut bool, &str, &str); 9] = [
            (&mut p.transport, "Transport", "Play, loop, follow, snap and auto-load / auto-play"),
            (&mut p.gain, "Gain", "Gain and normalize"),
            (&mut p.tools, "Tools", "Undo, crop, loop scaling, export"),
            (&mut p.wave, "Wave", "Main waveform"),
            (&mut p.nums, "Nums", "In / out / len / playhead readout, under the waveform"),
            (&mut p.scalpel, "Scalpel", "40 ms extreme-zoom strip around the playhead"),
            (&mut p.minimap, "Map", "Minimap: the whole file, click to jump"),
            (&mut p.stats, "Stats", "Stats table and phase meter"),
            (&mut p.help, "Help", "Keyboard shortcut list"),
        ];
        for (v, label, hover) in items {
            ui.add(vis_toggle(v, label)).on_hover_text(format!("Show / hide: {hover}"));
        }
    });
}

pub(super) fn toolbar(ui: &mut Ui, ed: &mut Editor, pv: &PlayView, can_export: bool, slots: &Slots, out: &mut Vec<EdIntent>) {
    let showing_kit = slots.showing_kit;
    let save_kit = showing_kit && slots.kit_saves_here;
    let playing = pv.playing;
    let has = ed.current.is_some();
    let pn = ed.prefs.panels.clone();
    if pn.transport {
    section_header(ui, "Transport");
    ui.horizontal_wrapped(|ui| {
        let icon = if playing { "⏹" } else { "▶" };
        let hover = if playing { "Stop (Space)" } else { "Play (Space)" };
        if ui.add_enabled(has, egui::Button::new(icon)).on_hover_text(hover).clicked() {
            out.push(EdIntent::TogglePlay);
        }
        // One button that flips the start point.
        let (label, hover) = if ed.prefs.play_from_in {
            ("[ Playhead", "Space plays from the in point (⇧Space: from the current position). Click to switch.")
        } else {
            ("▸ From current", "Space plays from the parked playhead (⇧Space: from the in point). Click to switch.")
        };
        if ui.button(label).on_hover_text(hover).clicked() {
            ed.prefs.play_from_in = !ed.prefs.play_from_in;
        }
        ui.separator();
        ui.toggle_value(&mut ed.prefs.looping, "🔁 Loop").on_hover_text("L — loop the in/out region");
        ui.toggle_value(&mut ed.prefs.follow_playback, "Follow")
            .on_hover_text("V — scroll the view along with the play cursor. Turn off to stay put while zoomed in on a loop end.");
        if ui
            .toggle_value(&mut ed.prefs.snap_zero, "Snap to 0")
            .on_hover_text("S — snap in/out points to the nearest zero crossing. Turning it on snaps the current selection.")
            .changed()
            && ed.prefs.snap_zero
        {
            ed.snap_selection();
        }
        // Nothing auto-loads into the kit, so these only matter for the sample.
        if !showing_kit {
            ui.separator();
            ui.toggle_value(&mut ed.prefs.auto_load, "Auto-load")
                .on_hover_text("Load the sample under the file cursor");
            ui.toggle_value(&mut ed.prefs.auto_play, "Auto-play")
                .on_hover_text("Play each sample as the cursor lands on it");
        }
    });
    }
    if pn.gain {
    section_header(ui, "Gain");
    ui.horizontal_wrapped(|ui| {
        ui.add_enabled_ui(has, |ui| {
            ui.label("Gain");
            ui.add(
                egui::Slider::new(&mut ed.gain_db, -36.0..=36.0)
                    .suffix(" dB")
                    .step_by(0.1)
                    .clamping(egui::SliderClamping::Never),
            )
            .on_hover_text("Double-click the number to type a value");
            if ui.button("Normalize").on_hover_text("N — set gain so the region peaks at the target").clicked() {
                ed.normalize();
            }
            ui.add(
                egui::DragValue::new(&mut ed.prefs.normalize_db)
                    .range(-24.0..=0.0)
                    .speed(0.05)
                    .suffix(" dB"),
            )
            .on_hover_text("Normalize target peak");
            if ui.button("Reset").on_hover_text("Whole file, 0 dB").clicked() {
                ed.reset();
            }
        });
    });
    }
    if pn.tools {
    section_header(ui, "Tools");
    ui.horizontal_wrapped(|ui| {
        ui.add_enabled_ui(has, |ui| {
            if ui.add_enabled(ed.can_undo(), egui::Button::new("↶ Undo")).on_hover_text("⌘Z").clicked() {
                ed.undo();
            }
            if ui.add_enabled(ed.can_redo(), egui::Button::new("↷ Redo")).on_hover_text("⇧⌘Z").clicked() {
                ed.redo();
            }
            ui.separator();
            let whole = ed.sel == (0, ed.frames());
            if ui
                .add_enabled(ed.can_remove(), egui::Button::new("Cut"))
                .on_hover_text("⌘X — take the selection out of the audio, onto the clipboard")
                .clicked()
            {
                ed.cut();
            }
            if ui.button("Copy").on_hover_text("⌘C — copy the selection (with gain) to the clipboard. It stays there when you open another file.").clicked() {
                ed.copy();
            }
            let paste_tip = match &ed.clip {
                Some(c) if whole => format!("⌘V — insert {:.3}s at the playhead", c.frames() as f64 / c.rate.max(1) as f64),
                Some(c) => format!("⌘V — replace the selection with {:.3}s", c.frames() as f64 / c.rate.max(1) as f64),
                None => "⌘V — nothing copied yet".to_owned(),
            };
            if ui.add_enabled(ed.can_paste(), egui::Button::new("Paste")).on_hover_text(paste_tip).clicked() {
                ed.paste();
            }
            if ui
                .button("+100 ms")
                .on_hover_text("G — insert 100 ms of silence just before the playhead, pushing everything after it (slice markers too) later")
                .clicked()
            {
                ed.insert_silence(0.1);
            }
            if ui
                .add_enabled(ed.is_dirty() && !ed.exporting, egui::Button::new("Save"))
                .on_hover_text("⌘S — write what you hear back over the original file")
                .clicked()
            {
                out.push(EdIntent::SaveInPlace);
            }
            ui.separator();
            if ui
                .button("✂ Crop")
                .on_hover_text("C — load only the selected region, to zoom and fine-tune inside it. The file isn't changed.")
                .clicked()
            {
                ed.crop_to_selection();
            }
            if ui
                .add_enabled(ed.crop.is_some(), egui::Button::new("Uncrop"))
                .on_hover_text("⇧C — show the whole file again, keeping the selection")
                .clicked()
            {
                ed.uncrop();
            }
            ui.separator();
            if ui
                .add_enabled(ed.sel.1 - ed.sel.0 >= 2, egui::Button::new("½ Loop"))
                .on_hover_text("Halve the selected region, keeping the in point")
                .clicked()
            {
                ed.scale_selection(0.5);
            }
            if ui
                .add_enabled(ed.can_double_selection(), egui::Button::new("×2 Loop"))
                .on_hover_text("Double the selected region, keeping the in point")
                .clicked()
            {
                ed.scale_selection(2.0);
            }
            if ui
                .add_enabled(ed.can_shift_selection(-1), egui::Button::new("◀"))
                .on_hover_text("Move the loop left by half its length")
                .clicked()
            {
                ed.shift_selection(-1);
            }
            if ui
                .add_enabled(ed.can_shift_selection(1), egui::Button::new("▶"))
                .on_hover_text("Move the loop right by half its length")
                .clicked()
            {
                ed.shift_selection(1);
            }
            ui.separator();
            // One control for listening and export: stereo, or which mono fold.
            ui.label("◐").on_hover_text("Mono: what you hear, the stats, Normalize and export all follow. M toggles.");
            let mut mono = ed.prefs.mono;
            let mut changed = ui
                .selectable_value(&mut mono, None, "Stereo")
                .on_hover_text("Keep all channels. M toggles mono.")
                .changed();
            for f in Fold::ALL {
                let tip = match f {
                    Fold::Mix => "Mono: L+R averaged. Very wide stereo can lose level or sound hollow (phase cancellation).",
                    Fold::Left => "Mono from the left channel only — no phase cancellation.",
                    Fold::Right => "Mono from the right channel only — no phase cancellation.",
                };
                changed |= ui.selectable_value(&mut mono, Some(f), f.short()).on_hover_text(tip).changed();
            }
            if changed {
                ed.set_mono(mono);
            }
            let label = if ed.exporting {
                if save_kit { "Saving…" } else { "Exporting…" }
            } else if save_kit {
                if ed.exports_mono() { "Save kit (mono)" } else { "Save kit" }
            } else if showing_kit {
                if ed.exports_mono() { "Export kit (mono)" } else { "Export kit" }
            } else if ed.exports_mono() {
                match ed.fold() {
                    Some(Fold::Left) => "Export mono (L) → Destination",
                    Some(Fold::Right) => "Export mono (R) → Destination",
                    _ => "Export mono → Destination",
                }
            } else {
                "Export → Destination"
            };
            let tip = if save_kit {
                "↩ / ⌘S — write the whole kit back over its file (it's already in the destination folder)"
            } else if showing_kit {
                "↩ — write the whole kit (in / out ignored), with its slice markers, into the active destination tab, then name it"
            } else {
                "↩ — write the trimmed, gained region as a new WAV into the active destination tab"
            };
            if ui
                .add_enabled(!ed.exporting && if save_kit { ed.is_dirty() } else { can_export }, egui::Button::new(label))
                .on_hover_text(tip)
                .clicked()
            {
                out.push(EdIntent::Export);
            }
            if !showing_kit
                && ui
                    .add_enabled(ed.current.is_some(), egui::Button::new("+ Slice → Kit"))
                    .on_hover_text("K — add the selection (as you hear it) to the end of the slice kit")
                    .clicked()
            {
                out.push(EdIntent::AddSlice);
            }
        });
    });
    }
}

/// In / out / length / playhead readout. In and out are drag-or-type fields
/// in frames for when the mouse isn't precise enough.
pub(super) fn positions(ui: &mut Ui, ed: &mut Editor, pv: &PlayView) {
    let Some(s) = ed.sample().cloned() else { return };
    let frames = s.frames();
    let secs = |f: usize| format!("{:.3}s", s.seconds(f));
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("In").color(IN_COLOR));
        let mut i = ed.sel.0;
        if ui.add(egui::DragValue::new(&mut i).range(0..=frames).speed(1.0)).changed() {
            ed.sel.0 = i.min(ed.sel.1.saturating_sub(1));
        }
        ui.label(RichText::new(secs(ed.sel.0)).weak().monospace());
        ui.separator();
        ui.label(RichText::new("Out").color(OUT_COLOR));
        let mut o = ed.sel.1;
        if ui.add(egui::DragValue::new(&mut o).range(0..=frames).speed(1.0)).changed() {
            ed.sel.1 = o.max(ed.sel.0 + 1).min(frames);
        }
        ui.label(RichText::new(secs(ed.sel.1)).weak().monospace());
        ui.separator();
        let len = ed.sel.1 - ed.sel.0;
        ui.label(RichText::new(format!("Len {} · {len} fr", secs(len))).monospace());
        ui.separator();
        let (label, f) = if pv.playing { ("▶", pv.pos) } else { ("▸", ed.playhead) };
        ui.label(RichText::new(format!("{label} {}", secs(f.min(frames)))).color(PLAY_COLOR).monospace());
    });
}
