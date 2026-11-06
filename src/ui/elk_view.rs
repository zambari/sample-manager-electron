//! The Elk Sample Order view: the device's sample pool in two side-by-side
//! lists, so a slot can be dragged onto any other (in any bank) to swap them.

use crate::device::{ElkProject, PoolEntry, SavedProject};
use crate::ui::theme;
use egui::{Color32, RichText, Sense, Stroke};
use std::collections::{BTreeSet, HashMap, HashSet};

const ROW_H: f32 = 20.0;
const TEXT: f32 = 16.0;
const BANK: usize = 128;

/// Every piece of slot-list text goes through here: the Silkscreen pixel font.
pub fn oled_font(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(theme::OLED_FAMILY.into()))
}

/// Readout green-white, like the device screen.
const OLED: Color32 = Color32::from_rgb(222, 236, 230);
const OLED_DIM: Color32 = Color32::from_rgb(110, 122, 118);
const OLED_BG: Color32 = Color32::from_rgb(12, 14, 14);

#[derive(Default)]
pub struct ElkState {
    pub projects: Option<Result<Vec<SavedProject>, String>>,
    pub project: Option<Box<ElkProject>>,
    /// `order[slot] = slot it came from` in the project as read.
    pub order: Vec<u16>,
    /// +Drive paths by `(hash, length)`.
    pub names: HashMap<(u32, u32), String>,
    pub bank: [usize; 2],
    /// `(label, done, total)` while the device works.
    pub busy: Option<(String, usize, usize)>,
    pub status: String,
    pub confirm_push: bool,
    /// Asking whether to drop unpushed changes and close the project.
    confirm_close: bool,
    /// After a push: the pool the device should now have.
    pub expect: Option<Vec<PoolEntry>>,
    pub asked_projects: bool,
    /// Selected slots (positions in the new order). Slot 0 is never in it.
    sel: BTreeSet<usize>,
    /// Keyboard cursor / shift-click anchor.
    cursor: Option<usize>,
    /// Pane the keyboard drives.
    active: usize,
    /// Earlier orders, for undo.
    undo: Vec<Vec<u16>>,
    /// Scroll offset of each pane last frame, and a slot to bring into view.
    scroll: [f32; 2],
    view_h: [f32; 2],
    reveal: Option<usize>,
}

pub enum ElkIntent {
    BackToEditor,
    ListProjects,
    Read,
    Push,
}

impl ElkState {
    pub fn changed(&self) -> usize {
        self.order
            .iter()
            .enumerate()
            .filter(|(i, o)| *i != **o as usize)
            .count()
    }

    pub fn set_project(&mut self, p: Box<ElkProject>) {
        // The full read landing on the pool shown early: keep the edits made
        // while it streamed in.
        if self.project.as_ref().is_some_and(|old| old.partial && old.pool == p.pool) && self.order.len() == p.pool.len() {
            self.project = Some(p);
            return;
        }
        self.order = (0..p.pool.len() as u16).collect();
        let banks = p.pool.len().div_ceil(BANK);
        self.bank = [0, 1.min(banks - 1)];
        self.sel.clear();
        self.cursor = None;
        self.undo.clear();
        self.project = Some(p);
    }

    /// Drops the loaded project, back to the project picker.
    fn close_project(&mut self) {
        self.project = None;
        self.order.clear();
        self.sel.clear();
        self.cursor = None;
        self.undo.clear();
        self.status.clear();
    }

    /// The pool as it will be after a push.
    pub fn new_pool(&self) -> Vec<PoolEntry> {
        let Some(p) = &self.project else {
            return Vec::new();
        };
        self.order.iter().map(|&o| p.pool[o as usize]).collect()
    }

    fn label(&self, slot: usize) -> String {
        let banks = self
            .project
            .as_ref()
            .map_or(1, |p| p.pool.len().div_ceil(BANK));
        if banks > 1 {
            format!("{}{:03}", (b'A' + (slot / BANK) as u8) as char, slot % BANK)
        } else {
            format!("{slot:03}")
        }
    }

