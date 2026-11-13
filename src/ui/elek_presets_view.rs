//! The editable Presets page: two independent +Drive bank browsers,
//! sound copy/paste with the current sound pool.
//! Selection and ordering use slot_list, shared with the Samples page;
//! permutations become grouped native preset moves/swaps only after bank reads.

use crate::device::presets::{Preset, PresetEntry, PresetOp, PresetPush, PresetSlot};
use crate::ui::drag_drop::ResponseDnd;
use crate::ui::elek_view::{self, ElekIntent, ElekState};
use crate::ui::slot_list::{self, DropTarget};
use crate::ui::theme::{self, OLED, OLED_BG, OLED_DIM, OLED_HOVER, oled_font};
use egui::{RichText, Sense, Stroke};
use std::collections::{BTreeMap, BTreeSet, HashMap};

type Read<T> = Option<Result<T, String>>;

pub struct PresetView {
    pub banks: Read<Vec<String>>,
    asked: bool,
    pub listings: HashMap<String, Read<Vec<PresetEntry>>>,
    pub sounds: HashMap<(String, u32), Read<Preset>>,
    bank: [String; 2],
    selected: [Option<u32>; 2],
    sel: BTreeSet<usize>,
    cursor: Option<usize>,
    active: usize,
    reveal: Option<usize>,
    undo: Vec<usize>,
    pending: Option<Reorder>,
    filter: String,
    hide_empty: bool,
    ops: Vec<PresetOp>,
    pub pushing: Option<(String, usize, usize)>,
    pub status: String,
    pub push_failed: bool,
    loading: (usize, usize),
    pub read_progress: f32,
    confirm_reload: bool,
    confirm_push: bool,
    copy_pending: Option<(PresetSlot, PresetSource)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PresetSource {
    Saved(PresetSlot),
    Imported(usize),
}

#[derive(Clone)]
struct PresetDrag;

struct Reorder {
    banks: Vec<String>,
    order: Vec<usize>,
    sel: BTreeSet<usize>,
    cursor: Option<usize>,
    clear: Option<Vec<usize>>,
    side: usize,
}

#[derive(Clone, Copy)]
enum Edit {
    Drop(DropTarget),
    Nudge(i32),
    InsertEmpty(usize, usize),
    CloseGap(usize),
    Clear(usize),
}

fn location(banks: &[String], index: usize) -> PresetSlot {
    PresetSlot::new(&banks[index / 256], (index % 256 + 1) as u32)
}

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
        self.undo.clear();
        self.pending = None;
        self.copy_pending = None;
        self.sel.clear();
        self.cursor = None;
        self.selected = [None; 2];
        self.reveal = None;
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

    fn base(&self, loc: &PresetSlot) -> Option<PresetSource> {
        self.listings
            .get(&loc.bank)
            .and_then(|r| r.as_ref())
            .and_then(|r| r.as_ref().ok())
            .and_then(|entries| entries.iter().find(|e| e.slot == loc.slot))
            .map(|_| PresetSource::Saved(loc.clone()))
    }

