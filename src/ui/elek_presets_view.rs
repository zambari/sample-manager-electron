//! The read-only Presets page: two independent +Drive bank browsers,
//! decoded sound details and exact matches in the current sound pool.

use crate::device::presets::{Preset, PresetEntry};
use crate::ui::elek_library_view::{is_default, machine_text, param_at, src_detail};
use crate::ui::elek_view::{self, ElekIntent, ElekState};
use crate::ui::theme::{self, OLED, OLED_BG, OLED_DIM, OLED_HOVER, oled_font};
use egui::{RichText, Sense, Stroke};
use std::collections::HashMap;

type Read<T> = Option<Result<T, String>>;

pub struct PresetView {
    pub banks: Read<Vec<String>>,
    asked: bool,
    pub listings: HashMap<String, Read<Vec<PresetEntry>>>,
    pub sounds: HashMap<(String, u32), Read<Preset>>,
    bank: [String; 2],
    selected: [Option<u32>; 2],
    filter: String,
    hide_empty: bool,
    show_defaults: bool,
}

impl Default for PresetView {
    fn default() -> Self {
        Self {
            banks: None,
            asked: false,
            listings: HashMap::new(),
            sounds: HashMap::new(),
            bank: ["A".into(), "B".into()],
            selected: [None; 2],
            filter: String::new(),
            hide_empty: false,
            show_defaults: false,
        }
    }
}

pub fn show(ui: &mut egui::Ui, st: &mut ElekState) -> Vec<ElekIntent> {
    let mut out = Vec::new();
    ui.horizontal(|ui| elek_view::mode_header(ui, st, &mut out));
    let present = crate::device::presence().available();
    if present && !st.presets.asked {
        st.presets.asked = true;
        out.push(ElekIntent::ListPresetBanks);
    }
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(present, egui::Button::new("⟳ Refresh presets"))
            .clicked()
        {
            let bank = st.presets.bank.clone();
            let filter = st.presets.filter.clone();
            let hide_empty = st.presets.hide_empty;
            let show_defaults = st.presets.show_defaults;
            st.presets = PresetView {
                bank,
                filter,
                hide_empty,
                show_defaults,
                asked: true,
                ..Default::default()
            };
            out.push(ElekIntent::ListPresetBanks);
        }
        ui.add(
            egui::TextEdit::singleline(&mut st.presets.filter)
                .hint_text("filter presets")
                .desired_width(140.0),
        );
        ui.add(theme::soft_toggle(&mut st.presets.hide_empty, "Hide empty"));
        ui.add(theme::soft_toggle(
            &mut st.presets.show_defaults,
            "Show defaults",
        ));
        ui.label(
            RichText::new("Saved on +Drive · select a preset to decode it")
                .weak()
                .small(),
        );
    });
    ui.separator();
    match &st.presets.banks {
        None => {
            ui.label(if present {
                "Reading preset banks…"
            } else {
                "Connect the device to browse its presets."
            });
        }
        Some(Err(e)) => {
            ui.colored_label(theme::CHANGED, e);
        }
        Some(Ok(banks)) if banks.is_empty() => {
            ui.label("No preset banks found.");
        }
        Some(Ok(banks)) => {
            let banks = banks.clone();
            for side in 0..2 {
                if !banks.contains(&st.presets.bank[side]) {
                    st.presets.bank[side] = banks[side.min(banks.len() - 1)].clone();
                    st.presets.selected[side] = None;
                }
            }
            ui.columns(2, |cols| {
                for (side, col) in cols.iter_mut().enumerate() {
                    col.push_id(("preset_pane", side), |ui| {
                        pane(ui, st, side, &banks, present, &mut out)
                    });
                }
            });
        }
    }
    elek_view::finish(ui, st, &mut out);
    out
}

