//! The Elk Project Editor (experimental, Digitakt 1 only): the loaded
//! project's 128 patterns as an 8 × 16 grid. Move, copy, clear and rename
//! whole patterns; the result goes to the device with the same push as the
//! sample order, so both pages edit one loaded project.

use crate::device::patterns::{self, Patterns, NAME_MAX, PATTERNS};
use crate::device::{Cell, ElkProject, PatState};
use crate::ui::elk_view::{self, ElkIntent, ElkState};
use crate::ui::theme::{self, oled_font, OLED, OLED_BG, OLED_DIM, OLED_HOVER};
use egui::{Color32, RichText, Sense, Stroke};
use std::collections::BTreeSet;

const BANKS: usize = 8;
const PER_BANK: usize = 16;
const CELL_H: f32 = 46.0;
const INSPECTOR_W: f32 = 300.0;
const WARN: Color32 = Color32::from_rgb(230, 120, 90);
const SEL_FILL: Color32 = Color32::from_rgb(70, 38, 14);

#[derive(Default)]
pub struct ProjEd {
    /// `cells[slot]`: what goes into each slot. Empty while there are no
    /// patterns to edit.
    pub cells: Vec<Cell>,
    undo: Vec<Vec<Cell>>,
    sel: BTreeSet<usize>,
    cursor: Option<usize>,
    clip: Vec<Cell>,
    /// Cell a drag started on.
    drag: Option<usize>,
    /// Name typed for the selected slot(s), and the selection it was filled for.
    name_edit: String,
    name_for: Vec<usize>,
    focus_name: bool,
}

impl ProjEd {
    pub fn reset(&mut self, p: &ElkProject) {
        *self = Self::default();
        if p.patterns.is_ok() {
            self.cells = (0..PATTERNS).map(Cell::as_read).collect();
        }
    }

    /// Pattern slots that differ from the read.
    pub fn changed(&self) -> usize {
        self.cells.iter().enumerate().filter(|(i, c)| **c != Cell::as_read(*i)).count()
    }

    fn edit(&mut self, f: impl FnOnce(&mut Vec<Cell>)) {
        let before = self.cells.clone();
        f(&mut self.cells);
        if self.cells != before {
            self.undo.push(before);
        }
    }

    fn undo(&mut self) {
        if let Some(c) = self.undo.pop() {
            self.cells = c;
        }
    }

    /// The selection, else the cursor.
    fn targets(&self) -> Vec<usize> {
        if self.sel.is_empty() {
            self.cursor.into_iter().collect()
        } else {
            self.sel.iter().copied().collect()
        }
    }

    fn select_only(&mut self, i: usize) {
        self.sel.clear();
        self.sel.insert(i);
        self.cursor = Some(i);
    }
}

/// What a slot shows: its state and name, with edits applied.
fn view(p: &Patterns, c: &Cell) -> (PatState, String) {
    match c.src {
        Some(s) => {
            let info = &p.info[s as usize];
            (info.state, c.name.clone().unwrap_or_else(|| info.kit_name.clone()))
        }
        None => (PatState::Blank, c.name.clone().unwrap_or_default()),
    }
}

/// Move (swap) or copy the selection so the grabbed cell `from` lands on `to`.
fn drop_sel(ed: &mut ProjEd, from: usize, to: usize, copy: bool) -> Result<String, String> {
    let src: Vec<usize> = ed.sel.iter().copied().collect();
    let off = to as isize - from as isize;
    if off == 0 {
        return Ok(String::new());
    }
    let dst: Vec<usize> = src.iter().map(|&s| s as isize + off).filter(|d| (0..PATTERNS as isize).contains(d)).map(|d| d as usize).collect();
    if dst.len() != src.len() {
        return Err("Doesn't fit: part of the selection would land past H16".into());
    }
    ed.edit(|cells| {
        let old = cells.clone();
        for (s, d) in src.iter().zip(&dst) {
            cells[*d] = old[*s].clone();
        }
        if !copy {
            // What was under the destination fills the slots left behind.
            let sset: BTreeSet<usize> = src.iter().copied().collect();
            let dset: BTreeSet<usize> = dst.iter().copied().collect();
            for (v, d) in sset.difference(&dset).zip(dset.difference(&sset)) {
                cells[*v] = old[*d].clone();
            }
        }
    });
    ed.sel = dst.iter().copied().collect();
    ed.cursor = Some(to);
    let n = src.len();
    Ok(if copy { format!("Copied {n} pattern(s) to {}", patterns::label(dst[0])) } else { format!("Moved {n} pattern(s) to {}", patterns::label(dst[0])) })
}