    /// Runs an edit of `order`, keeping the selection on the same samples and
    /// recording undo when something changed.
    fn edit(&mut self, f: impl FnOnce(&mut Vec<u16>)) {
        let before = self.order.clone();
        let picked: HashSet<u16> = self.sel.iter().map(|&s| self.order[s]).collect();
        let cur = self.cursor.map(|c| self.order[c]);
        f(&mut self.order);
        if self.order == before {
            return;
        }
        self.undo.push(before);
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
        self.sel = self
            .order
            .iter()
            .enumerate()
            .filter(|(_, o)| picked.contains(o))
            .map(|(i, _)| i)
            .collect();
        self.cursor = cur.and_then(|c| self.order.iter().position(|&o| o == c));
        self.reveal = self.cursor;
    }

    fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.order = prev;
            self.status = "Undone".into();
        }
    }

    /// Moves the selection so it starts right before slot `to` (as numbered
    /// now). Everything in between closes up.
    fn move_sel_to(&mut self, to: usize) {
        let sel: Vec<usize> = self.sel.iter().copied().collect();
        if sel.is_empty() {
            return;
        }
        let n = self.order.len();
        let to = to.clamp(1, n);
        self.edit(|order| {
            let moved: Vec<u16> = sel.iter().map(|&s| order[s]).collect();
            let set: HashSet<usize> = sel.iter().copied().collect();
            let mut rest: Vec<u16> = order
                .iter()
                .enumerate()
                .filter(|(i, _)| !set.contains(i))
                .map(|(_, &o)| o)
                .collect();
            let at = to - sel.iter().filter(|&&s| s < to).count();
            rest.splice(at..at, moved);
            *order = rest;
        });
        if let (Some(&a), Some(&b)) = (self.sel.first(), self.sel.last()) {
            self.status = if a == b {
                format!("Moved to {}", self.label(a))
            } else {
                format!("Moved to {}–{}", self.label(a), self.label(b))
            };
        }
    }

    /// Puts the selection into slots `to, to+1, …` (in selection order). What
    /// was there goes to the slots the selection left, so nothing else moves:
    /// dropping onto empty slots leaves empty slots behind.
    fn place_sel_at(&mut self, to: usize) {
        let sel: Vec<usize> = self.sel.iter().copied().collect();
        let n = self.order.len();
        if sel.is_empty() {
            return;
        }
        let to = to.clamp(1, n - sel.len());
        let targets: Vec<usize> = (to..to + sel.len()).collect();
        let tset: HashSet<usize> = targets.iter().copied().collect();
        let sset: HashSet<usize> = sel.iter().copied().collect();
        let displaced: Vec<usize> = targets
            .iter()
            .copied()
            .filter(|t| !sset.contains(t))
            .collect();
        let freed: Vec<usize> = sel.iter().copied().filter(|s| !tset.contains(s)).collect();
        let occupied = displaced.iter().filter(|&&t| !self.is_empty_at(t)).count();
        self.edit(|order| {
            let old = order.clone();
            for (&s, &t) in sel.iter().zip(&targets) {
                order[t] = old[s];
            }
            for (&d, &f) in displaced.iter().zip(&freed) {
                order[f] = old[d];
            }
        });
        let what = if sel.len() == 1 {
            self.label(to)
        } else {
            format!("{}–{}", self.label(to), self.label(to + sel.len() - 1))
        };
        self.status = if occupied > 0 {
            format!("Placed at {what}; {occupied} sample(s) there swapped into the old slots")
        } else {
            format!("Placed at {what}")
        };
    }

    fn is_empty_at(&self, slot: usize) -> bool {
        self.project
            .as_ref()
            .is_some_and(|p| p.pool[self.order[slot] as usize].is_empty())
    }

    /// Shifts the selection one slot up (`-1`) or down (`+1`), as a block.
    fn nudge(&mut self, dir: i32) {
        let sel: Vec<usize> = self.sel.iter().copied().collect();
        let n = self.order.len();
        let (Some(&lo), Some(&hi)) = (sel.first(), sel.last()) else {
            return;
        };
        if (dir < 0 && lo <= 1) || (dir > 0 && hi + 1 >= n) {
            return;
        }
        self.edit(|order| {
            if dir < 0 {
                for &s in &sel {
                    order.swap(s - 1, s);
                }
            } else {
                for &s in sel.iter().rev() {
                    order.swap(s, s + 1);
                }
            }
        });
    }

    /// Puts an empty slot at `at`, pushing `at` and the samples below it down
    /// as far as the next empty slot, which is used up.
    fn insert_empty(&mut self, at: usize) {
        let Some(p) = &self.project else { return };
        let Some(e) =
            (at + 1..self.order.len()).find(|&i| p.pool[self.order[i] as usize].is_empty())
        else {
            self.status = "No empty slot below to make room".into();
            return;
        };
        self.edit(|order| order[at..=e].rotate_right(1));
        self.status = format!(
            "Inserted empty at {}; shifted down to {}",
            self.label(at),
            self.label(e)
        );
    }

    /// Removes the empty slot at `at`, pulling the samples below it up as far
    /// as the next empty slot.
    fn close_gap(&mut self, at: usize) {
        let Some(p) = &self.project else { return };
        let n = self.order.len();
        let e = (at + 1..n)
            .find(|&i| p.pool[self.order[i] as usize].is_empty())
            .unwrap_or(n);
        self.edit(|order| order[at..e].rotate_left(1));
        self.status = format!("Closed gap at {}", self.label(at));
    }

    fn click(&mut self, slot: usize, m: egui::Modifiers) {
        if m.shift {
            let a = self.cursor.unwrap_or(slot);
            if !m.command {
                self.sel.clear();
            }
            self.sel.extend(a.min(slot)..=a.max(slot));
        } else if m.command {
            if !self.sel.remove(&slot) {
                self.sel.insert(slot);
            }
            self.cursor = Some(slot);
        } else {
            self.sel.clear();
            self.sel.insert(slot);
            self.cursor = Some(slot);
        }
        self.sel.remove(&0);
    }

    fn keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() || self.project.is_none() {
            return;
        }
        let n = self.order.len();
        let (m, up, down, pgup, pgdn, esc, z, a) = ctx.input(|i| {
            (
                i.modifiers,
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::PageUp),
                i.key_pressed(egui::Key::PageDown),
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::Z),
                i.key_pressed(egui::Key::A),
            )
        });
        if m.command && z {
            self.undo();
        }
        if m.command && a {
            let b = self.bank[self.active] * BANK;
            self.sel = (b.max(1)..(b + BANK).min(n)).collect();
        }
        if esc {
            self.sel.clear();
        }
        let step: i32 = if up {
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
            self.nudge(step);
            return;
        }
        let c = self.cursor.unwrap_or(0) as i32;
        let next = (c + step).clamp(1, n as i32 - 1) as usize;
        if m.shift {
            let a = self.cursor.unwrap_or(next);
            self.sel.insert(a);
            self.sel.insert(next);
        } else {
            self.sel.clear();
            self.sel.insert(next);
        }
        self.cursor = Some(next);
        self.reveal = Some(next);
    }
}

