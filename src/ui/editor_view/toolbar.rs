//! The editor's header, slot bar, toolbar and position readouts.

use super::*;

pub(super) fn header(ui: &mut Ui, ed: &mut Editor, focused: bool, drive_shown: bool, out: &mut Vec<EdIntent>) {
    ui.horizontal(|ui| {
        if ui.add(crate::ui::theme::mode_title("Sample Editor", true, focused)).clicked() {
            out.push(EdIntent::TookFocus);
        }
        ui.add_space(14.0);
        crate::ui::theme::device_group(ui, |ui| {
            if crate::ui::theme::device_switch(ui, false, false).clicked() {
                out.push(EdIntent::ToggleDevice);
            }
            if crate::device::presence().available() {
                use crate::ui::elek_view::DevicePick;
                match crate::ui::elek_view::device_pages(ui, None, drive_shown) {
                    Some(DevicePick::Page(page)) => out.push(EdIntent::DevicePage(page)),
                    Some(DevicePick::Drive) => out.push(EdIntent::ShowDrive),
                    None => {}
                }
            }
        });
        ui.separator();
        if let (Some((a, b)), Some(s)) = (ed.crop, ed.sample()) {
            ui.label(RichText::new(format!("✂ cropped view {:.3}–{:.3}s", s.seconds(a), s.seconds(b))).weak())
                .on_hover_text("Only part of the file is loaded for editing. ⇧C or Uncrop shows the whole file again.");
        }
        if ed.loading.is_some() {
            ui.spinner();
        }
    });
    panel_toggles(ui, &mut ed.prefs.panels, ed.is_kit);
}

/// Sample / Kit tabs (drawn like the file panes' tabs), then the Kit
/// section with the kit's own controls.
pub(super) fn slot_bar(ui: &mut Ui, ed: &Editor, s: &Slots, out: &mut Vec<EdIntent>) {
    ui.add_space(4.0);
    slot_tabs(ui, ed, s, out);
    if ed.prefs.panels.kit {
        kit_section(ui, ed, s, out);
    }
}

/// The Kit section: → add (K), slot fill boxes, grid picker, Start New Grid Kit.
fn kit_section(ui: &mut Ui, ed: &Editor, s: &Slots, out: &mut Vec<EdIntent>) {
    let show_start = !s.showing_kit && ed.current.is_some();
    if s.kit_name.is_none() {
        // No kit open: only the Start button, highlighted.
        if show_start {
            ui.horizontal(|ui| {
                let btn = egui::Button::new(
                    RichText::new("Start New Grid Kit").color(crate::ui::theme::ON_ACCENT),
                )
                .fill(crate::ui::theme::ACCENT);
                if ui
                    .add(btn)
                    .on_hover_text("Make this file the slice kit, to add slices to its end")
                    .clicked()
                {
                    out.push(EdIntent::UseAsKit);
                }
            });
        }
        return;
    }
    section_header(ui, "Kit");
    ui.horizontal(|ui| {
        if let Some(k) = &s.kit_name {
            let fill_total = match s.kit_fill {
                (n, Some(g)) if !s.showing_kit || n > 1 => {
                    Some(if s.kit_grid == Some(crate::editor::AUTO_GRID) { g.max(8) } else { g })
                }
                _ => None,
            };
            if !s.showing_kit && add_slice_button(ui, ed.current.is_some(), k) {
                out.push(EdIntent::AddSlice);
            }
            if let (n, Some(total)) = (s.kit_fill.0, fill_total) {
                fill_boxes(ui, n, total);
            }
            grid_picker(ui, s.kit_grid, out);
        }
        if show_start {
            // Pinned to the right edge; the rest of the row flows from the left.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("Start New Grid Kit")
                    .on_hover_text("Make this file the slice kit, to add slices to its end")
                    .clicked()
                {
                    out.push(EdIntent::UseAsKit);
                }
            });
        }
    });
}