fn clear(ed: &mut ProjEd, p: &Patterns) -> String {
    let t = ed.targets();
    if !p.can_clear() {
        return "Can't clear: no empty pattern found in this project to copy (see the log)".into();
    }
    let used = t.iter().filter(|&&i| view(p, &ed.cells[i]).0 != PatState::Blank).count();
    ed.edit(|cells| {
        for &i in &t {
            cells[i] = Cell { src: None, name: None };
        }
    });
    format!("Cleared {} slot(s), {used} of them had something in them", t.len())
}

fn copy(ed: &mut ProjEd) -> String {
    let t = ed.targets();
    ed.clip = t.iter().map(|&i| ed.cells[i].clone()).collect();
    format!("Copied {} pattern(s); select a slot and paste (⌘V)", ed.clip.len())
}

fn paste(ed: &mut ProjEd) -> String {
    let Some(at) = ed.sel.first().copied().or(ed.cursor) else {
        return "Select where to paste first".into();
    };
    if ed.clip.is_empty() {
        return "Nothing copied yet".into();
    }
    let clip = ed.clip.clone();
    let n = clip.len().min(PATTERNS - at);
    ed.edit(|cells| {
        for (k, c) in clip.into_iter().take(n).enumerate() {
            cells[at + k] = c;
        }
    });
    ed.sel = (at..at + n).collect();
    format!("Pasted {n} pattern(s) at {}", patterns::label(at))
}

/// Pull everything that isn't empty to the front of each bank the
/// selection touches, keeping the order.
fn pack(ed: &mut ProjEd, p: &Patterns) -> String {
    let banks: BTreeSet<usize> = ed.targets().iter().map(|i| i / PER_BANK).collect();
    if banks.is_empty() {
        return "Select a slot in the bank(s) to pack".into();
    }
    ed.edit(|cells| {
        for &b in &banks {
            let r = b * PER_BANK..(b + 1) * PER_BANK;
            let (full, empty): (Vec<Cell>, Vec<Cell>) = cells[r.clone()].iter().cloned().partition(|c| view(p, c).0 != PatState::Blank || c.name.is_some());
            for (k, c) in full.into_iter().chain(empty).enumerate() {
                cells[r.start + k] = c;
            }
        }
    });
    let names: Vec<String> = banks.iter().map(|b| ((b'A' + *b as u8) as char).to_string()).collect();
    format!("Packed bank {}", names.join(", "))
}

/// Apply a name (single slot) or a template (several): `{n}` counts from 1,
/// `{slot}` is A01…, `{name}` the current name.
fn rename(ed: &mut ProjEd, p: &Patterns, text: &str) -> String {
    let t = ed.targets();
    if t.is_empty() {
        return String::new();
    }
    let template = t.len() > 1 && text.contains('{');
    let mut empty = 0;
    ed.edit(|cells| {
        for (k, &i) in t.iter().enumerate() {
            let cur = view(p, &cells[i]).1;
            let raw = if template || t.len() > 1 {
                text.replace("{n}", &(k + 1).to_string()).replace("{slot}", &patterns::label(i)).replace("{name}", &cur)
            } else {
                text.to_owned()
            };
            let name = patterns::clean_name(&raw);
            let read = cells[i].src.map(|s| p.info[s as usize].kit_name.clone()).unwrap_or_default();
            if cells[i].src.is_none() {
                empty += 1;
            }
            cells[i].name = (name != read).then_some(name);
        }
    });
    let mut s = format!("Renamed {} slot(s)", t.len());
    if empty > 0 {
        s.push_str(&format!(" ({empty} empty: the name goes on an empty pattern)"));
    }
    s
}

