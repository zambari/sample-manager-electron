//! Compact device-style sound tags, bitmap editor and a shared tags-only clipboard.
use crate::device::presets::{TAG_NAMES, tag_names};
use crate::ui::theme::{OLED_BG, OLED_DIM, oled_font};
use egui::{Color32, Id, Rect, Sense};

pub const WIDTH: f32 = 156.0;
const ABBR: [&str; 32] = [
    "KICK", "SNAR", "RIM", "CLAP", "TOP", "PERC", "HHAT", "CYM", "COW", "SYN", "BASS", "LEAD",
    "PAD", "TEXT", "CHRD", "SFX", "ELEC", "MET", "ACOU", "ATMO", "NOIS", "GLCH", "HARD", "SOFT",
    "DARK", "BRI", "VIN", "EPIC", "FAIL", "LOOP", "MINE", "FAV",
];

fn clipboard(ctx: &egui::Context) -> Option<u32> {
    ctx.data(|d| d.get_temp(Id::unique("sound_tags_clipboard")))
}

fn copy(ctx: &egui::Context, mask: u32) {
    ctx.data_mut(|d| d.insert_temp(Id::unique("sound_tags_clipboard"), mask));
}

/// Draw tags into a fixed row cell; click any badge (or + TAGS) to edit.
/// Right-click copies/pastes just the flags, including an empty bitmap.
pub fn show(
    ui: &egui::Ui,
    rect: Rect,
    id: Id,
    mask: u32,
    editable: bool,
    changed: bool,
) -> Option<u32> {
    show_with_open(ui, rect, id, mask, editable, changed).0
}

/// Also report opening the editor/menu, so a preset row can lazily load its sound.
pub fn show_with_open(
    ui: &egui::Ui,
    rect: Rect,
    id: Id,
    mask: u32,
    editable: bool,
    changed: bool,
) -> (Option<u32>, bool) {
    let response = ui.interact(rect, id, Sense::click());
    let opened = response.clicked() || response.secondary_clicked();
    let p = ui.painter().with_clip_rect(rect);
    let ink = if changed {
        crate::ui::theme::CHANGED
    } else {
        Color32::from_gray(176)
    };
    // Keep the group against the right edge, preserving the device's bit order.
    let y = rect.center().y - 1.0;
    if mask == 0 {
        p.text(
            egui::pos2(rect.right(), y),
            egui::Align2::RIGHT_CENTER,
            "+ TAGS",
            oled_font(9.0),
            OLED_DIM,
        );
    } else {
        let bits: Vec<usize> = (0..32).filter(|&bit| mask & (1u32 << bit) != 0).collect();
        let ellipsis = p.layout_no_wrap("…".into(), oled_font(10.0), ink);
        let mut labels = Vec::new();
        let mut width = 0.0;
        let mut overflow = false;
        for (n, &bit) in bits.iter().enumerate() {
            let text = p.layout_no_wrap(ABBR[bit].into(), oled_font(10.0), OLED_BG);
            let w = text.size().x + 9.0;
            let gap = if labels.is_empty() { 0.0 } else { 3.0 };
            let reserve = if n + 1 < bits.len() {
                ellipsis.size().x + 3.0
            } else {
                0.0
            };
            if width + gap + w + reserve > rect.width() {
                overflow = true;
                break;
            }
            width += gap + w;
            labels.push((text, w));
        }
        if overflow {
            width += ellipsis.size().x + if labels.is_empty() { 0.0 } else { 3.0 };
        }
        let mut x = rect.right() - width;
        for (text, w) in labels {
            let r = Rect::from_min_size(egui::pos2(x, y - 5.5), egui::vec2(w, 11.0));
            p.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(r.left(), y),
                    egui::pos2(r.left() + 4.0, r.top()),
                    r.right_top(),
                    r.right_bottom(),
                    egui::pos2(r.left() + 4.0, r.bottom()),
                ],
                ink,
                egui::Stroke::NONE,
            ));
            p.circle_filled(egui::pos2(x + 3.0, y), 0.8, OLED_BG);
            p.galley(egui::pos2(x + 6.0, y - text.size().y / 2.0), text, OLED_BG);
            x += w + 3.0;
        }
        if overflow {
            p.galley(egui::pos2(x, y - ellipsis.size().y / 2.0), ellipsis, ink);
        }
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let names = tag_names(mask).collect::<Vec<_>>().join(", ");
    let response = response.on_hover_text(format!(
        "{}\nClick: edit tags. Right-click: copy / paste tags",
        if names.is_empty() { "No tags" } else { &names }
    ));
    let mut value = mask;
    response.context_menu(|ui| {
        if ui.button("Copy tags").clicked() {
            copy(ui.ctx(), mask);
            ui.close();
        }
        if ui
            .add_enabled(
                editable && clipboard(ui.ctx()).is_some(),
                egui::Button::new("Paste tags"),
            )
            .clicked()
        {
            value = clipboard(ui.ctx()).unwrap_or(mask);
            ui.close();
        }
    });
    egui::Popup::menu(&response)
        .id(id.with("editor"))
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.label("SOUND TAGS");
            ui.horizontal(|ui| {
                if ui.button("Copy tags").clicked() {
                    copy(ui.ctx(), value);
                }
                if ui
                    .add_enabled(
                        editable && clipboard(ui.ctx()).is_some(),
                        egui::Button::new("Paste tags"),
                    )
                    .clicked()
                {
                    value = clipboard(ui.ctx()).unwrap_or(value);
                }
                if ui
                    .add_enabled(editable && value != 0, egui::Button::new("Clear"))
                    .clicked()
                {
                    value = 0;
                }
            });
            if !editable {
                ui.label("Editing unavailable while loading or busy");
            }
            ui.separator();
            ui.add_enabled_ui(editable, |ui| {
                egui::Grid::new(id.with("flags"))
                    .spacing(egui::vec2(12.0, 3.0))
                    .show(ui, |ui| {
                        for row in 0..8 {
                            for column in 0..4 {
                                let bit = column * 8 + row;
                                let flag = 1u32 << bit;
                                let mut on = value & flag != 0;
                                if ui.checkbox(&mut on, TAG_NAMES[bit]).changed() {
                                    if on {
                                        value |= flag;
                                    } else {
                                        value &= !flag;
                                    }
                                }
                            }
                            ui.end_row();
                        }
                    });
            });
            ui.label(
                egui::RichText::new("Edits are staged. Push to device saves them.")
                    .small()
                    .color(Color32::GRAY),
            );
        });
    ((editable && value != mask).then_some(value), opened)
}