/// Sample tab, kit tab(s) and the ▾ kit menu, with a hairline under the
/// row that breaks under the selected tab, like the file panes' tab strip.
fn slot_tabs(ui: &mut Ui, ed: &Editor, s: &Slots, out: &mut Vec<EdIntent>) {
    let mut active_rect = None;
    let row = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let sample = s.sample_name.as_deref().unwrap_or("—");
        let mut label = crate::app::short_name(sample, 28);
        let mut tip = "B — the sample you're browsing. Moving the file cursor always loads here.".to_owned();
        if !s.showing_kit {
            if ed.is_dirty() {
                label.push_str("  ● unsaved");
            } else if ed.is_edited() {
                label.push_str("  •");
            }
            if let Some(p) = ed.path() {
                tip = format!("{}\n{tip}", p.display());
            }
        }
        let resp = ui
            .add(crate::ui::theme::pane_tab(&label, !s.showing_kit))
            .on_hover_text(tip);
        if !s.showing_kit {
            active_rect = Some(resp.rect);
        }
        if resp.clicked() {
            out.push(EdIntent::ShowKit(false));
        }
        // No kit yet: nothing to show until K starts one.
        if let Some(k) = &s.kit_name {
            // Room the ▾ menu after the kit tabs needs.
            let gap = ui.spacing().item_spacing.x;
            let reserve = 30.0 + 4.0 * gap;
            let all_fit = s.kits.len() > 1 && {
                let tab_w = |t: &str| button_text_width(ui, t) + 2.0 * TAB_PAD_X + gap;
                let active = tab_w(&format!("[00/00] {}      ●", crate::app::short_name(k, 28)));
                let parked: f32 = s
                    .kits
                    .iter()
                    .filter(|e| e.parked.is_some())
                    .map(|e| tab_w(&parked_label(e)))
                    .sum();
                active + parked + reserve <= ui.available_width()
            };
            if all_fit {
                // Every open kit as its own tab, in name order.
                for e in &s.kits {
                    match e.parked {
                        None => {
                            if let Some(r) = kit_tab(ui, k, s, out) {
                                active_rect = Some(r);
                            }
                        }
                        Some(i) => {
                            if ui
                                .add(crate::ui::theme::pane_tab(&parked_label(e), false))
                                .on_hover_text(format!("{} — click to make it the kit K adds to", e.name))
                                .clicked()
                            {
                                out.push(EdIntent::SwitchKit(i));
                            }
                        }
                    }
                }
            } else if let Some(r) = kit_tab(ui, k, s, out) {
                active_rect = Some(r);
            }
            kit_switcher(ui, ed, s, !all_fit, out);
        }
    });
    // Hairline under the strip, open under the selected tab.
    let row = row.response.rect;
    if let Some(a) = active_rect {
        let y = a.bottom() - 0.5;
        let stroke = egui::Stroke::new(1.0, Color32::from_gray(52));
        let right = ui.max_rect().right();
        let p = ui.painter();
        if a.left() > row.left() {
            p.hline(row.left()..=a.left(), y, stroke);
        }
        if a.right() < right {
            p.hline(a.right()..=right, y, stroke);
        }
    }
}

/// Horizontal padding inside a `theme::pane_tab`, for fitting tabs.
const TAB_PAD_X: f32 = 10.0;

/// A parked kit's tab text.
fn parked_label(e: &KitEntry) -> String {
    let mut t = crate::app::short_name(&e.name, 20);
    if e.dirty {
        t.push_str(" ●");
    }
    t
}

fn button_text_width(ui: &Ui, text: &str) -> f32 {
    let font = egui::TextStyle::Button.resolve(ui.style());
    ui.painter()
        .layout_no_wrap(text.to_owned(), font, egui::Color32::WHITE)
        .size()
        .x
}

/// The → (K) button: a big arrow in a button as tall as its neighbours.
/// The arrow is painted on top so its large font doesn't make the button taller.
fn add_slice_button(ui: &mut Ui, enabled: bool, kit: &str) -> bool {
    let h = ui.spacing().interact_size.y;
    let resp = ui
        .add_enabled(
            enabled,
            egui::Button::new("")
                .fill(crate::ui::theme::ACCENT)
                .min_size(egui::vec2(h * 1.6, h)),
        )
        .on_hover_text(format!(
            "K — add the selection (as you hear it) to the end of {kit}"
        ));
    let color = if enabled {
        crate::ui::theme::ON_ACCENT
    } else {
        ui.visuals().weak_text_color()
    };
    ui.painter().text(
        resp.rect.center(),
        egui::Align2::CENTER_CENTER,
        "→",
        egui::FontId::proportional(20.0),
        color,
    );
    resp.clicked()
}

const FILL_SIDE: f32 = 7.0;
const FILL_GAP: f32 = 2.0;
const FILL_GROUP_GAP: f32 = 5.0;

fn fill_width(total: usize) -> f32 {
    let groups = total.div_ceil(4);
    total as f32 * FILL_SIDE
        + total.saturating_sub(1) as f32 * FILL_GAP
        + groups.saturating_sub(1) as f32 * (FILL_GROUP_GAP - FILL_GAP)
}

