//! The editable Presets page: two independent +Drive bank browsers,
//! decoded sound details and exact matches in the current sound pool.

use crate::device::presets::{Preset, PresetEntry, PresetOp, PresetPush, PresetSlot};
use crate::ui::elek_library_view::{is_default, machine_text, param_at, src_detail};
use crate::ui::elek_view::{self, ElekIntent, ElekState};
use crate::ui::theme::{self, OLED, OLED_BG, OLED_DIM, OLED_HOVER, oled_font};
use egui::{RichText, Sense, Stroke};
use std::collections::{BTreeMap, HashMap};

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
    ops: Vec<PresetOp>,
    pub pushing: Option<(String, usize, usize)>,
    pub status: String,
    pub push_failed: bool,
    loading: (usize, usize),
    pub read_progress: f32,
    confirm_reload: bool,
    confirm_push: bool,
}

#[derive(Clone)]
struct PresetDrag(PresetSlot);

impl PresetView {
    pub fn begin_load(&mut self) {
        if self.loading.0 == self.loading.1 {
            self.loading = (0, 0);
        }
        self.loading.1 += 1;
    }

    pub fn end_load(&mut self) {
        self.loading.0 = (self.loading.0 + 1).min(self.loading.1);
        self.read_progress = 0.0;
    }

    fn loading(&self) -> bool {
        self.loading.0 < self.loading.1
    }

    pub fn reload(&mut self) {
        self.banks = None;
        self.asked = false;
        self.listings.clear();
        self.sounds.clear();
        self.ops.clear();
        self.push_failed = false;
        self.loading = (0, 0);
        self.read_progress = 0.0;
        self.status.clear();
    }

    pub fn device_reconnected(&mut self, other: bool) {
        if self.changed() > 0 && !other {
            if !self.push_failed {
                self.status = "Device reconnected; pending preset edits kept. Push checks for changes on the device before writing.".into();
            }
        } else {
            let dropped = self.changed();
            self.reload();
            if dropped > 0 {
                self.status = format!(
                    "Another device connected; {dropped} pending preset slot change(s) belonged to the previous device."
                );
            }
        }
    }

    fn base(&self, loc: &PresetSlot) -> Option<PresetSlot> {
        self.listings
            .get(&loc.bank)
            .and_then(|r| r.as_ref())
            .and_then(|r| r.as_ref().ok())
            .and_then(|entries| entries.iter().find(|e| e.slot == loc.slot))
            .map(|_| loc.clone())
    }

    fn edits(&self) -> BTreeMap<PresetSlot, Option<PresetSlot>> {
        let mut cells = BTreeMap::new();
        for op in &self.ops {
            for loc in op.slots() {
                cells.entry(loc.clone()).or_insert_with(|| self.base(&loc));
            }
            match op {
                PresetOp::Move(a, b) => {
                    let src = cells[a].clone();
                    cells.insert(b.clone(), src);
                    cells.insert(a.clone(), None);
                }
                PresetOp::Swap(a, b) => {
                    let src = cells[a].clone();
                    let dst = cells[b].clone();
                    cells.insert(a.clone(), dst);
                    cells.insert(b.clone(), src);
                }
                PresetOp::Clear(a) => {
                    cells.insert(a.clone(), None);
                }
            }
        }
        cells
    }

    fn origin(&self, loc: &PresetSlot) -> Option<PresetSlot> {
        self.edits()
            .get(loc)
            .cloned()
            .unwrap_or_else(|| self.base(loc))
    }

    fn changed(&self) -> usize {
        self.edits()
            .iter()
            .filter(|(loc, src)| self.base(loc) != **src)
            .count()
    }

    fn name(&self, loc: &PresetSlot) -> String {
        self.listings
            .get(&loc.bank)
            .and_then(|r| r.as_ref())
            .and_then(|r| r.as_ref().ok())
            .and_then(|entries| entries.iter().find(|e| e.slot == loc.slot))
            .map_or_else(|| "—".into(), |e| e.name.clone())
    }

    fn request_sound(&mut self, loc: &PresetSlot, out: &mut Vec<ElekIntent>) {
        let key = (loc.bank.clone(), loc.slot);
        if !self.sounds.contains_key(&key) {
            self.sounds.insert(key, None);
            out.push(ElekIntent::ReadPreset {
                bank: loc.bank.clone(),
                slot: loc.slot,
            });
        }
    }

    fn stage(&mut self, op: PresetOp, out: &mut Vec<ElekIntent>) {
        for loc in op.slots() {
            if self.base(&loc).is_some() {
                self.request_sound(&loc, out);
            }
        }
        self.ops.push(op);
        if self.changed() == 0 {
            self.ops.clear();
        }
        self.status.clear();
    }

    fn data(&self, loc: &PresetSlot) -> Option<Vec<u8>> {
        self.sounds
            .get(&(loc.bank.clone(), loc.slot))?
            .as_ref()?
            .as_ref()
            .ok()
            .map(|p| p.data.clone())
    }