pub fn show(ui: &mut egui::Ui, st: &mut ElkState) -> Vec<ElkIntent> {
    let mut out = Vec::new();
    ui.horizontal(|ui| elk_view::mode_header(ui, st, &mut out));
    if st.project.is_none() {
        elk_view::load_screen(ui, st, &mut out);
        elk_view::finish(ui, st, &mut out);
        return out;
    }
    let busy = st.busy.is_some();
    ui.horizontal_wrapped(|ui| {
        elk_view::session_tools(ui, st, &mut out, true);
        let ed = &mut st.proj;
        if ui.add_enabled(!busy && !ed.undo.is_empty(), egui::Button::new("↶ Undo")).on_hover_text("Undo the last pattern edit (⌘Z)").clicked() {
            ed.undo();
        }
        let changed = ed.changed();
        if ui.add_enabled(!busy && changed > 0, egui::Button::new("↺ Reset")).on_hover_text("Back to the patterns as read from the device").clicked() {
            ed.edit(|c| *c = (0..PATTERNS).map(Cell::as_read).collect());
        }
        elk_view::status_line(ui, st, true);
    });

    let msg = match st.project.as_ref().map(|p| &p.patterns) {
        Some(Err(e)) if st.project.as_ref().is_some_and(|p| p.partial) => Some(format!("Patterns: {e}…")),
        Some(Err(e)) => Some(format!("Pattern editing unavailable: {e}")),
        _ => None,
    };
    if let Some(m) = msg {
        ui.add_space(8.0);
        ui.colored_label(WARN, m);
        elk_view::finish(ui, st, &mut out);
        return out;
    }
    if let Some(why) = st.project.as_ref().and_then(|p| p.patterns.as_ref().ok()).and_then(|p| p.read_only.clone()) {
        ui.colored_label(WARN, format!("Read-only: {why}. Details in the log (lines starting elk-pat:)."));
    }
    keys(ui.ctx(), st);
    ui.label(
        RichText::new("Experimental · Click / ⇧click / ⌘click to select · drag to move (swaps with what's there) · ⌥ while dropping copies · ⌘C / ⌘V · ⌫ clears · ⏎ renames · arrows move")
            .weak()
            .small(),
    );

    let avail = ui.available_size();
    let grid_w = (avail.x - INSPECTOR_W - 12.0).max(400.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(grid_w, avail.y), egui::Layout::top_down(egui::Align::Min), |ui| {
            grid(ui, st, busy);
        });
        ui.separator();
        ui.allocate_ui_with_layout(egui::vec2(INSPECTOR_W, avail.y), egui::Layout::top_down(egui::Align::Min), |ui| {
            egui::ScrollArea::vertical().id_salt("proj_inspector").auto_shrink([false, false]).show(ui, |ui| inspector(ui, st, busy));
        });
    });
    elk_view::finish(ui, st, &mut out);
    out
}

fn keys(ctx: &egui::Context, st: &mut ElkState) {
    if ctx.egui_wants_keyboard_input() || st.busy.is_some() {
        return;
    }
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
    if m.command && z {
        ed.undo();
    }
    if m.command && a {
        ed.sel = (0..PATTERNS).collect();
    }
    if esc {
        ed.sel.clear();
    }
    if del && !ed.targets().is_empty() {
        status = Some(clear(ed, p));
    }
    if ev_copy || (m.command && c) {
        status = Some(copy(ed));
    }
    if ev_paste || (m.command && v) {
        status = Some(paste(ed));
    }
    if enter && !ed.targets().is_empty() {
        ed.focus_name = true;
    }
    let step: isize = if l { -1 } else if r { 1 } else if u { -(PER_BANK as isize) } else if d { PER_BANK as isize } else { 0 };
    if step != 0 && !m.command {
        let cur = ed.cursor.unwrap_or(0) as isize;
        let next = (cur + step).clamp(0, PATTERNS as isize - 1) as usize;
        if m.shift {
            let anchor = ed.cursor.unwrap_or(next);
            ed.sel.insert(anchor);
            ed.sel.insert(next);
            ed.cursor = Some(next);
        } else {
            ed.select_only(next);
        }
    }
    if let Some(s) = status {
        st.status = s;
    }
}