/// A drag of the selected slots.
#[derive(Clone, Copy)]
struct SlotDrag;

enum RowAct {
    Click(usize, egui::Modifiers),
    DragStart(usize),
    DropBefore(usize),
    DropOn(usize),
    InsertEmpty(usize),
    CloseGap(usize),
    MoveHere(usize),
    Nudge(i32),
}

pub fn show(ui: &mut egui::Ui, st: &mut ElkState) -> Vec<ElkIntent> {
    let mut out = Vec::new();
    ui.horizontal(|ui| {
        if ui.add(theme::pane_title("Sample Editor", false)).clicked() {
            out.push(ElkIntent::BackToEditor);
        }
        ui.add(theme::pane_title("Elk Sample Order", true));
        ui.separator();
        if let Some(p) = &st.project {
            ui.label(
                RichText::new(format!("{} · {}", p.model.name(), p.versions))
                    .weak()
                    .small(),
            );
        }
    });

    let busy = st.busy.is_some();
    if st.project.is_some() {
        ui.horizontal(|ui| {
        if ui.add_enabled(!busy, egui::Button::new("✕ Close / pick another")).on_hover_text("Close this project and go back to the project list").clicked() {
            if st.changed() > 0 {
                st.confirm_close = true;
            } else {
                st.close_project();
            }
        }
        if ui.add_enabled(!busy, egui::Button::new("⬇ Reload")).on_hover_text("Fetch the project open on the device right now again (from its memory)").clicked() {
            out.push(ElkIntent::Read);
        }
        let changed = st.changed();
        let can_push = !busy && changed > 0 && st.project.as_ref().is_some_and(|p| p.read_only.is_none());
        let push = ui
            .add_enabled(can_push, egui::Button::new(RichText::new(format!("⬆ Push to memory ({changed})")).color(if can_push { theme::ACCENT_BRIGHT } else { Color32::GRAY })))
            .on_hover_text("Write the new order into the project loaded on the device (not the saved copy on +Drive)");
        if push.clicked() {
            st.confirm_push = true;
        }
        if ui.add_enabled(!busy && !st.undo.is_empty(), egui::Button::new("↶ Undo")).on_hover_text("Undo the last move (⌘Z)").clicked() {
            st.undo();
        }
        if ui.add_enabled(!busy && changed > 0, egui::Button::new("↺ Reset")).on_hover_text("Back to the order as read from the device").clicked() {
            st.edit(|o| *o = (0..o.len() as u16).collect());
        }
        if let Some((label, done, total)) = &st.busy {
            ui.add(egui::ProgressBar::new(*done as f32 / (*total).max(1) as f32).desired_width(ui.available_width().max(320.0)).text(format!("{label} {done}/{total}")));
        } else if !st.status.is_empty() && st.project.is_some() {
            ui.add(egui::Label::new(RichText::new(&st.status).weak()).truncate()).on_hover_text(&st.status);
        }
    });
    }

    if st.project.as_ref().is_some_and(|p| p.partial) && busy {
        ui.label(RichText::new("Sample pool loaded; use counts fill in as the patterns arrive. You can start arranging, pushing unlocks when the read finishes.").weak());
    } else if let Some(why) = st.project.as_ref().and_then(|p| p.read_only.clone()) {
        ui.colored_label(Color32::from_rgb(230, 120, 90), format!("Read-only: {why}"))
            .on_hover_text(
                "Pushing is disabled because the dump doesn't match the layouts we know.",
            );
    }

    if st.project.is_none() {
        ui.add_space(12.0);
        ui.vertical_centered(|ui| {
            if let Some((label, done, total)) = &st.busy {
                ui.label(RichText::new(format!("{label}…")).font(oled_font(14.0)).color(OLED));
                ui.add(egui::ProgressBar::new(*done as f32 / (*total).max(1) as f32).desired_width(260.0).text(format!("{done}/{total}")));
            } else {
                let big = egui::Button::new(RichText::new("⬇  Load current project from device memory").size(15.0).color(theme::ACCENT_BRIGHT))
                    .min_size(egui::vec2(320.0, 36.0));
                if ui.add(big).clicked() {
                    out.push(ElkIntent::Read);
                }
                ui.add_space(6.0);
                ui.label(RichText::new("Reads whatever project is open on the device now.\nTo work on another one, load it on the device first (PROJECT › LOAD).").weak());
                if !st.status.is_empty() {
                    ui.add_space(6.0);
                    ui.colored_label(Color32::from_rgb(230, 120, 90), &st.status);
                }
            }
        });
        ui.add_space(12.0);
        projects_list(ui, st, &mut out);
    } else {
        st.keys(ui.ctx());
        ui.label(
            RichText::new("Click / ⇧click / ⌘click to select · drag to place into slots (swaps with what's there; ⌃/⌥ while dropping inserts and shifts instead) · drag onto a bank tab or the other pane · ⌥↑↓ nudge · right-click for more")
                .weak()
                .small(),
        );
        let w = (ui.available_width() - 8.0) / 2.0;
        let h = ui.available_height();
        ui.horizontal_top(|ui| {
            for side in 0..2 {
                ui.allocate_ui_with_layout(
                    egui::vec2(w, h),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_min_size(egui::vec2(w, h));
                        ui.push_id(side, |ui| slot_pane(ui, st, side));
                    },
                );
            }
        });
    }

    if st.confirm_close {
        if let Some(yes) = confirm_close(ui.ctx(), st.changed()) {
            st.confirm_close = false;
            if yes {
                st.close_project();
            }
        }
    }
    if st.confirm_push {
        if let Some(yes) = confirm_push(ui.ctx(), st.changed()) {
            st.confirm_push = false;
            if yes {
                out.push(ElkIntent::Push);
            }
        }
    }
    out
}