fn bank_strip(ui: &mut egui::Ui, bank: &mut String, banks: &[String]) -> bool {
    let (strip, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 26.0), Sense::empty());
    ui.painter().rect_filled(strip, 0.0, OLED_BG);
    let cur = banks.iter().position(|b| b == bank).unwrap_or(0);
    let cell = (strip.width() / (banks.len() + 2) as f32).clamp(12.0, 26.0);
    let left = strip.center().x - cell * (banks.len() + 2) as f32 / 2.0;
    let mut pick = None;
    for i in 0..banks.len() + 2 {
        let (text, to) = if i == 0 {
            ("<", cur.checked_sub(1))
        } else if i == banks.len() + 1 {
            (">", (cur + 1 < banks.len()).then_some(cur + 1))
        } else {
            (banks[i - 1].as_str(), Some(i - 1))
        };
        let r = egui::Rect::from_min_size(
            egui::pos2(left + i as f32 * cell, strip.top() + 3.0),
            egui::vec2(cell, 20.0),
        );
        let resp = ui.interact(r, ui.scope_id().with(("bank", i)), Sense::click());
        if i > 0 && i <= banks.len() {
            if to == Some(cur) {
                ui.painter().rect_stroke(
                    r.shrink2(egui::vec2(4.0, 0.0)),
                    0.0,
                    Stroke::new(2.0, OLED),
                    egui::StrokeKind::Inside,
                );
            } else if resp.hovered() {
                ui.painter().rect_filled(r, 0.0, OLED_HOVER);
            }
            theme::color_badge(
                ui.painter(),
                r.shrink2(egui::vec2(4.0, 0.0)),
                theme::BadgeEdge::Bottom,
                theme::identity_color(i - 1, banks.len()),
            );
        }
        ui.painter().text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            text,
            oled_font(16.0),
            if to.is_some() { OLED } else { OLED_DIM },
        );
        if resp.clicked() {
            pick = to;
        }
    }
    if let Some(i) = pick {
        let changed = *bank != banks[i];
        *bank = banks[i].clone();
        changed
    } else {
        false
    }
}

fn pane(
    ui: &mut egui::Ui,
    st: &mut ElekState,
    side: usize,
    banks: &[String],
    present: bool,
    out: &mut Vec<ElekIntent>,
) {
    if bank_strip(ui, &mut st.presets.bank[side], banks) {
        st.presets.selected[side] = None;
    }
    let bank = st.presets.bank[side].clone();
    if present && !st.presets.listings.contains_key(&bank) {
        st.presets.listings.insert(bank.clone(), None);
        out.push(ElekIntent::ListPresets(bank.clone()));
    }
    let Some(Some(listing)) = st.presets.listings.get(&bank) else {
        ui.label("Reading bank…");
        return;
    };
    let entries = match listing {
        Ok(e) => e,
        Err(e) => {
            ui.colored_label(theme::CHANGED, e);
            return;
        }
    };
    ui.label(
        RichText::new(format!("BANK {bank} · {} PRESETS", entries.len()))
            .font(oled_font(12.0))
            .color(OLED_DIM),
    );
    let filter = st.presets.filter.to_uppercase();
    let rows: Vec<(u32, String, bool)> = (1..=256)
        .filter_map(|slot| {
            let entry = entries.iter().find(|e| e.slot == slot);
            if st.presets.hide_empty && entry.is_none() {
                return None;
            }
            let name = entry.map_or("—", |e| e.name.as_str());
            if !filter.is_empty()
                && !format!("{bank}:{slot:03} {name}")
                    .to_uppercase()
                    .contains(&filter)
            {
                return None;
            }
            Some((slot, name.to_owned(), entry.is_some()))
        })
        .collect();
    let height = (ui.available_height() * 0.57).max(60.0);
    let mut select = None;
    egui::Frame::new().fill(OLED_BG).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        egui::ScrollArea::vertical()
            .id_salt(("presets", &bank))
            .max_height(height)
            .min_scrolled_height(height)
            .auto_shrink([false, false])
            .show_rows(ui, 20.0, rows.len(), |ui, range| {
                for row in range {
                    let (slot, name, filled) = &rows[row];
                    let (r, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 20.0),
                        Sense::click(),
                    );
                    let selected = st.presets.selected[side] == Some(*slot);
                    if selected || resp.hovered() {
                        ui.painter().rect_filled(
                            r,
                            0.0,
                            if selected {
                                egui::Color32::from_rgb(42, 53, 51)
                            } else {
                                OLED_HOVER
                            },
                        );
                    }
                    ui.painter().text(
                        r.left_center() + egui::vec2(6.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        format!("{bank}:{slot:03}  {name}"),
                        oled_font(16.0),
                        if *filled { OLED } else { OLED_DIM },
                    );
                    if resp.clicked() {
                        select = Some((*slot, *filled));
                    }
                }
            });
    });
    if let Some((slot, filled)) = select {
        st.presets.selected[side] = Some(slot);
        if filled && present && !st.presets.sounds.contains_key(&(bank.clone(), slot)) {
            st.presets.sounds.insert((bank.clone(), slot), None);
            out.push(ElekIntent::ReadPreset {
                bank: bank.clone(),
                slot,
            });
        }
    }
    ui.add_space(5.0);
    egui::ScrollArea::vertical()
        .id_salt("preset_details")
        .max_height(ui.available_height())
        .show(ui, |ui| {
            details(ui, st, side, &bank, out);
        });
}