fn grid(ui: &mut egui::Ui, st: &mut ElkState, busy: bool) {
    let Some(p) = st.project.as_ref().and_then(|p| p.patterns.as_deref().ok()) else { return };
    let editable = p.read_only.is_none() && !busy;
    let ed = &mut st.proj;
    let label_w = 22.0;
    let w = ui.available_width();
    let cell_w = ((w - label_w) / PER_BANK as f32).floor();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 16.0 + BANKS as f32 * CELL_H), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, OLED_BG);
    for c in 0..PER_BANK {
        let x = rect.left() + label_w + (c as f32 + 0.5) * cell_w;
        painter.text(egui::pos2(x, rect.top() + 8.0), egui::Align2::CENTER_CENTER, format!("{}", c + 1), oled_font(11.0), OLED_DIM);
    }
    let cell_rect = |i: usize| {
        let (b, c) = (i / PER_BANK, i % PER_BANK);
        egui::Rect::from_min_size(
            egui::pos2(rect.left() + label_w + c as f32 * cell_w, rect.top() + 16.0 + b as f32 * CELL_H),
            egui::vec2(cell_w, CELL_H),
        )
        .shrink(1.5)
    };
    for b in 0..BANKS {
        let y = rect.top() + 16.0 + (b as f32 + 0.5) * CELL_H;
        painter.text(egui::pos2(rect.left() + label_w / 2.0, y), egui::Align2::CENTER_CENTER, ((b'A' + b as u8) as char).to_string(), oled_font(14.0), OLED_DIM);
    }

    let pointer = ui.input(|i| i.pointer.interact_pos());
    let under = pointer.and_then(|pt| (0..PATTERNS).find(|&i| cell_rect(i).contains(pt)));
    let mods = ui.input(|i| i.modifiers);
    // Where a drag would put the selection.
    let landing: BTreeSet<usize> = match (ed.drag, under) {
        (Some(from), Some(to)) if from != to => {
            let off = to as isize - from as isize;
            ed.sel.iter().map(|&s| s as isize + off).filter(|d| (0..PATTERNS as isize).contains(d)).map(|d| d as usize).collect()
        }
        _ => BTreeSet::new(),
    };
    let mut status = None;
    let mut src_count = [0usize; PATTERNS];
    for c in &ed.cells {
        if let Some(s) = c.src {
            src_count[s as usize] += 1;
        }
    }

    for i in 0..PATTERNS {
        let r = cell_rect(i);
        let resp = ui.interact(r, ui.scope_id().with(("pcell", i)), if editable { Sense::click_and_drag() } else { Sense::click() });
        let cell = &ed.cells[i];
        let (state, name) = view(p, cell);
        let selected = ed.sel.contains(&i);
        let fill = if selected {
            SEL_FILL
        } else if resp.hovered() {
            OLED_HOVER
        } else {
            Color32::from_rgb(18, 21, 21)
        };
        painter.rect_filled(r, 2.0, fill);
        let changed = *cell != Cell::as_read(i);
        let border = if landing.contains(&i) {
            Stroke::new(2.0, theme::ACCENT_BRIGHT)
        } else if ed.cursor == Some(i) {
            Stroke::new(1.0, OLED_DIM)
        } else if changed {
            Stroke::new(1.0, theme::ACCENT)
        } else {
            Stroke::new(1.0, Color32::from_rgb(34, 40, 40))
        };
        painter.rect_stroke(r, 2.0, border, egui::StrokeKind::Inside);
        let p2 = painter.with_clip_rect(r);
        p2.text(r.left_top() + egui::vec2(4.0, 3.0), egui::Align2::LEFT_TOP, patterns::label(i), oled_font(10.0), OLED_DIM);
        // Where it came from, when it's not what was read here.
        let badge = match cell.src {
            None if changed => Some("∅".to_owned()),
            Some(s) if s as usize != i => Some(format!("{}{}", if src_count[s as usize] > 1 { "⧉" } else { "←" }, patterns::label(s as usize))),
            _ => None,
        };
        if let Some(b) = badge {
            p2.text(r.right_top() + egui::vec2(-4.0, 3.0), egui::Align2::RIGHT_TOP, b, oled_font(10.0), theme::ACCENT_BRIGHT);
        }
        let (text, color) = match state {
            PatState::Used if name.is_empty() => ("(no name)".to_owned(), OLED),
            PatState::Used => (name.clone(), OLED),
            PatState::Phantom => (if name.is_empty() { "≈".to_owned() } else { name.clone() }, OLED_DIM),
            PatState::Blank => (if name.is_empty() { "—".to_owned() } else { name.clone() }, Color32::from_rgb(70, 80, 78)),
        };
        let italic = state == PatState::Used && name.is_empty();
        p2.text(
            r.left_bottom() + egui::vec2(4.0, -5.0),
            egui::Align2::LEFT_BOTTOM,
            text,
            oled_font(if italic { 11.0 } else { 13.0 }),
            if italic { OLED_DIM } else { color },
        );
        if cell.name.is_some() {
            p2.text(r.right_bottom() + egui::vec2(-3.0, -4.0), egui::Align2::RIGHT_BOTTOM, "✎", oled_font(10.0), theme::ACCENT_BRIGHT);
        }

        if resp.drag_started() {
            if !ed.sel.contains(&i) {
                ed.select_only(i);
            }
            ed.drag = Some(i);
        }
        if resp.double_clicked() {
            ed.select_only(i);
            ed.focus_name = true;
        } else if resp.clicked() {
            if mods.shift {
                let a = ed.cursor.unwrap_or(i);
                let (lo, hi) = (a.min(i), a.max(i));
                ed.sel.extend(lo..=hi);
                ed.cursor = Some(i);
            } else if mods.command {
                if !ed.sel.remove(&i) {
                    ed.sel.insert(i);
                }
                ed.cursor = Some(i);
            } else {
                ed.select_only(i);
            }
        }
        resp.context_menu(|ui| {
            if !ed.sel.contains(&i) {
                ed.select_only(i);
            }
            if ui.button("Rename…  ⏎").clicked() {
                ed.focus_name = true;
                ui.close();
            }
            if ui.button("Copy  ⌘C").clicked() {
                status = Some(copy(ed));
                ui.close();
            }
            if ui.add_enabled(editable && !ed.clip.is_empty(), egui::Button::new("Paste here  ⌘V")).clicked() {
                status = Some(paste(ed));
                ui.close();
            }
            if ui.add_enabled(editable && p.can_clear(), egui::Button::new("Clear  ⌫")).clicked() {
                status = Some(clear(ed, p));
                ui.close();
            }
            if ui.add_enabled(editable, egui::Button::new("Pack bank (used patterns first)")).clicked() {
                status = Some(pack(ed, p));
                ui.close();
            }
        });
    }

    if let Some(from) = ed.drag {
        if ui.input(|i| i.pointer.any_released()) {
            ed.drag = None;
            if let Some(to) = under.filter(|&t| t != from) {
                status = Some(drop_sel(ed, from, to, mods.alt).unwrap_or_else(|e| e));
            }
        } else if let Some(pt) = pointer {
            let n = ed.sel.len();
            let tip = format!("{} {n}", if mods.alt { "⧉ copy" } else { "move" });
            ui.painter().text(pt + egui::vec2(14.0, 8.0), egui::Align2::LEFT_TOP, tip, oled_font(12.0), theme::ACCENT_BRIGHT);
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
    }
    if let Some(s) = status {
        st.status = s;
    }
}