/// A row of small boxes, `filled` of `total` in orange: how close the grid
/// kit is to full. Grouped in fours.
fn fill_boxes(ui: &mut Ui, filled: usize, total: usize) {
    let (side, gap) = (FILL_SIDE, FILL_GAP);
    let group_gap = FILL_GROUP_GAP;
    let w = fill_width(total);
    let h = ui.spacing().interact_size.y;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    resp.on_hover_text(format!("{filled} of {total} slots used"));
    let v = ui.visuals();
    let full = filled >= total;
    let mut x = rect.left();
    for i in 0..total {
        if i > 0 {
            x += if i % 4 == 0 { side + group_gap } else { side + gap };
        }
        let r = egui::Rect::from_min_size(egui::pos2(x, rect.center().y - side / 2.0), egui::vec2(side, side));
        if i < filled {
            // A full kit is done (nothing more to add): drawn muted, not bright.
            let c = if full { Color32::from_gray(78) } else { crate::ui::theme::ACCENT_BRIGHT };
            ui.painter().rect_filled(r, 1.0, c);
        } else {
            ui.painter().rect_stroke(
                r,
                1.0,
                egui::Stroke::new(1.0, v.weak_text_color()),
                egui::StrokeKind::Inside,
            );
        }
    }
}

/// The × at the right end of the kit tab. Added after the tab so clicks on
/// it reach it rather than the tab. Returns true when clicked.
fn close_x(ui: &mut Ui, tab: egui::Rect, dirty: bool) -> bool {
    let size = tab.height().min(16.0);
    let rect = egui::Rect::from_center_size(
        egui::pos2(tab.right() - size / 2.0 - 3.0, tab.center().y),
        egui::vec2(size, size),
    );
    let resp = ui
        .interact(
            rect,
            egui::Id::unique("kit_tab_close"),
            egui::Sense::click(),
        )
        .on_hover_text(if dirty {
            "Save the kit (⌘S in kit view) before closing it"
        } else {
            "Close this kit (the file stays)"
        });
    let v = ui.visuals();
    let color = if dirty {
        v.weak_text_color()
    } else if resp.hovered() {
        v.strong_text_color()
    } else {
        v.text_color()
    };
    if resp.hovered() && !dirty {
        ui.painter()
            .rect_filled(rect, 3.0, v.widgets.hovered.bg_fill);
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        crate::ui::theme::CLOSE_GLYPH,
        egui::FontId::proportional(14.0),
        color,
    );
    // A dirty kit refuses to close and says why in the status bar.
    resp.clicked()
}