    fn edits(&self) -> BTreeMap<PresetSlot, Option<PresetSource>> {
        let mut cells = BTreeMap::new();
        for (index, op) in self.ops.iter().enumerate() {
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
                PresetOp::Write(a, _) => {
                    cells.insert(a.clone(), Some(PresetSource::Imported(index)));
                }
            }
        }
        cells
    }

    fn origin(&self, loc: &PresetSlot) -> Option<PresetSource> {
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

    fn preset(&self, src: &PresetSource) -> Read<&Preset> {
        match src {
            PresetSource::Saved(loc) => self.sounds.get(&(loc.bank.clone(), loc.slot))
                .and_then(|r| r.as_ref()).map(|r| r.as_ref().map_err(Clone::clone)),
            PresetSource::Imported(index) => match self.ops.get(*index) {
                Some(PresetOp::Write(_, preset)) => Some(Ok(preset)),
                _ => None,
            },
        }
    }

    fn source_name(&self, src: &PresetSource) -> String {
        match src {
            PresetSource::Saved(loc) => self.name(loc),
            PresetSource::Imported(_) => self.preset(src).and_then(Result::ok)
                .map_or_else(|| "—".into(), |p| p.sound.name.clone()),
        }
    }

    fn request_sound(&mut self, src: &PresetSource, out: &mut Vec<ElekIntent>) {
        let PresetSource::Saved(loc) = src else { return; };
        let key = (loc.bank.clone(), loc.slot);
        if !self.sounds.contains_key(&key) {
            self.sounds.insert(key, None);
            out.push(ElekIntent::ReadPreset {
                bank: loc.bank.clone(),
                slot: loc.slot,
            });
        }
    }

    fn stage(&mut self, ops: Vec<PresetOp>, out: &mut Vec<ElekIntent>) {
        if ops.is_empty() {
            return;
        }
        let touched: BTreeSet<_> = ops.iter().flat_map(PresetOp::slots).collect();
        for loc in touched {
            if let Some(src) = self.base(&loc) {
                self.request_sound(&src, out);
            }
        }
        self.undo.push(self.ops.len());
        self.ops.extend(ops);
        if self.changed() == 0 {
            self.ops.clear();
            self.undo.clear();
        }
        self.status.clear();
    }

    fn undo(&mut self) {
        if let Some(len) = self.undo.pop() {
            self.ops.truncate(len);
        }
    }

    fn select(&mut self, banks: &[String], side: usize, index: usize, modifiers: egui::Modifiers) {
        slot_list::click(&mut self.sel, &mut self.cursor, index, modifiers, 0);
        self.active = side;
        self.selected[side] = Some((index % 256 + 1) as u32);
        self.bank[side] = banks[index / 256].clone();
    }

    fn edit(&mut self, banks: &[String], action: Edit, out: &mut Vec<ElekIntent>) {
        if let Edit::Clear(at) = action {
            let pick: Vec<_> = if self.sel.contains(&at) {
                self.sel.iter().copied().collect()
            } else {
                vec![at]
            };
            self.pending = Some(Reorder {
                banks: banks.to_vec(),
                order: Vec::new(),
                sel: self.sel.clone(),
                cursor: self.cursor,
                clear: Some(pick),
                side: self.active,
            });
            self.apply_pending(out);
            return;
        }
        let mut order: Vec<usize> = (0..banks.len() * 256).collect();
        let sel: Vec<_> = self.sel.iter().copied().collect();
        match action {
            Edit::Drop(DropTarget::Place(at)) => {
                slot_list::place(&mut order, &sel, at, 0);
            }
            Edit::Drop(DropTarget::Insert(at)) => slot_list::insert(&mut order, &sel, at, 0),
            Edit::Nudge(dir) => slot_list::nudge(&mut order, &sel, dir, 0),
            Edit::InsertEmpty(at, n) => {
                let start = at / 256 * 256;
                let (made, _) =
                    slot_list::insert_empty(&mut order[start..start + 256], at - start, n, |i| {
                        self.origin(&location(banks, *i)).is_none()
                    });
                if made == 0 {
                    self.status = "No empty slot below in this bank to make room".into();
                    return;
                }
            }
            Edit::CloseGap(at) => {
                let start = at / 256 * 256;
                slot_list::close_gap(&mut order[start..start + 256], at - start, |i| {
                    self.origin(&location(banks, *i)).is_none()
                });
            }
            Edit::Clear(_) => unreachable!(),
        }
        if order.iter().enumerate().all(|(i, old)| i == *old) {
            return;
        }
        self.pending = Some(Reorder {
            banks: banks.to_vec(),
            order,
            sel: self.sel.clone(),
            cursor: self.cursor,
            clear: None,
            side: self.active,
        });
        self.apply_pending(out);
    }

    /// Translate the shared permutation into native moves/swaps. Read every
    /// affected bank first, including intermediate banks on cross-bank inserts.
    fn apply_pending(&mut self, out: &mut Vec<ElekIntent>) {
        let Some(edit) = self.pending.as_ref() else {
            return;
        };
        let touched: BTreeSet<_> = if let Some(pick) = &edit.clear {
            pick.iter().map(|i| edit.banks[i / 256].clone()).collect()
        } else {
            edit.order
                .iter()
                .enumerate()
                .filter(|(i, old)| *i != **old)
                .map(|(i, _)| edit.banks[i / 256].clone())
                .collect()
        };
        let mut waiting = false;
        for bank in touched {
            match self.listings.get(&bank) {
                Some(Some(Ok(_))) => {}
                Some(Some(Err(e))) => {
                    self.status = format!("Cannot edit bank {bank}: {e}");
                    self.pending = None;
                    return;
                }
                Some(None) => waiting = true,
                None => {
                    self.listings.insert(bank.clone(), None);
                    out.push(ElekIntent::ListPresets(bank));
                    waiting = true;
                }
            }
        }
        if waiting {
            self.status = "Reading affected banks before staging the group edit…".into();
            return;
        }
        let edit = self.pending.take().unwrap();
        self.status.clear();
        let edits = self.edits();
        if let Some(pick) = edit.clear {
            let ops = pick
                .into_iter()
                .map(|i| location(&edit.banks, i))
                .filter(|loc| {
                    edits
                        .get(loc)
                        .cloned()
                        .unwrap_or_else(|| self.base(loc))
                        .is_some()
                })
                .map(PresetOp::Clear)
                .collect();
            self.stage(ops, out);
            return;
        }
        let n = edit.order.len();
        let origins: Vec<_> = (0..n)
            .map(|i| {
                let loc = location(&edit.banks, i);
                edits.get(&loc).cloned().unwrap_or_else(|| self.base(&loc))
            })
            .collect();
        let mut current: Vec<_> = (0..n).collect();
        let mut positions = current.clone();
        let mut ops = Vec::new();
        for at in 0..n {
            let from = positions[edit.order[at]];
            if at == from {
                continue;
            }
            let a = location(&edit.banks, at);
            let b = location(&edit.banks, from);
            match (
                origins[current[at]].is_some(),
                origins[current[from]].is_some(),
            ) {
                (true, true) => ops.push(PresetOp::Swap(a, b)),
                (false, true) => ops.push(PresetOp::Move(b, a)),
                (true, false) => ops.push(PresetOp::Move(a, b)),
                (false, false) => {}
            }
            current.swap(at, from);
            positions[current[at]] = at;
            positions[current[from]] = from;
        }
        self.sel = edit
            .order
            .iter()
            .enumerate()
            .filter(|(_, old)| edit.sel.contains(old))
            .map(|(i, _)| i)
            .collect();
        self.cursor = edit
            .cursor
            .and_then(|c| edit.order.iter().position(|old| *old == c));
        self.active = edit.side;
        if let Some(c) = self.cursor {
            self.bank[self.active] = edit.banks[c / 256].clone();
            self.selected[self.active] = Some((c % 256 + 1) as u32);
            self.reveal = Some(c);
        }
        self.stage(ops, out);
    }

    fn keys(
        &mut self,
        ctx: &egui::Context,
        enabled: bool,
        can_edit: bool,
        out: &mut Vec<ElekIntent>,
    ) {
        if !enabled || ctx.egui_wants_keyboard_input() {
            return;
        }
        if self.pending.is_some() {
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.pending = None;
                self.status = "Cancelled queued preset edit".into();
            }
            return;
        }
        let Some(Ok(banks)) = self.banks.as_ref() else {
            return;
        };
        let banks = banks.clone();
        if banks.is_empty() {
            return;
        }
        let (m, up, down, pgup, pgdn, a, z, esc) = ctx.input(|i| {
            (
                i.modifiers,
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::PageUp),
                i.key_pressed(egui::Key::PageDown),
                i.key_pressed(egui::Key::A),
                i.key_pressed(egui::Key::Z),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if esc {
            self.sel.clear();
        }
        if m.command && z && can_edit {
            self.undo();
        }
        let start = banks
            .iter()
            .position(|b| *b == self.bank[self.active])
            .unwrap_or(0)
            * 256;
        if m.command && a {
            self.sel = (start..start + 256).collect();
        }
        let step: isize = if up {
            -1
        } else if down {
            1
        } else if pgup {
            -16
        } else if pgdn {
            16
        } else {
            0
        };
        if step == 0 {
            return;
        }
        if m.alt && (up || down) {
            if can_edit {
                self.edit(&banks, Edit::Nudge(step as i32), out);
            }
            return;
        }
        let next = (self.cursor.unwrap_or(start) as isize + step)
            .clamp(0, banks.len() as isize * 256 - 1) as usize;
        if m.shift {
            self.sel.insert(self.cursor.unwrap_or(next));
            self.sel.insert(next);
            self.cursor = Some(next);
            self.bank[self.active] = banks[next / 256].clone();
            self.selected[self.active] = Some((next % 256 + 1) as u32);
        } else {
            self.select(&banks, self.active, next, egui::Modifiers::NONE);
        }
        self.reveal = Some(next);
    }

    fn data(&self, src: &PresetSource) -> Option<&Vec<u8>> {
        self.preset(src)?.ok().map(|p| &p.data)
    }

    fn ready(&self) -> bool {
        self.edits().iter().all(|(loc, src)| {
            (self.base(loc).is_none() || self.data(&PresetSource::Saved(loc.clone())).is_some())
                && src.as_ref().is_none_or(|src| self.data(src).is_some())
        })
    }

    fn plan(&self) -> Option<PresetPush> {
        let mut before = Vec::new();
        let mut after = Vec::new();
        for (loc, src) in self.edits() {
            let old = if self.base(&loc).is_some() {
                Some(self.data(&PresetSource::Saved(loc.clone()))?.clone())
            } else {
                None
            };
            let new = match src {
                Some(src) => Some(self.data(&src)?.clone()),
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
            sel: BTreeSet::new(),
            cursor: None,
            active: 0,
            reveal: None,
            undo: Vec::new(),
            pending: None,
            filter: String::new(),
            hide_empty: false,
            ops: Vec::new(),
            pushing: None,
            status: String::new(),
            push_failed: false,
            loading: (0, 0),
            read_progress: 0.0,
            confirm_reload: false,
            confirm_push: false,
            copy_pending: None,
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
    if present && st.presets.pushing.is_none() && !st.presets.push_failed {
        st.presets.apply_pending(&mut out);
    }
    let can_edit = present
        && st.presets.pushing.is_none()
        && !st.presets.push_failed
        && st.presets.pending.is_none();
    st.presets
        .keys(ui.ctx(), st.keys_on && present, can_edit, &mut out);
    finish_copy(st, ui.ctx());
    if st.keys_on && present && !ui.ctx().egui_wants_keyboard_input() {
        let (copy, paste) = ui.input(|i| (
            i.events.iter().any(|e| matches!(e, egui::Event::Copy)) || (i.modifiers.command_only() && i.key_pressed(egui::Key::C)),
            i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))) || (i.modifiers.command_only() && i.key_pressed(egui::Key::V)),
        ));
        if let Some(slot) = st.presets.selected[st.presets.active] {
            let loc = PresetSlot::new(&st.presets.bank[st.presets.active], slot);
            if copy { copy_preset(st, loc.clone(), &mut out, ui.ctx()); }
            if paste && can_edit { paste_sound(st, loc, &mut out); }
        }
    }
    let changed = st.presets.changed();
    let busy = st.presets.pushing.is_some() || st.presets.pending.is_some();
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(present && !busy && !st.presets.loading(), egui::Button::new("⬇ Reload presets")).clicked() {
            if changed > 0 {
                st.presets.confirm_reload = true;
            } else {
                st.presets.reload();
            }
        }
        let ready = present && !busy && !st.presets.push_failed && !st.presets.loading()
            && changed > 0 && st.presets.ready();
        if ui.add_enabled(ready, egui::Button::new(RichText::new("⬆ Push to device").color(theme::DEVICE_ACCENT_BRIGHT)))
            .on_hover_text("Save staged preset edits to +Drive after checking the device still has the presets that were read. Preset data must finish loading first.").clicked() {
            st.presets.confirm_push = true;
        }
        if ui.add_enabled(!busy && !st.presets.ops.is_empty(), egui::Button::new("Undo")).clicked() {
            st.presets.undo();
        }
        if ui.add_enabled(!busy && changed > 0, egui::Button::new("Reset")).clicked() {
            st.presets.ops.clear();
            st.presets.undo.clear();
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
    } else {
        ui.add(
            theme::progress_bar(0.0)
                .desired_width(ui.available_width()),
        );
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
            ui.label("This saves the staged pastes, moves, swaps and clears directly to the preset banks on +Drive. Cleared presets are removed from their saved slots.");
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

fn copy_preset(st: &mut ElekState, loc: PresetSlot, out: &mut Vec<ElekIntent>, ctx: &egui::Context) {
    let Some(src) = st.presets.origin(&loc) else {
        st.presets.status = "Empty preset slot: nothing to copy".into();
        return;
    };
    // A slow read must not leave the previous sound available for Paste.
    st.proj.clear_sound_clipboard();
    st.presets.request_sound(&src, out);
    st.presets.copy_pending = Some((loc, src));
    st.presets.status = "Loading preset for the sound clipboard…".into();
    finish_copy(st, ctx);
}

/// Complete queued copies even when the user switches to the Sounds page
/// before the device finishes reading the preset.
pub(crate) fn finish_copy(st: &mut ElekState, ctx: &egui::Context) {
    let Some((loc, src)) = st.presets.copy_pending.clone() else { return; };
    let Some(result) = st.presets.preset(&src) else { return; };
    let preset = match result {
        Ok(p) => p.clone(),
        Err(e) => {
            st.presets.status = format!("Could not copy preset: {e}");
            st.presets.copy_pending = None;
            return;
        }
    };
    let cell = crate::device::library::SoundCell {
        src: crate::device::library::SoundSrc::Preset { model: preset.model, data: preset.data },
        name: None,
    };
    let track_ok = st.project.as_deref().ok_or_else(|| "No project loaded".to_owned())
        .and_then(|p| p.accepts_preset(preset.model, match &cell.src {
            crate::device::library::SoundSrc::Preset { data, .. } => data, _ => unreachable!(),
        }))
        // A preset's sample slot must be translated through the pool first.
        .and_then(|_| if preset.sample.is_some() { Err("Paste this preset into the sound pool first to resolve its sample".into()) } else { Ok(()) });
    st.proj.set_pool_clip(crate::ui::elek_project_view::PoolClip {
        at: 0, cell, name: preset.sound.name, track_ok,
    });
    st.presets.copy_pending = None;
    let msg = format!("Copied preset {}:{:03}. Paste into Sounds or a preset slot; Push saves the edit", loc.bank, loc.slot);
    ctx.copy_text(msg.clone());
    st.status = msg.clone();
    st.presets.status = msg;
}

fn paste_sound(st: &mut ElekState, loc: PresetSlot, out: &mut Vec<ElekIntent>) {
    let Some(clip) = st.proj.pool_clip.clone() else {
        st.presets.status = "Copy a sound on the Sounds or Presets page first".into();
        return;
    };
    let mut preset = match &clip.cell.src {
        crate::device::library::SoundSrc::Preset { model, data } => {
            let data = if let Some(name) = &clip.cell.name {
                let mut d = data.clone();
                let name = crate::device::patterns::clean_name(name);
                d[12..28].fill(0);
                d[12..12 + name.len()].copy_from_slice(name.as_bytes());
                d
            } else { data.clone() };
            crate::device::presets::Preset {
                model: *model, container: Vec::new(),
                sound: crate::device::library::parse_sound(*model, 0, &data),
                sample: crate::device::presets::sample_key(*model, &data).map(|key| (key, None)),
                data,
            }
        }
        _ => {
            let Some(project) = st.project.as_deref() else {
                st.presets.status = "Load the project the copied pool sound came from first".into();
                return;
            };
            match project.export_preset(&clip.cell, &st.fills) {
                Ok(p) => p,
                Err(e) => { st.presets.status = e; return; }
            }
        }
    };
    if let Some((key, path)) = &mut preset.sample {
        *path = st.names.get(key).cloned();
    }
    st.presets.stage(vec![PresetOp::Write(loc.clone(), preset)], out);
    st.presets.status = format!("{} pasted into preset {}:{:03}. Push on Presets to save it to +Drive", clip.desc(), loc.bank, loc.slot);
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
        if resp.clicked()
            || (i > 0 && i <= banks.len() && resp.dnd_hover_payload::<PresetDrag>().is_some())
        {
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
        st.presets.active = side;
    }
    let bank = st.presets.bank[side].clone();
    let start = banks.iter().position(|b| *b == bank).unwrap_or(0) * 256;
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
                .map_or_else(|| "—".into(), |src| st.presets.source_name(src));
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
    let height = ui.available_height();
    let mut select = None;
    let mut edit = None;
    let mut copy = None;
    let mut paste = None;
    let can_edit = present
        && st.presets.pushing.is_none()
        && !st.presets.push_failed
        && st.presets.pending.is_none();
    let reveal_row = st
        .presets
        .reveal
        .filter(|_| st.presets.active == side)
        .and_then(|index| {
            rows.iter()
                .position(|(slot, _, _, _)| start + *slot as usize - 1 == index)
        });
    egui::Frame::new().fill(OLED_BG).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt(("presets", &bank))
            .max_height(height)
            .min_scrolled_height(height)
            .auto_shrink([false, false]);
        if let Some(row) = reveal_row {
            scroll = scroll.vertical_scroll_offset(
                (row as f32 * (20.0 + ui.spacing().item_spacing.y) - height * 0.5).max(0.0),
            );
            st.presets.reveal = None;
        }
        scroll.show_rows(ui, 20.0, rows.len(), |ui, range| {
            for row in range {
                let (slot, name, filled, changed) = &rows[row];
                let index = start + *slot as usize - 1;
                let (r, resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 20.0),
                    Sense::click_and_drag(),
                );
                let selected = st.presets.sel.contains(&index);
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
                if resp.clicked() && st.presets.pending.is_none() {
                    select = Some((index, ui.input(|i| i.modifiers)));
                }
                if can_edit && resp.drag_started() {
                    if !selected {
                        st.presets.select(banks, side, index, egui::Modifiers::NONE);
                    }
                    st.presets.active = side;
                    resp.dnd_set_drag_payload(PresetDrag);
                }
                if can_edit && resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                }
                if can_edit {
                    let target = slot_list::drop_target(ui, r, index);
                    if resp.dnd_hover_payload::<PresetDrag>().is_some() {
                        slot_list::paint_drop(
                            ui,
                            r,
                            index,
                            target,
                            st.presets.sel.len().min(banks.len() * 256 - index),
                        );
                        theme::pointer_note(
                            ui.ctx(),
                            match target {
                                DropTarget::Insert(_) => "Insert selection and shift slots",
                                DropTarget::Place(_) => "Place selection / swap occupied slots",
                            },
                            theme::DEVICE_ACCENT_BRIGHT,
                        );
                    }
                    if resp.release_payload::<PresetDrag>().is_some() {
                        st.presets.active = side;
                        edit = Some(Edit::Drop(target));
                    }
                }
                resp.context_menu(|ui| {
                    ui.label(format!("Preset {bank}:{slot:03} · {name}"));
                    if ui.add_enabled(*filled && present, egui::Button::new("Copy sound  (⌘C)")).clicked() {
                        copy = Some(PresetSlot::new(&bank, *slot));
                        ui.close();
                    }
                    let copied = st.proj.pool_clip.as_ref().map(|c| c.desc());
                    if ui.add_enabled(can_edit && copied.is_some(), egui::Button::new(
                        format!("Paste {}{}  (⌘V)", copied.as_deref().unwrap_or("sound"), if *filled { " over this preset" } else { " here" })
                    )).clicked() {
                        paste = Some((side, index));
                        ui.close();
                    }
                    ui.separator();
                    ui.add_enabled_ui(can_edit, |ui| {
                        if ui
                            .button(if selected {
                                format!("Clear {} selected slot(s)", st.presets.sel.len())
                            } else {
                                "Clear".into()
                            })
                            .clicked()
                        {
                            edit = Some(Edit::Clear(index));
                            ui.close();
                        }
                        if ui.button("Insert empty slot here").clicked() {
                            edit = Some(Edit::InsertEmpty(index, 1));
                            ui.close();
                        }
                        if selected
                            && st.presets.sel.len() > 1
                            && ui
                                .button(format!(
                                    "Insert {} empty slots above selection",
                                    st.presets.sel.len()
                                ))
                                .clicked()
                        {
                            edit = Some(Edit::InsertEmpty(
                                *st.presets.sel.first().unwrap(),
                                st.presets.sel.len(),
                            ));
                            ui.close();
                        }
                        if !filled && ui.button("Remove this empty slot").clicked() {
                            edit = Some(Edit::CloseGap(index));
                            ui.close();
                        }
                        if !st.presets.sel.is_empty() {
                            if ui
                                .button(format!("Move {} selected here", st.presets.sel.len()))
                                .clicked()
                            {
                                st.presets.active = side;
                                edit = Some(Edit::Drop(DropTarget::Insert(index)));
                                ui.close();
                            }
                            if selected {
                                for (label, dir) in [
                                    ("Move selection up  (⌥↑)", -1),
                                    ("Move selection down  (⌥↓)", 1),
                                ] {
                                    if ui.button(label).clicked() {
                                        edit = Some(Edit::Nudge(dir));
                                        ui.close();
                                    }
                                }
                            }
                        }
                    });
                });
            }
        });
    });
    if let Some(loc) = copy { copy_preset(st, loc, out, ui.ctx()); }
    if let Some((side, index)) = paste {
        st.presets.select(banks, side, index, egui::Modifiers::NONE);
        paste_sound(st, location(banks, index), out);
    }
    if let Some(action) = edit {
        st.presets.edit(banks, action, out);
    }
    if let Some((index, modifiers)) = select {
        st.presets.select(banks, side, index, modifiers);
    }
}