fn inspector(ui: &mut egui::Ui, st: &mut ElkState, busy: bool) {
    let Some(proj) = st.project.as_ref() else { return };
    let Ok(p) = proj.patterns.as_deref() else { return };
    let editable = p.read_only.is_none() && !busy;
    let ed = &mut st.proj;
    let t = ed.targets();
    // Refill the name field when the selection changes.
    if ed.name_for != t {
        ed.name_for = t.clone();
        ed.name_edit = match t.as_slice() {
            [i] => view(p, &ed.cells[*i]).1,
            [] => String::new(),
            _ => "{name}".into(),
        };
    }
    let mut status = None;

    if t.is_empty() {
        ui.label(RichText::new("Select a pattern slot").strong());
        ui.add_space(4.0);
        let used = p.info.iter().filter(|i| i.state == PatState::Used).count();
        let ph = p.info.iter().filter(|i| i.state == PatState::Phantom).count();
        ui.label(format!("{used} used, {ph} nearly empty, {} empty", PATTERNS - used - ph));
        ui.add_space(8.0);
        ui.label(RichText::new("Each Digitakt pattern holds its own copy of its kit; the name shown is the kit name. Nothing points at a pattern number, so moving a pattern keeps its sounds and samples.").weak().small());
        ui.add_space(4.0);
        ui.label(RichText::new("←D02 moved from D02 · ⧉D02 copy of D02 · ∅ cleared · ✎ renamed · ≈ nearly empty (kit tweaks, no other content)").weak().small());
        return;
    }

    if let [i] = t.as_slice() {
        let i = *i;
        let c = ed.cells[i].clone();
        let (state, name) = view(p, &c);
        ui.label(RichText::new(format!("{}  {}", patterns::label(i), if name.is_empty() { "(no name)" } else { &name })).font(oled_font(16.0)).color(OLED));
        match c.src {
            Some(s) if s as usize != i => ui.label(RichText::new(format!("from {} as read", patterns::label(s as usize))).color(theme::ACCENT_BRIGHT)),
            None => ui.label(RichText::new("cleared (empty pattern)").color(theme::ACCENT_BRIGHT)),
            _ => ui.label(RichText::new("as read").weak()),
        };
        let info = c.src.map(|s| &p.info[s as usize]);
        ui.label(match (state, info) {
            (PatState::Used, Some(n)) => format!("used: {} bytes differ from an empty pattern", n.diff),
            (PatState::Phantom, Some(n)) => format!("nearly empty: {} bytes differ from an empty pattern", n.diff),
            _ => "empty".to_owned(),
        });
        ui.add_space(6.0);
        name_field(ui, ed, p, editable, "Kit name", &mut status);
        if let Some(n) = info {
            ui.add_space(8.0);
            ui.label(RichText::new("Tracks").strong());
            for (k, tr) in n.tracks.iter().enumerate() {
                let sample = if tr.slot == 0 {
                    "—".to_owned()
                } else {
                    let file = proj
                        .pool
                        .get(tr.slot)
                        .filter(|e| !e.is_empty())
                        .and_then(|e| st.names.get(&e.key()))
                        .map(|n| n.rsplit('/').next().unwrap_or(n).to_owned())
                        .unwrap_or_else(|| "?".into());
                    format!("{:03} {file}", tr.slot)
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{}", k + 1)).font(oled_font(12.0)).color(OLED_DIM));
                    ui.label(RichText::new(&tr.name).font(oled_font(12.0)).color(OLED));
                    ui.label(RichText::new(sample).small().weak());
                });
            }
            ui.label(RichText::new("Sample slots as read (before any Sample Order changes)").weak().small());
            if !n.pattern_field.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new(format!("elk-herd's pattern-name field holds \"{}\" (not used, not written)", n.pattern_field)).weak().small());
            }
        }
    } else {
        ui.label(RichText::new(format!("{} slots selected", t.len())).strong());
        ui.add_space(6.0);
        name_field(ui, ed, p, editable, "Rename all", &mut status);
        ui.label(RichText::new("{n} = 1, 2, 3…   {slot} = A01…   {name} = current name").weak().small());
        let preview: Vec<String> = t
            .iter()
            .take(3)
            .enumerate()
            .map(|(k, &i)| patterns::clean_name(&ed.name_edit.replace("{n}", &(k + 1).to_string()).replace("{slot}", &patterns::label(i)).replace("{name}", &view(p, &ed.cells[i]).1)))
            .collect();
        ui.label(RichText::new(format!("→ {}{}", preview.join(", "), if t.len() > 3 { ", …" } else { "" })).weak().small());
    }

    ui.add_space(10.0);
    ui.horizontal_wrapped(|ui| {
        if ui.button("Copy").clicked() {
            status = Some(copy(ed));
        }
        if ui.add_enabled(editable && !ed.clip.is_empty(), egui::Button::new(format!("Paste ({})", ed.clip.len()))).clicked() {
            status = Some(paste(ed));
        }
        if ui.add_enabled(editable && p.can_clear(), egui::Button::new("Clear")).clicked() {
            status = Some(clear(ed, p));
        }
        if ui.add_enabled(editable, egui::Button::new("Pack bank")).on_hover_text("Move the used patterns of the selected bank(s) to the front, in order").clicked() {
            status = Some(pack(ed, p));
        }
    });
    if let Some(s) = status {
        st.status = s;
    }
}

fn name_field(ui: &mut egui::Ui, ed: &mut ProjEd, p: &Patterns, editable: bool, label: &str, status: &mut Option<String>) {
    let multi = ed.targets().len() > 1;
    ui.horizontal(|ui| {
        ui.label(label);
        let te = egui::TextEdit::singleline(&mut ed.name_edit)
            .char_limit(if multi { 64 } else { NAME_MAX })
            .font(oled_font(14.0))
            .desired_width(150.0);
        let r = ui.add_enabled(editable, te);
        if ed.focus_name {
            ed.focus_name = false;
            r.request_focus();
        }
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui.add_enabled(editable, egui::Button::new("Apply")).clicked() || (enter && editable) {
            let text = ed.name_edit.clone();
            *status = Some(rename(ed, p, &text));
            ed.name_for.clear();
        }
    });
    ui.label(RichText::new(format!("Upper case, up to {NAME_MAX} characters [unverified: the device's own limit]")).weak().small());
}