fn details(ui: &mut egui::Ui, st: &ElekState, side: usize, bank: &str, out: &mut Vec<ElekIntent>) {
    let Some(slot) = st.presets.selected[side] else {
        ui.label(
            RichText::new(
                "Select a preset to see its machine, sample, SRC values and sound-pool matches.",
            )
            .weak(),
        );
        return;
    };
    let preset = match st.presets.sounds.get(&(bank.to_owned(), slot)) {
        None => {
            ui.label("Empty preset slot.");
            return;
        }
        Some(None) => {
            ui.label("Reading preset…");
            return;
        }
        Some(Some(Err(e))) => {
            ui.colored_label(theme::CHANGED, e);
            return;
        }
        Some(Some(Ok(p))) => p,
    };
    ui.label(
        RichText::new(format!("{bank}:{slot:03}  {}", preset.sound.name))
            .font(oled_font(16.0))
            .color(OLED),
    );
    ui.label(
        RichText::new(format!("MACHINE  {}", machine_text(&preset.sound)))
            .font(oled_font(12.0))
            .color(OLED),
    );
    if let Some((key, path)) = &preset.sample {
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(path.is_some(), egui::Button::new("▶ Sample"))
                .on_hover_text(
                    "Play the source sample; preset synthesis and effects are not rendered",
                )
                .clicked()
            {
                out.push(ElekIntent::Local(*key, false));
            }
            ui.label(
                RichText::new(path.as_deref().unwrap_or("Sample not found on +Drive")).small(),
            );
        });
    }
    ui.horizontal_wrapped(|ui| {
        for i in 0..8 {
            if let Some(param) = param_at(&preset.sound, i) {
                // SMP is a project-local number at save time; show the
                // embedded sample's +Drive path above instead.
                if param.name == "SMP" || (!st.presets.show_defaults && is_default(param)) {
                    continue;
                }
                ui.label(
                    RichText::new(format!("{} {}", param.name, param.text))
                        .font(oled_font(12.0))
                        .color(OLED),
                )
                .on_hover_text(src_detail(&preset.sound));
            }
        }
    });
    match st.project.as_deref().filter(|p| p.sounds_read) {
        Some(project) => {
            let matches = project.preset_pool_matches(preset, &st.lib.sounds);
            let text = if matches.is_empty() {
                "No exact sound-pool match".into()
            } else {
                format!(
                    "Identical to sound pool {}",
                    matches
                        .iter()
                        .map(|i| format!("#{:03}", i + 1))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            ui.label(RichText::new(text).font(oled_font(12.0)).color(OLED_DIM))
                .on_hover_text("Compares every byte of the saved sound with the current sound pool, including pending pool edits.");
        }
        None => {
            ui.label(
                RichText::new("Load the current project to compare its sound pool.")
                    .weak()
                    .small(),
            );
        }
    }
}