/// The Kit tab. Clicking it while the kit is showing (or double-clicking it)
/// turns it into a name field: ↩ or clicking away renames, Esc cancels.
fn kit_tab(ui: &mut Ui, k: &str, s: &Slots, out: &mut Vec<EdIntent>) -> Option<egui::Rect> {
    let mut rect = None;
    let buf_id = egui::Id::unique("kit_rename_buf");
    let field_id = egui::Id::unique("kit_rename_field");
    let mut buf: Option<String> = ui.data(|d| d.get_temp(buf_id));
    if !s.showing_kit {
        buf = None;
    }
    if let Some(text) = &mut buf {
        ui.label("Kit:");
        let resp = ui.add(
            egui::TextEdit::singleline(text)
                .id(field_id)
                .desired_width(180.0),
        );
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
        let count = match s.kit_fill {
            (n, Some(g)) => format!("[{n}/{g}]"),
            (n, None) => format!("[{n}]"),
        };
        let mut label = format!("{count} {}", crate::app::short_name(k, 28));
        if s.kit_dirty {
            label.push_str(" ●");
        }
        // Room on the right for the × painted inside the tab.
        label.push_str("     ");
        let resp = ui.add(crate::ui::theme::pane_tab(&label, s.showing_kit)).on_hover_text(
            "B — the slice kit. [n] = slices so far; [n/8] = slots used of the grid's 8.\n\
             K adds the sample's selection to its end. ⌘S here saves the whole kit.\n\
             Click again (or double-click) to rename it — a saved kit's file is renamed too.",
        );
        if s.showing_kit {
            rect = Some(resp.rect);
        }
        close_all_menu(&resp, s, out);
        if close_x(ui, resp.rect, s.kit_dirty) {
            out.push(EdIntent::CloseKit);
        } else if resp.double_clicked() || (resp.clicked() && s.showing_kit) {
            let stem = k
                .strip_suffix(".wav")
                .or_else(|| k.strip_suffix(".WAV"))
                .unwrap_or(k);
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
    rect
}

/// ▾ menu next to the Kit tab: the open kits, to pick which one K adds to,
/// and "New kit" to start another.
fn kit_switcher(ui: &mut Ui, ed: &Editor, s: &Slots, collapsed: bool, out: &mut Vec<EdIntent>) {
    let label = if collapsed && s.kits.len() > 1 {
        format!("▾ {}", s.kits.len())
    } else {
        "▾".to_owned()
    };
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
    .on_hover_text(
        "Open kits — pick the one K adds to. Right-click a file → Load as kit to open another.\n\
         Right-click here (or the kit tab) to close them all.",
    )
    .context_menu(|ui| close_all_items(ui, s, out));
}

/// Right-click on a kit tab: close all / all but this one.
fn close_all_menu(resp: &egui::Response, s: &Slots, out: &mut Vec<EdIntent>) {
    resp.context_menu(|ui| close_all_items(ui, s, out));
}

fn close_all_items(ui: &mut Ui, s: &Slots, out: &mut Vec<EdIntent>) {
    const HINT: &str = "Kits with unsaved slices ask first; Cancel keeps that one and goes on with the rest";
    if ui.button("Close all kits").on_hover_text(HINT).clicked() {
        out.push(EdIntent::CloseAllKits(false));
        ui.close();
    }
    if ui
        .add_enabled(s.kits.len() > 1, egui::Button::new("Close all but current"))
        .on_hover_text(HINT)
        .clicked()
    {
        out.push(EdIntent::CloseAllKits(true));
        ui.close();
    }
}

/// Free / 4 / 8 / 16 switch for the kit's layout.
fn grid_picker(ui: &mut Ui, cur: Option<usize>, out: &mut Vec<EdIntent>) {
    use crate::editor::AUTO_GRID;
    let label = |g: Option<usize>| match g {
        None => "Free".to_owned(),
        Some(AUTO_GRID) => "Auto".to_owned(),
        Some(n) => format!("{n} slots"),
    };
    egui::ComboBox::from_id_salt("kit_grid")
        .selected_text(label(cur))
        .width(70.0)
        .show_ui(ui, |ui| {
            for g in [None, Some(AUTO_GRID)].into_iter().chain(crate::editor::GRID_SIZES.map(Some)) {
                if ui.selectable_label(cur == g, label(g)).clicked() && cur != g {
                    out.push(EdIntent::SetGrid(g));
                }
            }
        })
        .response
        .on_hover_text(
            "Free: slices go end to end, wherever they fall.\n\
             N slots: the kit is split into N equal parts (what Digitakt 1 can slice), each slice at the start of \
             its slot, padded with silence to the longest one. K fills the next free slot; cuts and pastes snap back onto the grid.\n\
             Auto: 4 slots, growing to 8 then 16 as you add slices, and shrinking back when you cut them.",
        );
}

/// Right-aligned chips that show / hide each editor panel. Saved with the prefs.
pub(super) fn panel_toggles(ui: &mut Ui, p: &mut crate::editor::Panels, kit: bool) {
    section_header(ui, "Show");
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let items: Vec<(&mut bool, &str, &str)> = vec![
            (
                &mut p.kit,
                "Kit",
                "Slice kit row: → add (K), slot fill, grid, Start kit",
            ),
            (
                &mut p.transport,
                "Transport",
                "Play, loop, follow, snap and auto-load / auto-play",
            ),
            (&mut p.gain, "Gain", "Gain and normalize"),
            (
                &mut p.histogram,
                "Histogram",
                "Level histogram of the region with the gain applied, and lead / tail silence stats",
            ),
            (&mut p.tools, "Tools", "Undo, cut / paste, crop, loop scaling"),
            (
                &mut p.nums,
                "Nums",
                "In / out / len / playhead readout, under the waveform",
            ),
            (
                &mut p.scalpel,
                "Scalpel",
                "80 ms extreme-zoom strip around the playhead",
            ),
            (
                &mut p.minimap,
                "Map",
                "Minimap: the whole file, click to jump",
            ),
            (&mut p.stats, "Stats", "Stats table and phase meter"),
            (
                &mut p.trim_tails,
                "Trim",
                "Fade and cut every kit slice's tail below a threshold to free sample memory. Hidden, no fades show on the wave",
            ),
            (&mut p.help, "Help", "Keyboard shortcut list"),
        ];
        for (v, label, hover) in items {
            // Trim only applies to a kit: shown greyed out on a plain
            // sample, pointing the way to slice mode.
            if label == "Trim" && !kit {
                ui.add_enabled(false, vis_toggle(v, label)).on_disabled_hover_text(
                    "Trim works on a kit: enter slice mode (the Kit tab, or + Slice → Kit / K) to use it",
                );
                continue;
            }
            ui.add(vis_toggle(v, label))
                .on_hover_text(format!("Show / hide: {hover}"));
        }
    });
}