fn projects_list(ui: &mut egui::Ui, st: &mut ElkState, out: &mut Vec<ElkIntent>) {
    let n = match &st.projects {
        Some(Ok(v)) => format!(" ({})", v.len()),
        _ => String::new(),
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("Projects on +Drive{n}")).strong());
        if ui.small_button("⟳").on_hover_text("List again").clicked() {
            out.push(ElkIntent::ListProjects);
        }
        ui.label(
            RichText::new(
                "Can't be opened from here: load one on the device, then use the button above.",
            )
            .weak()
            .small(),
        );
    });
    egui::Frame::new()
        .fill(OLED_BG)
        .corner_radius(2.0)
        .inner_margin(6.0)
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            match &st.projects {
                None => {
                    ui.spinner();
                }
                Some(Err(e)) => {
                    ui.colored_label(Color32::from_rgb(230, 120, 90), e);
                }
                Some(Ok(v)) if v.is_empty() => {
                    ui.label(
                        RichText::new("No saved projects")
                            .font(oled_font(12.0))
                            .color(OLED_DIM),
                    );
                }
                Some(Ok(v)) => {
                    egui::ScrollArea::vertical()
                        .id_salt("elk_proj_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for p in v {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(format!("{:03}", p.slot))
                                            .font(oled_font(TEXT))
                                            .color(OLED_DIM),
                                    );
                                    ui.label(
                                        RichText::new(&p.name).font(oled_font(TEXT)).color(OLED),
                                    );
                                    ui.label(
                                        RichText::new(format!("{} KB", p.size / 1024))
                                            .font(oled_font(12.0))
                                            .color(OLED_DIM),
                                    );
                                });
                            }
                        });
                }
            }
        });
}