    fn plan(&self) -> Option<PresetPush> {
        let mut before = Vec::new();
        let mut after = Vec::new();
        for (loc, src) in self.edits() {
            let old = if self.base(&loc).is_some() {
                Some(self.data(&loc)?)
            } else {
                None
            };
            let new = match src {
                Some(src) => Some(self.data(&src)?),
                None => None,
            };
            before.push((loc.clone(), old));
            after.push((loc, new));
        }
        Some(PresetPush {
            ops: self.ops.clone(),
            before,
            after,
        })
    }
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
            ops: Vec::new(),
            pushing: None,
            status: String::new(),
            push_failed: false,
            loading: (0, 0),
            read_progress: 0.0,
            confirm_reload: false,
            confirm_push: false,
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
    let changed = st.presets.changed();
    let busy = st.presets.pushing.is_some();
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(present && !busy && !st.presets.loading(), egui::Button::new("⬇ Reload presets")).clicked() {
            if changed > 0 {
                st.presets.confirm_reload = true;
            } else {
                st.presets.reload();
            }
        }
        let ready = present && !busy && !st.presets.push_failed && !st.presets.loading()
            && changed > 0 && st.presets.plan().is_some();
        if ui.add_enabled(ready, egui::Button::new(RichText::new("⬆ Push to device").color(theme::DEVICE_ACCENT_BRIGHT)))
            .on_hover_text("Save staged preset edits to +Drive after checking the device still has the presets that were read. Preset data must finish loading first.").clicked() {
            st.presets.confirm_push = true;
        }
        if ui.add_enabled(!busy && !st.presets.ops.is_empty(), egui::Button::new("Undo")).clicked() {
            st.presets.ops.pop();
        }
        if ui.add_enabled(!busy && changed > 0, egui::Button::new("Reset")).clicked() {
            st.presets.ops.clear();
        }
        if changed > 0 {
            ui.colored_label(theme::CHANGED, format!("{changed} changed slot(s) · not pushed"));
        }
    });
    if let Some((label, done, total)) = &st.presets.pushing {
        ui.add(
            theme::progress_bar(*done as f32 / (*total).max(1) as f32)
                .text(format!("{label} · {done}/{total}"))
                .desired_width(ui.available_width()),
        );
        ui.ctx().request_repaint();
    } else if st.presets.loading() {
        let (done, total) = st.presets.loading;
        ui.add(
            theme::progress_bar((done as f32 + st.presets.read_progress) / total.max(1) as f32)
                .text(format!("Loading presets · {done}/{total}"))
                .desired_width(ui.available_width()),
        );
        ui.ctx().request_repaint();
    }
    if !st.presets.status.is_empty() {
        ui.colored_label(
            if st.presets.push_failed {
                theme::CHANGED
            } else {
                OLED_DIM
            },
            &st.presets.status,
        );
    }
    ui.horizontal_wrapped(|ui| {
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
            RichText::new("Drag to move / swap · right-click to clear · edits wait for Push")
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
    if st.presets.confirm_reload {
        egui::Modal::new(egui::Id::unique("preset_reload")).show(ui.ctx(), |ui| {
            ui.heading("Discard preset edits and reload?");
            ui.label(format!("{changed} changed slot(s) have not been pushed."));
            ui.horizontal(|ui| {
                if ui.button("Discard and reload").clicked() {
                    st.presets.confirm_reload = false;
                    st.presets.reload();
                }
                if ui.button("Cancel").clicked() {
                    st.presets.confirm_reload = false;
                }
            });
        });
    }
    if st.presets.confirm_push {
        egui::Modal::new(egui::Id::unique("preset_push")).show(ui.ctx(), |ui| {
            ui.set_width(420.0);
            ui.heading(format!("Push {changed} changed preset slot(s)?"));
            ui.label("This saves the staged moves, swaps and clears directly to the preset banks on +Drive. Cleared presets are removed from their saved slots.");
            ui.horizontal(|ui| {
                if ui.button("Push presets").clicked() {
                    st.presets.confirm_push = false;
                    if let Some(plan) = st.presets.plan() {
                        st.presets.pushing = Some(("Queued preset push".into(), 0, 1));
                        st.presets.status.clear();
                        out.push(ElekIntent::PushPresets(plan));
                    }
                }
                if ui.button("Cancel").clicked() { st.presets.confirm_push = false; }
            });
        });
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
    if present && st.presets.pushing.is_none() && !st.presets.listings.contains_key(&bank) {
        st.presets.listings.insert(bank.clone(), None);
        out.push(ElekIntent::ListPresets(bank.clone()));
    }
    let Some(Some(listing)) = st.presets.listings.get(&bank) else {
        ui.label("Reading bank…");
        return;
    };
    match listing {
        Ok(_) => {}
        Err(e) => {
            ui.colored_label(theme::CHANGED, e);
            return;
        }
    };
    let edits = st.presets.edits();
    let origin_at = |loc: &PresetSlot| {
        edits
            .get(loc)
            .cloned()
            .unwrap_or_else(|| st.presets.base(loc))
    };
    let count = (1..=256)
        .filter(|slot| origin_at(&PresetSlot::new(&bank, *slot)).is_some())
        .count();
    ui.label(
        RichText::new(format!("BANK {bank} · {count} PRESETS"))
            .font(oled_font(12.0))
            .color(OLED_DIM),
    );
    let filter = st.presets.filter.to_uppercase();
    let rows: Vec<(u32, String, bool, bool)> = (1..=256)
        .filter_map(|slot| {
            let loc = PresetSlot::new(&bank, slot);
            let origin = origin_at(&loc);
            let changed = origin != st.presets.base(&loc);
            if st.presets.hide_empty && origin.is_none() && !changed {
                return None;
            }
            let name = origin
                .as_ref()
                .map_or_else(|| "—".into(), |src| st.presets.name(src));
            if !filter.is_empty()
                && !format!("{bank}:{slot:03} {name}")
                    .to_uppercase()
                    .contains(&filter)
            {
                return None;
            }
            Some((slot, name, origin.is_some(), changed))
        })
        .collect();
    let show_details = st.presets.selected[side].is_some();
    let height = if show_details {
        (ui.available_height() * 0.57).max(60.0)
    } else {
        ui.available_height()
    };
    let mut select = None;
    let mut edit = None;
    let can_edit = present && st.presets.pushing.is_none() && !st.presets.push_failed;
    egui::Frame::new().fill(OLED_BG).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        egui::ScrollArea::vertical()
            .id_salt(("presets", &bank))
            .max_height(height)
            .min_scrolled_height(height)
            .auto_shrink([false, false])
            .show_rows(ui, 20.0, rows.len(), |ui, range| {
                for row in range {
                    let (slot, name, filled, changed) = &rows[row];
                    let loc = PresetSlot::new(&bank, *slot);
                    let (r, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 20.0),
                        Sense::click_and_drag(),
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
                        format!(
                            "{bank}:{slot:03}  {name}{}",
                            if *changed { " *" } else { "" }
                        ),
                        oled_font(16.0),
                        if *changed {
                            theme::CHANGED
                        } else if *filled {
                            OLED
                        } else {
                            OLED_DIM
                        },
                    );
                    if resp.clicked() {
                        select = Some((*slot, *filled));
                    }
                    if can_edit && *filled && resp.drag_started() {
                        egui::DragAndDrop::set_payload(ui.ctx(), PresetDrag(loc.clone()));
                    }
                    if can_edit && *filled && resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                    }
                    if can_edit {
                        if let Some(drag) = resp.dnd_hover_payload::<PresetDrag>() {
                            if drag.0 != loc {
                                ui.painter().rect_stroke(
                                    r,
                                    0.0,
                                    Stroke::new(1.0, theme::DEVICE_ACCENT_BRIGHT),
                                    egui::StrokeKind::Inside,
                                );
                                theme::pointer_note(
                                    ui.ctx(),
                                    if *filled {
                                        "Swap presets"
                                    } else {
                                        "Move preset"
                                    },
                                    theme::DEVICE_ACCENT_BRIGHT,
                                );
                            }
                        }
                        if let Some(drag) = resp.dnd_release_payload::<PresetDrag>() {
                            if drag.0 != loc {
                                edit = Some(if *filled {
                                    PresetOp::Swap(drag.0.clone(), loc.clone())
                                } else {
                                    PresetOp::Move(drag.0.clone(), loc.clone())
                                });
                            }
                        }
                    }
                    resp.context_menu(|ui| {
                        ui.label(format!("Preset {bank}:{slot:03} · {name}"));
                        if ui
                            .add_enabled(can_edit && *filled, egui::Button::new("Clear"))
                            .clicked()
                        {
                            edit = Some(PresetOp::Clear(loc.clone()));
                            ui.close();
                        }
                    });
                }
            });
    });
    if let Some(op) = edit {
        st.presets.stage(op, out);
    }
    if let Some((slot, filled)) = select {
        st.presets.selected[side] = Some(slot);
        if filled && present && st.presets.pushing.is_none() {
            if let Some(origin) = st.presets.origin(&PresetSlot::new(&bank, slot)) {
                st.presets.request_sound(&origin, out);
            }
        }
    }
    if show_details {
        ui.add_space(5.0);
        egui::ScrollArea::vertical()
            .id_salt("preset_details")
            .max_height(ui.available_height())
            .show(ui, |ui| {
                details(ui, st, side, &bank, out);
            });
    }
}

fn details(ui: &mut egui::Ui, st: &ElekState, side: usize, bank: &str, out: &mut Vec<ElekIntent>) {
    let Some(slot) = st.presets.selected[side] else {
        return;
    };
    let Some(origin) = st.presets.origin(&PresetSlot::new(bank, slot)) else {
        ui.label("Empty preset slot.");
        return;
    };
    let preset = match st.presets.sounds.get(&(origin.bank, origin.slot)) {
        None => {
            ui.label("Select this preset to load its details.");
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