/// Save and Export, always shown. Each button means the same thing in every
/// state: Save writes over the open file, Export writes a new file into the
/// destination tab.
/// Which file is open: its kind and path, each clickable to show it in the
/// source pane, plus the original when a copy is open.
fn origin_line(ui: &mut Ui, o: &super::Origin, out: &mut Vec<EdIntent>) {
    let warn = Color32::from_rgb(230, 170, 80);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let kind = RichText::new(o.kind).strong();
        ui.label(if o.warn { kind.color(warn) } else { kind }).on_hover_text(&o.tip);
        let shown = tidy_path(&o.path);
        if ui
            .link(RichText::new(crate::app::short_name(&shown, 60)).weak())
            .on_hover_text(format!("{}\nClick: show it in the source pane", o.path.display()))
            .clicked()
        {
            out.push(EdIntent::ShowInSource(o.path.clone()));
        }
        if let Some((orig, fresh)) = &o.original {
            ui.label("·");
            let label = if *fresh { "original: " } else { "original (changed since upload): " };
            ui.label(RichText::new(label).weak());
            if ui
                .link(crate::app::short_name(&tidy_path(orig), 48))
                .on_hover_text(format!(
                    "{}\nThe file this sample was uploaded from. It may hold markers the device copy lost.\nClick: show it in the source pane",
                    orig.display()
                ))
                .clicked()
            {
                out.push(EdIntent::ShowInSource(orig.clone()));
            }
        }
        if let Some((slot, same, by_name)) = &o.pool {
            ui.label("·");
            let (text, tip, bad) = match (same, by_name) {
                (Some(true), _) => (format!("pool {slot}"), format!("Slot {slot} holds this audio"), false),
                (Some(false), true) => (
                    format!("pool {slot}: not identical"),
                    format!("Slot {slot} has a file of the same name, but its audio differs from this file"),
                    true,
                ),
                (Some(false), false) => (
                    format!("pool {slot}: not identical"),
                    format!("Slot {slot} came from this file, but the file changed since"),
                    true,
                ),
                (None, _) => (
                    format!("pool {slot}? (same name)"),
                    format!("Slot {slot} has a file of the same name. Not compared yet: fetch it for preview to check the audio"),
                    false,
                ),
            };
            let t = RichText::new(text);
            if ui
                .link(if bad { t.color(warn) } else { t })
                .on_hover_text(format!("{tip}\nClick: show the slot in the sample pool"))
                .clicked()
            {
                out.push(EdIntent::OriginInPool);
            }
        }
    });
}

/// `~/…` for paths under the home folder.
fn tidy_path(p: &std::path::Path) -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    match home.as_deref().and_then(|h| p.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    }
}