fn slot_pane(ui: &mut egui::Ui, st: &mut ElkState, side: usize) {
    let Some(p) = &st.project else { return };
    let n = p.pool.len();
    let banks = n.div_ceil(BANK);
    // Keep the keyboard cursor visible in the pane it drives.
    if let Some(r) = st.reveal.filter(|_| side == st.active) {
        st.bank[side] = r / BANK;
    }
    if banks > 1 {
        // Device-style bank strip: "< A B C D E F G H >", current one boxed.
        let (strip, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), ROW_H + 6.0),
            Sense::empty(),
        );
        ui.painter().rect_filled(strip, 0.0, OLED_BG);
        let cell = 26.0;
        let x0 = strip.center().x - cell * (banks as f32 + 2.0) / 2.0;
        let font = oled_font(TEXT);
        let arrow = |ui: &mut egui::Ui, i: f32, txt: &str, to: Option<usize>| {
            let r = egui::Rect::from_min_size(
                egui::pos2(x0 + i * cell, strip.top() + 3.0),
                egui::vec2(cell, ROW_H),
            );
            let resp = ui.interact(r, ui.scope_id().with(("bank_tab", txt)), Sense::click());
            let on = to.is_some();
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                txt,
                font.clone(),
                if on && resp.hovered() { OLED } else { OLED_DIM },
            );
            (resp, to)
        };
        let mut pick = None;
        let cur = st.bank[side];
        let (r, to) = arrow(ui, 0.0, "<", cur.checked_sub(1));
        if r.clicked() {
            pick = to;
        }
        for b in 0..banks {
            let r = egui::Rect::from_min_size(
                egui::pos2(x0 + (b + 1) as f32 * cell, strip.top() + 3.0),
                egui::vec2(cell, ROW_H),
            );
            let resp = ui.interact(r, ui.scope_id().with(("bank", b)), Sense::click());
            let letter = ((b'A' + b as u8) as char).to_string();
            let p = ui.painter();
            if b == cur {
                p.rect_stroke(
                    r.shrink2(egui::vec2(4.0, 0.0)),
                    0.0,
                    Stroke::new(2.0, OLED),
                    egui::StrokeKind::Inside,
                );
            } else if resp.hovered() {
                p.rect_filled(
                    r.shrink2(egui::vec2(4.0, 0.0)),
                    0.0,
                    Color32::from_rgb(30, 36, 36),
                );
            }
            p.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                letter,
                font.clone(),
                OLED,
            );
            // Dragging slots over a bank tab opens that bank.
            if resp.clicked() || resp.dnd_hover_payload::<SlotDrag>().is_some() {
                pick = Some(b);
            }
        }
        let (r, to) = arrow(
            ui,
            banks as f32 + 1.0,
            ">",
            Some(cur + 1).filter(|&b| b < banks),
        );
        if r.clicked() {
            pick = to;
        }
        if let Some(b) = pick {
            st.bank[side] = b;
            st.active = side;
        }
    }
    let bank = st.bank[side].min(banks - 1);
    let first = bank * BANK;
    let rows = BANK.min(n - first);
    let mut acts = Vec::new();
    let mut scroll = egui::ScrollArea::vertical()
        .id_salt(("elk_slots", side, bank))
        .auto_shrink([false, false]);
    if let Some(r) = st.reveal.filter(|_| side == st.active) {
        let y = (r - first) as f32 * ROW_H;
        let (top, h) = (st.scroll[side], st.view_h[side]);
        if y < top {
            scroll = scroll.vertical_scroll_offset(y);
        } else if y + ROW_H > top + h {
            scroll = scroll.vertical_scroll_offset(y + ROW_H - h);
        }
    }
    let out = egui::Frame::new()
        .fill(OLED_BG)
        .corner_radius(2.0)
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            scroll.show_rows(ui, ROW_H, rows, |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for i in range {
                    slot_row(ui, st, first + i, &mut acts);
                }
            })
        });
    st.scroll[side] = out.inner.state.clamped_offset().y;
    st.view_h[side] = out.inner.inner_rect.height();
    if side == st.active {
        st.reveal = None;
    }
    for a in acts {
        match a {
            RowAct::Click(s, m) => {
                st.active = side;
                st.click(s, m);
            }
            RowAct::DragStart(s) => {
                st.active = side;
                if !st.sel.contains(&s) {
                    st.click(s, egui::Modifiers::NONE);
                }
            }
            RowAct::DropOn(s) => {
                st.active = side;
                st.place_sel_at(s);
            }
            RowAct::DropBefore(s) => {
                st.active = side;
                st.move_sel_to(s);
            }
            RowAct::InsertEmpty(s) => st.insert_empty(s),
            RowAct::CloseGap(s) => st.close_gap(s),
            RowAct::MoveHere(s) => st.move_sel_to(s),
            RowAct::Nudge(d) => st.nudge(d),
        }
    }
}