pub(super) fn file_row(ui: &mut Ui, ed: &mut Editor, can_export: bool, slots: &Slots, out: &mut Vec<EdIntent>) {
    let showing_kit = slots.showing_kit;
    let has = ed.current.is_some();
    section_header(ui, "File");
    if let Some(o) = slots.origin.as_ref().filter(|_| !showing_kit && has) {
        origin_line(ui, o, out);
    }
    ui.horizontal_wrapped(|ui| {
        mono_picker(ui, ed);
        ui.separator();
        let mono = if ed.exports_mono() {
            match ed.fold() {
                Some(Fold::Left) => " (mono L)",
                Some(Fold::Right) => " (mono R)",
                _ => " (mono)",
            }
        } else {
            ""
        };
        let busy = ed.exporting;
        let pool_slot = slots.pool_slot.as_ref().filter(|_| !showing_kit);
        // Save: always over the one thing this file belongs to, named on
        // the button. A pool sample's is its slot; a kit's is its own file,
        // once it has one; a sample's is the file itself.
        let short = |n: &str| crate::app::short_name(n, 28);
        if let Some(slot) = pool_slot {
            let label = if busy { "Updating…".to_owned() } else { format!("⬆ Save → slot {slot}{mono}") };
            let tip = format!(
                "⌘S — send what you hear (trimmed, with gain) to slot {slot} and push, so the device plays it right away. \
                 The first save uploads a new file next to the +Drive original (which stays: other projects may use it); \
                 later saves replace that copy. Your local files aren't touched."
            );
            let button = egui::Button::new(RichText::new(label).color(crate::ui::theme::ON_ACCENT))
                .fill(crate::ui::theme::ACCENT);
            if ui
                .add_enabled(has && ed.can_save() && !busy, button)
                .on_hover_text(&tip)
                .on_disabled_hover_text(format!("Nothing changed yet. {tip}"))
                .clicked()
            {
                out.push(EdIntent::SaveInPlace);
            }
        } else {
            let (label, enabled, tip) = if showing_kit {
                match slots.kit_name.as_deref().filter(|_| slots.kit_on_disk) {
                    Some(k) => (
                        format!("💾 Save over {}{mono}", short(k)),
                        ed.can_save(),
                        format!("⌘S — write the whole kit back over its own file, {k}. The samples its slices came from are never touched."),
                    ),
                    None => (
                        "💾 Save (export first)".to_owned(),
                        false,
                        "This kit has no file of its own yet: Export kit writes it into the destination, and Save writes over that file from then on. \
                         The samples its slices came from are never saved over. (⌘S on it runs Export.)"
                            .to_owned(),
                    ),
                }
            } else {
                let name = slots.sample_name.as_deref().unwrap_or("the file");
                (
                    format!("💾 Save over {}{mono}", short(name)),
                    ed.can_save(),
                    format!("⌘S — write what you hear (with gain) back over {name}, the file that's open; asks whether to trim when in / out don't span the whole file"),
                )
            };
            let label = if busy { "Saving…".to_owned() } else { label };
            if ui
                .add_enabled(has && enabled && !busy, egui::Button::new(label))
                .on_hover_text(&tip)
                .on_disabled_hover_text(&tip)
                .clicked()
            {
                out.push(EdIntent::SaveInPlace);
            }
        }
        // Export: a new file in the destination ("save as"), named on the
        // button when it isn't a local folder. A pool sample's destination
        // is its slot. The main action is solid accent: Export for a sample
        // (unless Save already is, for a slot), Export and close for a kit.
        let to = match (pool_slot, &slots.export_drive) {
            (Some(slot), _) => format!("slot {slot}"),
            (None, Some(dir)) => format!("+Drive {}", crate::app::short_name(dir, 28)),
            (None, None) => "Destination".to_owned(),
        };
        let (label, tip) = if showing_kit {
            (
                format!("Export kit{mono} → {to}"),
                "↩ — write the whole kit (in / out ignored), with its slice markers, as a new file in the active destination tab, and keep it open. A taken name asks for another one.".to_owned(),
            )
        } else if let Some(slot) = pool_slot {
            (
                format!("Export{mono} → slot {slot}"),
                format!("↩ — same as Save here: this sample's destination is its slot {slot}. Save to ⏷ has the other places."),
            )
        } else {
            (
                format!("Export{mono} → {to}"),
                match &slots.export_drive {
                    Some(dir) => format!("↩ — upload the trimmed, gained region as a new file into {dir} on the device (the active destination tab)"),
                    None => "↩ — write the trimmed, gained region as a new WAV into the active destination tab".to_owned(),
                },
            )
        };
        let accent = |label: String| egui::Button::new(RichText::new(label).color(crate::ui::theme::ON_ACCENT)).fill(crate::ui::theme::ACCENT);
        // A pool sample's Export does what its (accent) Save does: plain.
        let button = if showing_kit || pool_slot.is_some() { egui::Button::new(label) } else { accent(label) };
        let enabled = has && !busy && if pool_slot.is_some() { ed.can_save() } else { can_export };
        if ui.add_enabled(enabled, button).on_hover_text(tip).clicked() {
            out.push(EdIntent::Export);
        }
        if showing_kit
            && ui
                .add_enabled(has && can_export && !busy, accent(format!("Export{mono} and close")))
                .on_hover_text(format!(
                    "Export the kit into {to} as above, then close it once it's written: the usual way to finish a kit. \
                     Nothing closes if the export is cancelled or fails."
                ))
                .clicked()
        {
            out.push(EdIntent::ExportAndClose);
        }
        save_to_menu(ui, has && !busy, showing_kit, slots, &mono, out);
        if let Some(f) = ed.fold() {
            // Pinned to the right edge of the row.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let txt = if f == Fold::Mix { "◐ MONO".to_owned() } else { format!("◐ MONO {}", f.short()) };
                ui.label(RichText::new(txt).color(MONO_COLOR).strong())
                    .on_hover_text("Playing folded to mono. M toggles.");
            });
        }
    });
}

/// "Save to ▾": every other place what you hear can go, each saying
/// where. Overwrites ask first.
fn save_to_menu(ui: &mut Ui, enabled: bool, kit: bool, slots: &Slots, mono: &str, out: &mut Vec<EdIntent>) {
    ui.add_enabled_ui(enabled, |ui| {
        ui.menu_button("Save to ⏷", |ui| {
            ui.set_min_width(300.0);
            let head = |ui: &mut Ui, t: &str| {
                ui.label(RichText::new(t).small().weak());
            };
            let item = |ui: &mut Ui, on: bool, label: String, tip: &str| {
                ui.add_enabled(on, egui::Button::new(label)).on_hover_text(tip).on_disabled_hover_text(tip).clicked()
            };
            head(ui, "THIS COMPUTER");
            if let Some((orig, _)) = slots.origin.as_ref().filter(|_| !kit).and_then(|o| o.original.as_ref()) {
                let name = orig.file_name().unwrap_or_default().to_string_lossy();
                if item(
                    ui,
                    true,
                    format!("Over the original, {}{mono}…", crate::app::short_name(&name, 32)),
                    &format!("Write what you hear over {}, the local file this device sample was uploaded from. Asks first.", orig.display()),
                ) {
                    out.push(EdIntent::SaveOverOriginal);
                }
            }
            let to = match &slots.export_drive {
                Some(dir) => format!("+Drive {}", crate::app::short_name(dir, 28)),
                None => "Destination".to_owned(),
            };
            if kit {
                if item(ui, true, format!("New file in {to}, keep the kit open{mono}"), "Same as Export kit (↩)") {
                    out.push(EdIntent::Export);
                }
                if item(ui, true, format!("New file in {to}, then close the kit{mono}"), "Same as Export and close") {
                    out.push(EdIntent::ExportAndClose);
                }
            } else if slots.pool_slot.is_none() {
                if item(ui, true, format!("New file in {to}{mono}"), "Same as Export (↩)") {
                    out.push(EdIntent::Export);
                }
            }
            ui.separator();
            head(ui, "SAMPLE POOL (device memory)");
            match &slots.pool {
                None => {
                    ui.label(RichText::new("Read the device's project first (Device view → Load current project)").weak());
                }
                Some(p) => {
                    if let Some(n) = p.note {
                        ui.label(RichText::new(n).weak());
                    }
                    let sel_tip = "Select one slot in the Samples page (or the docked pool) to aim at it";
                    let clicked = match &p.selected {
                        Some((slot, None)) => item(
                            ui,
                            true,
                            format!("Into selected slot {slot} (empty){mono}"),
                            "Upload what you hear as a new +Drive file, put it in this slot and push. Saving after that updates the slot.",
                        ),
                        Some((slot, Some(name))) => item(
                            ui,
                            true,
                            format!("Replace selected slot {slot} ({}){mono}…", crate::app::short_name(name, 24)),
                            "Put what you hear into this slot in place of what it holds; patterns using the slot play it. Asks first.",
                        ),
                        None => item(ui, false, "Into the selected slot".to_owned(), sel_tip),
                    };
                    if clicked {
                        out.push(EdIntent::SaveToSlot(SlotPick::Selected));
                    }
                    let (on, label) = match &p.first_free {
                        Some(slot) => (true, format!("Into first empty slot ({slot}){mono}")),
                        None => (false, "Into first empty slot (none left)".to_owned()),
                    };
                    if item(ui, on, label, "Upload what you hear as a new +Drive file, put it in the first empty slot and push.") {
                        out.push(EdIntent::SaveToSlot(SlotPick::FirstFree));
                    }
                }
            }
            if kit {
                return;
            }
            ui.separator();
            head(ui, "+DRIVE (device files)");
            match &slots.device_home {
                Some(home) => {
                    let tip = format!("Upload what you hear as a new file next to {home}, with a name you pick. The original stays.");
                    if item(ui, true, format!("New file next to {}{mono}…", crate::app::short_name(home, 32)), &tip) {
                        out.push(EdIntent::SendToDevice);
                    }
                }
                None => {
                    item(ui, false, "New file next to its device original…".to_owned(), "Only for samples that came from the device or went to it");
                }
            }
        });
    });
}