/// One slot row, laid out like the device's file list.
fn slot_row(ui: &mut egui::Ui, st: &ElkState, slot: usize, acts: &mut Vec<RowAct>) {
    let Some(p) = st.project.as_ref() else { return };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::empty());
    let from = st.order[slot] as usize;
    let e = p.pool[from];
    let off = slot == 0;
    let resp = ui.interact(
        rect,
        ui.scope_id().with(("slot", slot)),
        if off {
            Sense::hover()
        } else {
            Sense::click_and_drag()
        },
    );
    let selected = st.sel.contains(&slot);
    let moved = from != slot;

    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, 0.0, OLED);
    } else if resp.hovered() && !off {
        painter.rect_filled(rect, 0.0, Color32::from_rgb(30, 36, 36));
    }
    if st.cursor == Some(slot) && !selected {
        painter.rect_stroke(
            rect.shrink(0.5),
            0.0,
            Stroke::new(1.0, OLED_DIM),
            egui::StrokeKind::Inside,
        );
    }
    if moved {
        painter.rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
            0.0,
            theme::ACCENT_BRIGHT,
        );
    }
    let (fg, dim) = if selected {
        (OLED_BG, Color32::from_rgb(60, 70, 66))
    } else {
        (OLED, OLED_DIM)
    };
    let font = oled_font(TEXT);
    let y = rect.center().y;
    let label_color = if moved && !selected {
        theme::ACCENT_BRIGHT
    } else {
        dim
    };
    let label_w = 46.0;
    painter.text(
        egui::pos2(rect.left() + 8.0, y),
        egui::Align2::LEFT_CENTER,
        format!("{:03}", slot % BANK),
        font.clone(),
        label_color,
    );
    let name = if off {
        "OFF".to_owned()
    } else if e.is_empty() {
        "-".to_owned()
    } else {
        match st.names.get(&e.key()) {
            Some(path) => path.rsplit('/').next().unwrap_or(path).to_owned(),
            None => format!("#{:08x}", e.hash),
        }
    };
    let uses = p.uses[from];
    let size = if e.is_empty() || off {
        String::new()
    } else {
        format!("{:.2}MB", e.size.saturating_sub(16) as f64 / 1_048_576.0)
    };
    let size_w = 84.0;
    let uses_w = 44.0;
    let name_x = rect.left() + 8.0 + label_w;
    let clip = egui::Rect::from_min_max(
        egui::pos2(name_x, rect.top()),
        egui::pos2(rect.right() - size_w - uses_w, rect.bottom()),
    );
    painter.with_clip_rect(clip).text(
        egui::pos2(name_x, y),
        egui::Align2::LEFT_CENTER,
        name,
        font.clone(),
        if e.is_empty() || off { dim } else { fg },
    );
    if uses > 0 {
        painter.text(
            egui::pos2(rect.right() - size_w - 6.0, y),
            egui::Align2::RIGHT_CENTER,
            format!("x{uses}"),
            font.clone(),
            dim,
        );
    }
    painter.text(
        egui::pos2(rect.right() - 8.0, y),
        egui::Align2::RIGHT_CENTER,
        size,
        font,
        fg,
    );

    if off {
        return;
    }
    if resp.clicked() {
        acts.push(RowAct::Click(slot, ui.input(|i| i.modifiers)));
    }
    if resp.drag_started() {
        acts.push(RowAct::DragStart(slot));
        resp.dnd_set_drag_payload(SlotDrag);
    }
    // Drop: by default the selection lands in this slot and the ones after it,
    // swapping with what's there. With ⌃ or ⌥ held it's inserted instead,
    // before this row (top half) or after it (bottom half), shifting the rest.
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let shift_mode = ui.input(|i| i.modifiers.ctrl || i.modifiers.alt);
    let before = |pos: Option<egui::Pos2>| {
        if pos.is_some_and(|p| p.y > rect.center().y) {
            slot + 1
        } else {
            slot
        }
    };
    if resp.dnd_hover_payload::<SlotDrag>().is_some() {
        if shift_mode {
            let at = before(pointer);
            let ly = if at == slot {
                rect.top()
            } else {
                rect.bottom()
            };
            ui.painter()
                .hline(rect.x_range(), ly, Stroke::new(2.0, theme::ACCENT_BRIGHT));
        } else {
            let n = st.sel.len().max(1).min(st.order.len() - slot);
            let r = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), ROW_H * n as f32));
            ui.painter().rect_stroke(
                r,
                0.0,
                Stroke::new(2.0, theme::ACCENT_BRIGHT),
                egui::StrokeKind::Inside,
            );
        }
    }
    if resp.dnd_release_payload::<SlotDrag>().is_some() {
        acts.push(if shift_mode {
            RowAct::DropBefore(before(pointer))
        } else {
            RowAct::DropOn(slot)
        });
    }

    let n_sel = st.sel.len();
    resp.context_menu(|ui| {
        if ui
            .button("Insert empty slot here")
            .on_hover_text(
                "Shift this and the samples below down by one, up to the next empty slot",
            )
            .clicked()
        {
            acts.push(RowAct::InsertEmpty(slot));
            ui.close();
        }
        if e.is_empty()
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
    });
    if !e.is_empty() {
        let mut tip = st
            .names
            .get(&e.key())
            .cloned()
            .unwrap_or_else(|| "(not found on +Drive)".into());
        tip.push_str(&format!("\npool slot {slot}"));
        if moved {
            tip.push_str(&format!(", was {}", st.label(from)));
        }
        tip.push_str(&format!(
            "\nused {uses}× (kit tracks, sample locks, sounds)"
        ));
        resp.on_hover_text(tip);
    }
}

fn confirm_push(ctx: &egui::Context, changed: usize) -> Option<bool> {
    let mut answer = None;
    egui::Modal::new(egui::Id::unique("elk_push_modal")).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.heading(format!("Push new sample order ({changed} slots)?"));
        ui.add_space(6.0);
        ui.label(
            "This rewrites the project currently loaded on the device: only the patterns, kits and pool sounds \
             that reference moved slots are sent, plus the sample pool. Everything else on the device is left as is. \
             The saved copy on +Drive is not touched.",
        );
        ui.add_space(4.0);
        ui.label(RichText::new("Wait for the device's own progress bar to finish. If it sounds right, save via PROJECT › SAVE. If anything is off, don't save: reload the project from +Drive.").weak());
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(RichText::new("Push to device").color(theme::ACCENT_BRIGHT)).clicked() {
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
    egui::Modal::new(egui::Id::unique("elk_close_modal")).show(ctx, |ui| {
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