pub(super) fn toolbar(
    ui: &mut Ui,
    ed: &mut Editor,
    pv: &PlayView,
    slots: &Slots,
    out: &mut Vec<EdIntent>,
) {
    let showing_kit = slots.showing_kit;
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
        ui.add(crate::ui::theme::soft_toggle(&mut ed.prefs.looping, "🔁 Loop")).on_hover_text("L — loop the in/out region");
        ui.add(crate::ui::theme::soft_toggle(&mut ed.prefs.follow_playback, "Follow"))
            .on_hover_text("V — scroll the view along with the play cursor. Turn off to stay put while zoomed in on a loop end.");
        if ui
            .add(crate::ui::theme::soft_toggle(&mut ed.prefs.snap_zero, "Snap to 0"))
            .on_hover_text("S — snap in/out points to the nearest zero crossing. Turning it on snaps the current selection.")
            .changed()
            && ed.prefs.snap_zero
        {
            ed.snap_selection();
        }
        // Nothing auto-loads into the kit, so these only matter for the sample.
        if !showing_kit {
            ui.separator();
            ui.add(crate::ui::theme::soft_toggle(&mut ed.prefs.auto_load, "Auto-load"))
                .on_hover_text("Load the sample under the file cursor");
            ui.add(crate::ui::theme::soft_toggle(&mut ed.prefs.auto_play, "Auto-play"))
                .on_hover_text("Play each sample as the cursor lands on it");
        }
    });
    }
    if pn.gain {
        section_header(ui, "Gain");
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(has, |ui| {
                // In a kit, gain scales only the selected audio.
                let kit = ed.is_kit;
                ui.label(if kit { "Selection gain" } else { "Gain" });
                let mut sel_db = ed.sel_gain_db();
                let db = if kit { &mut sel_db } else { &mut ed.gain_db };
                let changed = ui
                    .add(
                        egui::Slider::new(db, -36.0..=36.0)
                            .suffix(" dB")
                            .step_by(0.1)
                            .clamping(egui::SliderClamping::Never),
                    )
                    .on_hover_text(if kit {
                        "Gain on the selected audio only; the rest of the kit is untouched. Double-click the number to type a value"
                    } else {
                        "Double-click the number to type a value"
                    })
                    .changed();
                if kit && changed {
                    ed.set_sel_gain(sel_db);
                }
                if ui
                    .button("Normalize")
                    .on_hover_text(if kit {
                        "N — gain the selection so it peaks at the target"
                    } else {
                        "N — set gain so the region peaks at the target"
                    })
                    .clicked()
                {
                    if kit {
                        ed.normalize_sel();
                    } else {
                        ed.normalize();
                    }
                }
                ui.add(
                    egui::DragValue::new(&mut ed.prefs.normalize_db)
                        .range(-24.0..=0.0)
                        .speed(0.05)
                        .suffix(" dB"),
                )
                .on_hover_text("Normalize target peak");
                if ui
                    .button("Reset")
                    .on_hover_text(if kit {
                        "Selection back to 0 dB (while it's still selected)"
                    } else {
                        "Whole file, 0 dB"
                    })
                    .clicked()
                {
                    if kit {
                        ed.set_sel_gain(0.0);
                    } else {
                        ed.reset();
                    }
                }
            });
        });
    }
    if pn.histogram && has {
        super::level_strip::histogram_panel(ui, ed);
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
            ui.separator();
            if ui
                .button("✂ Trim")
                .on_hover_text("C — load only the selected region, to zoom and fine-tune inside it. The file isn't changed.")
                .clicked()
            {
                ed.crop_to_selection();
            }
            if ui
                .add_enabled(ed.crop.is_some(), egui::Button::new("UnTrim"))
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
    let Some(s) = ed.sample().cloned() else {
        return;
    };
    let frames = s.frames();
    let secs = |f: usize| format!("{:.3}s", s.seconds(f));
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("In").color(IN_COLOR));
        let mut i = ed.sel.0;
        if ui
            .add(egui::DragValue::new(&mut i).range(0..=frames).speed(1.0))
            .changed()
        {
            ed.sel.0 = i.min(ed.sel.1.saturating_sub(1));
        }
        ui.label(RichText::new(secs(ed.sel.0)).weak().monospace());
        ui.separator();
        ui.label(RichText::new("Out").color(OUT_COLOR));
        let mut o = ed.sel.1;
        if ui
            .add(egui::DragValue::new(&mut o).range(0..=frames).speed(1.0))
            .changed()
        {
            ed.sel.1 = o.max(ed.sel.0 + 1).min(frames);
        }
        ui.label(RichText::new(secs(ed.sel.1)).weak().monospace());
        ui.separator();
        let len = ed.sel.1 - ed.sel.0;
        ui.label(RichText::new(format!("Len {} · {len} fr", secs(len))).monospace());
        ui.separator();
        let (label, f) = if pv.playing {
            ("▶", pv.pos)
        } else {
            ("▸", ed.playhead)
        };
        ui.label(
            RichText::new(format!("{label} {}", secs(f.min(frames))))
                .color(PLAY_COLOR)
                .monospace(),
        );
    });
}

/// Stereo, or which mono fold: one control for listening and export alike.
/// Greyed out on a mono file, where there is nothing to fold.
fn mono_picker(ui: &mut Ui, ed: &mut Editor) {
    let multi = ed.is_multichannel();
    ui.add_enabled_ui(multi || ed.current.is_none(), |ui| {
        ui.label("◐").on_hover_text("Mono: what you hear, the stats, Normalize, Save and Export all follow. M toggles.");
        let mut mono = ed.prefs.mono;
        let mut changed = ui
            .selectable_value(&mut mono, None, "Stereo")
            .on_hover_text("Keep all channels. M toggles mono.")
            .changed();
        for f in Fold::ALL {
            let tip = match f {
                Fold::Mix => "Mono: L+R averaged. Very wide stereo can lose level or sound hollow (phase cancellation).",
                Fold::Left => "Mono from the left channel only — the right one is dimmed and dropped. No phase cancellation.",
                Fold::Right => "Mono from the right channel only — the left one is dimmed and dropped. No phase cancellation.",
            };
            changed |= ui.selectable_value(&mut mono, Some(f), f.short()).on_hover_text(tip).changed();
        }
        if changed {
            ed.set_mono(mono);
        }
    })
    .response
    .on_disabled_hover_text("This file is already mono");
}
