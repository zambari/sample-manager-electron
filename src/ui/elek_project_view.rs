//! The Elek Project Editor (experimental; DT1 and DT2, DT2 less tested): the loaded
//! project's 128 patterns as one long list, filtered by bank. Each row folds
//! out into a dot view of its tracks' steps. Move, copy, clear and rename
//! whole patterns; the result goes to the device with the same push as the
//! sample order, so both pages edit one loaded project.

use crate::device::patterns::{self, Patterns, Step, TrackSeq, NAME_MAX, PATTERNS};
use crate::device::{Cell, ElekProject, PatState};
use crate::ui::elek_view::{self, ElekIntent, ElekState};
use crate::ui::theme::{self, oled_font, OLED, OLED_BG, OLED_DIM, OLED_HOVER};
use egui::{Color32, Rect, RichText, Sense, Stroke};
use std::collections::BTreeSet;

const BANKS: usize = 8;
const PER_BANK: usize = 16;
const ROW_H: f32 = 26.0;
const TRACK_H: f32 = 15.0;
const BANK_H: f32 = 20.0;
/// Where the name column starts, and its width.
const NAME_X: f32 = 84.0;
const NAME_W: f32 = 170.0;
/// Where the step dots start, in folded-out rows.
const DOTS_X: f32 = 370.0;
const WARN: Color32 = Color32::from_rgb(230, 120, 90);
const SEL_FILL: Color32 = Color32::from_rgb(70, 38, 14);
const ROW_FILL: Color32 = Color32::from_rgb(18, 21, 21);
const DOT_OFF: Color32 = Color32::from_rgb(38, 45, 44);
const DOT_BEAT: Color32 = Color32::from_rgb(62, 72, 70);

#[derive(Default)]
pub struct ProjEd {
    /// `cells[slot]`: what goes into each slot. Empty while there are no
    /// patterns to edit.
    pub cells: Vec<Cell>,
    undo: Vec<Vec<Cell>>,
    sel: BTreeSet<usize>,
    cursor: Option<usize>,
    clip: Vec<Cell>,
    /// Row a drag started on.
    drag: Option<usize>,
    /// Name typed for several selected slots, and the selection it was filled for.
    name_edit: String,
    name_for: Vec<usize>,
    focus_name: bool,
    /// Row being renamed in place, its text, and whether to focus it.
    inline: Option<usize>,
    inline_text: String,
    inline_focus: bool,
    /// Folded-out banks (the app remembers them).
    pub banks: [bool; BANKS],
    /// Hide empty slots.
    used_only: bool,
    /// Folded-out rows show raw track / pattern bytes instead of steps.
    bytes: bool,
    /// Folded-out rows.
    open: BTreeSet<usize>,
    /// Each row's rect last frame, and the list's visible area, to find the
    /// row under a drag.
    rows: Vec<(usize, Rect)>,
    list_rect: Option<Rect>,
    scroll_to: Option<usize>,
    /// Asked for from the list, for the app (file dialogs).
    want: Vec<ElekIntent>,
    /// Banks whose patterns have all arrived: the only ones edits may touch.
    ready: [bool; BANKS],
    /// Why the last edit was turned down, for the status line.
    refused: Option<String>,
}

impl ProjEd {
    pub fn reset(&mut self, p: &ElekProject) {
        let (used_only, banks, bytes) = (self.used_only, self.banks, self.bytes);
        *self = Self { used_only, banks, bytes, ..Self::default() };
        self.refresh(p);
    }

    /// The project's patterns changed under the same slots (another one
    /// arrived, or the read finished): keep the edits, update what's loaded.
    pub fn refresh(&mut self, p: &ElekProject) {
        match p.patterns.as_deref() {
            Ok(pats) => {
                if self.cells.len() != PATTERNS {
                    self.cells = (0..PATTERNS).map(Cell::as_read).collect();
                }
                for (b, r) in self.ready.iter_mut().enumerate() {
                    *r = pats.bank_loaded(b);
                }
            }
            Err(_) => {
                self.cells.clear();
                self.undo.clear();
                self.sel.clear();
                self.cursor = None;
                self.ready = [false; BANKS];
            }
        }
    }

    /// Folded-out banks as bits (A = bit 0), for the read to take first.
    pub fn bank_mask(&self) -> u8 {
        self.banks.iter().enumerate().filter(|(_, o)| **o).fold(0, |m, (b, _)| m | 1 << b)
    }

    /// Put pattern `src` (one loaded from a file) into `slot`, as an
    /// undoable edit.
    pub fn place_import(&mut self, slot: usize, src: u8) {
        if slot < self.cells.len() {
            self.edit(|c| c[slot] = Cell { src: Some(src), name: None });
            self.select_only(slot);
            self.scroll_to = Some(slot);
        }
    }

    /// Pattern slots that differ from the read.
    pub fn changed(&self) -> usize {
        self.cells.iter().enumerate().filter(|(i, c)| **c != Cell::as_read(*i)).count()
    }

    /// Runs an edit, keeping it out of banks that haven't fully loaded (as
    /// the changed slot, or as where a pattern comes from).
    fn edit(&mut self, f: impl FnOnce(&mut Vec<Cell>)) {
        let before = self.cells.clone();
        f(&mut self.cells);
        if self.cells == before {
            return;
        }
        let blocked: BTreeSet<usize> = (0..self.cells.len())
            .filter(|&i| self.cells[i] != before[i])
            .flat_map(|i| [Some(i), self.cells[i].src.map(usize::from).filter(|&s| s < PATTERNS)])
            .flatten()
            .map(|i| i / PER_BANK)
            .filter(|&b| !self.ready[b])
            .collect();
        if !blocked.is_empty() {
            self.cells = before;
            let names: Vec<String> = blocked.iter().map(|&b| bank_letter(b).to_string()).collect();
            self.refused = Some(format!(
                "Bank {} hasn't fully loaded: only banks that finished loading can be edited",
                names.join(", ")
            ));
            return;
        }
        self.undo.push(before);
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

    fn start_inline(&mut self, p: &Patterns, i: usize) {
        self.select_only(i);
        self.inline = Some(i);
        self.inline_text = view(p, &self.cells[i]).1;
        self.inline_focus = true;
    }

    /// Rows shown, in slot order.
    fn visible(&self, p: &Patterns) -> Vec<usize> {
        (0..self.cells.len())
            .filter(|&i| self.banks[i / PER_BANK])
            .filter(|&i| {
                !self.used_only || !p.loaded[i] || {
                    let c = &self.cells[i];
                    view(p, c).0 != PatState::Blank || c.name.is_some() || *c != Cell::as_read(i) || self.sel.contains(&i)
                }
            })
            .collect()
    }
}

/// What a slot shows: its state and name, with edits applied.
fn view(p: &Patterns, c: &Cell) -> (PatState, String) {
    match c.src {
        Some(s) => {
            let info = &p.info[s as usize];
            (info.state, c.name.clone().unwrap_or_else(|| info.name.clone()))
        }
        None => (PatState::Blank, c.name.clone().unwrap_or_default()),
    }
}

/// Move (swap) or copy the selection so the grabbed row `from` lands on `to`.
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
    // Fold state follows the patterns.
    let was: BTreeSet<usize> = ed.open.clone();
    for (s, d) in src.iter().zip(&dst) {
        if was.contains(s) {
            ed.open.insert(*d);
        } else {
            ed.open.remove(d);
        }
    }
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
    let names: Vec<String> = banks.iter().map(|b| bank_letter(*b).to_string()).collect();
    format!("Packed bank {}", names.join(", "))
}

/// Apply a name to `t`: as typed for one slot; for several, a template
/// where `{n}` counts from 1, `{slot}` is A01…, `{name}` the current name.
fn rename(ed: &mut ProjEd, p: &Patterns, t: &[usize], text: &str) -> String {
    if t.is_empty() {
        return String::new();
    }
    let mut empty = 0;
    ed.edit(|cells| {
        for (k, &i) in t.iter().enumerate() {
            let cur = view(p, &cells[i]).1;
            let raw = if t.len() > 1 {
                text.replace("{n}", &(k + 1).to_string()).replace("{slot}", &patterns::label(i)).replace("{name}", &cur)
            } else {
                text.to_owned()
            };
            let name = patterns::clean_name(&raw);
            let read = cells[i].src.map(|s| p.info[s as usize].name.clone()).unwrap_or_default();
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

fn bank_letter(b: usize) -> char {
    (b'A' + b as u8) as char
}

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
    keys(ui.ctx(), st);
    action_bar(ui, st, busy);
    ui.label(
        RichText::new("Click / ⇧click / ⌘click to select · drag to move (swaps with what's there) · ⌥ while dropping copies · ⌘C / ⌘V · ⌫ clears · ⏎ or double-click renames · ←/→ fold · renaming sets the pattern name and the kit name · patterns in songs aren't updated when moved")
            .weak()
            .small(),
    );
    list(ui, st, busy);
    out.append(&mut st.proj.want);
    if let Some(m) = st.proj.refused.take() {
        st.status = m;
    }
    elek_view::finish(ui, st, &mut out);
    out
}

fn keys(ctx: &egui::Context, st: &mut ElekState) {
    if ctx.egui_wants_keyboard_input() || st.edit_locked() {
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
    let vis = ed.visible(p);
    if m.command && z {
        ed.undo();
    }
    if m.command && a {
        ed.sel = vis.iter().copied().collect();
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
    if enter {
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
    ui.toggle_value(&mut ed.used_only, "Hide empty").on_hover_text("Hide empty slots (selected and changed ones stay)");
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

/// Tracks a folded-out row shows: the audio tracks, then MIDI tracks that
/// have something on them. `(label, index into seq)`.
fn shown_tracks(seq: &[TrackSeq], audio: usize) -> Vec<(String, usize)> {
    let mut v: Vec<(String, usize)> = (0..audio.min(seq.len())).map(|t| (format!("TRACK {}", t + 1), t)).collect();
    v.extend((audio..seq.len()).filter(|&t| !seq[t].is_empty()).map(|t| (format!("MIDI {}", t - audio + 1), t)));
    v
}

fn list(ui: &mut egui::Ui, st: &mut ElekState, busy: bool) {
    let Some(proj) = st.project.as_ref() else { return };
    let Ok(p) = proj.patterns.as_deref() else { return };
    let editable = p.read_only.is_none() && !busy;
    // Audio tracks: 8 on the DT1 (then 8 MIDI tracks), 16 on the DT2.
    let audio = proj.model.kit_tracks();
    let ed = &mut st.proj;
    let names = &st.names;
    let vis = ed.visible(p);

    let pointer = ui.input(|i| i.pointer.interact_pos());
    let in_list = |pt: egui::Pos2| ed.list_rect.is_some_and(|r| r.contains(pt));
    let under = pointer.filter(|pt| in_list(*pt)).and_then(|pt| ed.rows.iter().find(|(_, r)| r.contains(pt)).map(|(i, _)| *i));
    let mods = ui.input(|i| i.modifiers);
    // Where a drag would put the selection.
    let landing: BTreeSet<usize> = match (ed.drag, under) {
        (Some(from), Some(to)) if from != to => {
            let off = to as isize - from as isize;
            ed.sel.iter().map(|&s| s as isize + off).filter(|d| (0..PATTERNS as isize).contains(d)).map(|d| d as usize).collect()
        }
        _ => BTreeSet::new(),
    };
    let mut src_count = [0usize; PATTERNS];
    for c in &ed.cells {
        if let Some(s) = c.src {
            src_count[s as usize] += 1;
        }
    }
    // Pool slot → short file name, for the track lines.
    let sample_name = |slot: usize| -> String {
        if slot == 0 {
            return "—".into();
        }
        proj
            .pool
            .get(slot)
            .filter(|e| !e.is_empty())
            .and_then(|e| names.get(&e.key()))
            .map(|n| {
                let f = n.rsplit('/').next().unwrap_or(n);
                f.rsplit_once('.').map_or(f, |(stem, _)| stem).to_owned()
            })
            .unwrap_or_else(|| "?".into())
    };

    let mut status = None;
    let mut rows = Vec::new();
    let mut toggles = Vec::new();
    let mut bank_toggles = Vec::new();
    let mut inline_done: Option<(usize, Option<String>)> = None;

    let frame = egui::Frame::new().fill(OLED_BG).corner_radius(2.0).inner_margin(4.0);
    frame.show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        let sa = egui::ScrollArea::vertical().id_salt("proj_list").auto_shrink([false, false]).show(ui, |ui| {
            let clip = ui.clip_rect();
            // Scroll while dragging near the top or bottom edge.
            if ed.drag.is_some() {
                if let Some(pt) = pointer {
                    let edge = 36.0;
                    let dy = if pt.y < clip.top() + edge { 10.0 } else if pt.y > clip.bottom() - edge { -10.0 } else { 0.0 };
                    if dy != 0.0 {
                        ui.scroll_with_delta(egui::vec2(0.0, dy));
                        ui.ctx().request_repaint();
                    }
                }
            }
            let w = ui.available_width();
            for b in 0..BANKS {
                // The bank header folds its patterns in and out; dragging
                // over a folded one opens it.
                let (r, hdr) = ui.allocate_exact_size(egui::vec2(w, BANK_H), Sense::click());
                let bank_open = ed.banks[b];
                let used = (b * PER_BANK..(b + 1) * PER_BANK).filter(|&k| view(p, &ed.cells[k]).0 == PatState::Used).count();
                let changed = (b * PER_BANK..(b + 1) * PER_BANK).filter(|&k| ed.cells[k] != Cell::as_read(k)).count();
                if hdr.hovered() {
                    ui.painter().rect_filled(r, 2.0, OLED_HOVER);
                }
                let col = if hdr.hovered() || bank_open { OLED } else { OLED_DIM };
                triangle(ui.painter(), egui::pos2(r.left() + 14.0, r.center().y), bank_open, col);
                ui.painter().text(egui::pos2(r.left() + 26.0, r.center().y), egui::Align2::LEFT_CENTER, format!("BANK {}", bank_letter(b)), oled_font(13.0), col);
                let here = (b * PER_BANK..(b + 1) * PER_BANK).filter(|&k| p.loaded[k]).count();
                let mut note = format!("{used} in use");
                if here < PER_BANK {
                    note = if here == 0 { "not loaded".to_owned() } else { format!("{here}/{PER_BANK} loaded · {used} in use") };
                }
                if changed > 0 {
                    note.push_str(&format!(" · {changed} changed"));
                }
                ui.painter().text(egui::pos2(r.left() + 100.0, r.center().y), egui::Align2::LEFT_CENTER, note, oled_font(10.0), if changed > 0 { theme::ACCENT_BRIGHT } else { OLED_DIM });
                if hdr.clicked() {
                    bank_toggles.push(b);
                }
                if ed.drag.is_some() && !bank_open && pointer.is_some_and(|pt| r.contains(pt)) {
                    bank_toggles.push(b);
                }
                if !bank_open {
                    continue;
                }
                for &i in vis.iter().filter(|&&i| i / PER_BANK == b) {

                let cell = &ed.cells[i];
                let (state, name) = view(p, cell);
                let info = cell.src.map(|s| &p.info[s as usize]);
                let open = ed.open.contains(&i);
                let tracks = info.map(|n| shown_tracks(&n.seq, audio)).unwrap_or_default();
                let mut step_hover: Option<String> = None;
                let byte_rows = if ed.bytes && info.is_some() { 2.0 } else { 0.0 };
                let extra = if open { 4.0 + (tracks.len().max(1) as f32 + byte_rows) * TRACK_H + 4.0 } else { 0.0 };
                let (rect, _) = ui.allocate_exact_size(egui::vec2(w, ROW_H + extra), Sense::hover());
                let rect = rect.shrink2(egui::vec2(0.0, 1.0));
                rows.push((i, rect));
                if ed.scroll_to == Some(i) {
                    ui.scroll_to_rect(rect, None);
                    ed.scroll_to = None;
                }
                if !ui.is_rect_visible(rect) {
                    continue;
                }
                let resp = ui.interact(rect, ui.scope_id().with(("prow", i)), if editable { Sense::click_and_drag() } else { Sense::click() });
                let selected = ed.sel.contains(&i);
                let fill = if selected {
                    SEL_FILL
                } else if resp.hovered() {
                    OLED_HOVER
                } else {
                    ROW_FILL
                };
                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, 2.0, fill);
                let changed = *cell != Cell::as_read(i);
                if landing.contains(&i) {
                    painter.rect_stroke(rect, 2.0, Stroke::new(2.0, theme::ACCENT_BRIGHT), egui::StrokeKind::Inside);
                } else if ed.cursor == Some(i) {
                    painter.rect_stroke(rect, 2.0, Stroke::new(1.0, OLED_DIM), egui::StrokeKind::Inside);
                }
                if changed {
                    painter.rect_filled(Rect::from_min_size(rect.left_top(), egui::vec2(3.0, rect.height())), 1.0, theme::ACCENT);
                }

                let head = Rect::from_min_size(rect.left_top(), egui::vec2(rect.width(), ROW_H - 2.0));
                let y = head.center().y;
                let x0 = rect.left();
                // Fold-out arrow, its own click target.
                let chev = Rect::from_center_size(egui::pos2(x0 + 14.0, y), egui::vec2(44.0, ROW_H - 4.0));
                let chev_resp = ui.interact(chev, ui.scope_id().with(("pchev", i)), Sense::click());
                triangle(&painter, chev.center() + egui::vec2(12.0, 0.0), open, if chev_resp.hovered() { OLED } else { OLED_DIM });
                if chev_resp.clicked() {
                    toggles.push(i);
                }
                painter.text(egui::pos2(x0 + 34.0 + 12.0, y), egui::Align2::LEFT_CENTER, patterns::label(i), oled_font(12.0), OLED_DIM);
                let name_rect = Rect::from_min_size(egui::pos2(x0 + NAME_X, head.top() + 2.0), egui::vec2(NAME_W, head.height() - 4.0));
                let pending = cell.src.is_some_and(|s| (s as usize) < PATTERNS && !p.loaded[s as usize]);
                let (text, color) = match state {
                    _ if pending => ("loading…".to_owned(), OLED_DIM),
                    PatState::Used if name.is_empty() => ("(no name)".to_owned(), OLED_DIM),
                    PatState::Used => (name.clone(), OLED),
                    PatState::Phantom => (if name.is_empty() { "≈ nearly empty".to_owned() } else { name.clone() }, OLED_DIM),
                    PatState::Blank => (if name.is_empty() { "—".to_owned() } else { name.clone() }, Color32::from_rgb(70, 80, 78)),
                };
                if ed.inline != Some(i) {
                    painter.with_clip_rect(name_rect).text(name_rect.left_center(), egui::Align2::LEFT_CENTER, text, oled_font(14.0), color);
                }
                // Where it came from, when it's not what was read here.
                let mut bx = x0 + NAME_X + NAME_W + 6.0;
                let badge = match cell.src {
                    None if changed => Some("∅ cleared".to_owned()),
                    Some(s) if s as usize != i => Some(format!("{}{}", if src_count[s as usize] > 1 { "⧉" } else { "←" }, patterns::label(s as usize))),
                    _ => None,
                };
                for bt in badge.into_iter().chain(cell.name.is_some().then(|| "✎".to_owned())) {
                    let r = painter.text(egui::pos2(bx, y), egui::Align2::LEFT_CENTER, bt, oled_font(11.0), theme::ACCENT_BRIGHT);
                    bx = r.right() + 6.0;
                }

                // Folded: a tiny picture of the audio tracks. Folded out: one
                // dot line per track.
                if let Some(n) = info {
                    let dots_x = x0 + DOTS_X;
                    if !open && state != PatState::Blank {
                        let tr: Vec<&TrackSeq> = n.seq.iter().take(audio).collect();
                        mini(&painter, egui::pos2(dots_x, head.top() + 3.0), head.height() - 6.0, &tr);
                        let longest = tr.iter().map(|t| t.len).max().unwrap_or(0);
                        let notes: usize = tr.iter().map(|t| t.notes()).sum();
                        if !tr.is_empty() {
                            let right = dots_x + n.seq[0].steps.len() as f32 * 2.0 + 10.0;
                            painter.text(egui::pos2(right, y), egui::Align2::LEFT_CENTER, format!("{longest} steps · {notes} trigs"), oled_font(10.0), OLED_DIM);
                        }
                    }
                    if open {
                        let mut ty = head.bottom() + 4.0;
                        if tracks.is_empty() {
                            let why = if pending { "not loaded yet" } else { "step layout unknown for this version" };
                            painter.text(egui::pos2(x0 + 26.0, ty + TRACK_H / 2.0), egui::Align2::LEFT_CENTER, why, oled_font(10.0), OLED_DIM);
                        }
                        if ed.bytes && !tracks.is_empty() {
                            // Column offsets (from the track end), then the
                            // pattern-level bytes after the name.
                            let cols: Vec<String> = (0..patterns::TAIL).map(|k| format!("{:>2}", patterns::TAIL - k)).collect();
                            hex_row(&painter, egui::pos2(dots_x, ty + TRACK_H / 2.0), &cols, OLED_DIM, None);
                            painter.text(egui::pos2(x0 + DOTS_X - 10.0, ty + TRACK_H / 2.0), egui::Align2::RIGHT_CENTER, "bytes from track end: -", oled_font(10.0), OLED_DIM);
                            ty += TRACK_H;
                            let cy = ty + TRACK_H / 2.0;
                            painter.text(egui::pos2(x0 + 24.0, cy), egui::Align2::LEFT_CENTER, format!("PATTERN @{}", n.tail_at), oled_font(11.0), OLED_DIM);
                            let hx: Vec<String> = n.tail.iter().map(|b| format!("{b:02x}")).collect();
                            hex_row(&painter, egui::pos2(x0 + NAME_X + 60.0, cy), &hx, OLED, Some(&n.tail));
                            ty += TRACK_H;
                        }
                        for (label, t) in &tracks {
                            let cy = ty + TRACK_H / 2.0;
                            painter.text(egui::pos2(x0 + 24.0, cy), egui::Align2::LEFT_CENTER, label, oled_font(11.0), OLED_DIM);
                            if let Some(ti) = n.tracks.get(*t) {
                                // The sample, then the sound's own name when it isn't the default.
                                let mut text = sample_name(ti.slot);
                                if ti.name != format!("SOUND {}", t + 1) && !ti.name.is_empty() {
                                    text = format!("{text}  ·  {}", ti.name);
                                }
                                let sr = Rect::from_min_size(egui::pos2(x0 + NAME_X, ty), egui::vec2(DOTS_X - NAME_X - 56.0, TRACK_H));
                                painter.with_clip_rect(sr).text(sr.left_center(), egui::Align2::LEFT_CENTER, text, egui::FontId::proportional(12.0), OLED);
                            }
                            let seq = &n.seq[*t];
                            // Track length in steps (16 = one bar); ≈ when
                            // guessed from the last used step.
                            let len = if seq.len_known { seq.len.to_string() } else { format!("≈{}", seq.len) };
                            painter.text(egui::pos2(x0 + DOTS_X - 10.0, cy), egui::Align2::RIGHT_CENTER, len, oled_font(10.0), OLED_DIM);
                            if ed.bytes {
                                let hx: Vec<String> = seq.tail.iter().map(|b| format!("{b:02x}")).collect();
                                hex_row(&painter, egui::pos2(dots_x, cy), &hx, OLED, Some(&seq.tail));
                            } else {
                                let max_w = (rect.right() - dots_x - 10.0).max(120.0);
                                dots(&painter, egui::pos2(dots_x, cy), max_w, seq);
                                if let Some(pp) = pointer.filter(|pp| (pp.y - cy).abs() <= TRACK_H / 2.0 && rect.contains(*pp)) {
                                    if let Some(k) = step_at(seq.steps.len(), dots_x, max_w, pp.x).filter(|&k| k < seq.len && seq.steps[k] != Step::None) {
                                        step_hover = Some(step_tip(seq, k, label, &sample_name));
                                    }
                                }
                            }
                            ty += TRACK_H;
                        }
                    }
                } else if open {
                    painter.text(egui::pos2(x0 + 26.0, head.bottom() + 4.0 + TRACK_H / 2.0), egui::Align2::LEFT_CENTER, "empty pattern", oled_font(10.0), OLED_DIM);
                }

                // Rename in place.
                if ed.inline == Some(i) {
                    let te = egui::TextEdit::singleline(&mut ed.inline_text).char_limit(NAME_MAX).font(oled_font(14.0)).desired_width(NAME_W);
                    let r = ui.put(name_rect, te);
                    if ed.inline_focus {
                        ed.inline_focus = false;
                        r.request_focus();
                    }
                    if r.lost_focus() {
                        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                        inline_done = Some((i, (!esc).then(|| ed.inline_text.clone())));
                    }
                }

                let hover = if let Some(h) = step_hover.take() { h } else { match (state, info) {
                    (PatState::Used, Some(n)) if !n.kit_name.is_empty() && n.kit_name != n.name => format!(
                        "{}: {} bytes differ from an empty pattern\nkit name: \"{}\"",
                        patterns::label(i),
                        n.diff,
                        n.kit_name
                    ),
                    (PatState::Used, Some(n)) => format!("{}: {} bytes differ from an empty pattern", patterns::label(i), n.diff),
                    (PatState::Phantom, Some(n)) => format!("{}: nearly empty ({} bytes differ from an empty pattern: kit tweaks, no trigs)", patterns::label(i), n.diff),
                    _ => format!("{}: empty", patterns::label(i)),
                } };
                let resp = resp.on_hover_text_at_pointer(hover);
                if resp.drag_started() {
                    if !ed.sel.contains(&i) {
                        ed.select_only(i);
                    }
                    ed.drag = Some(i);
                }
                if resp.double_clicked() && editable {
                    ed.start_inline(p, i);
                } else if resp.clicked() {
                    if mods.shift {
                        let a = ed.cursor.unwrap_or(i);
                        let (lo, hi) = (a.min(i), a.max(i));
                        // Only what's shown: hidden banks stay out of it.
                        ed.sel.extend(vis.iter().copied().filter(|v| (lo..=hi).contains(v)));
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
                    if ui.add_enabled(editable, egui::Button::new("Rename…  ⏎")).clicked() {
                        if ed.targets().len() > 1 {
                            ed.focus_name = true;
                        } else {
                            ed.start_inline(p, i);
                        }
                        ui.close();
                    }
                    if ui.button(if open { "Fold  ←" } else { "Fold out  →" }).clicked() {
                        toggles.push(i);
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
                    ui.separator();
                    let one = ed.targets().len() == 1;
                    if ui
                        .add_enabled(one, egui::Button::new("Save pattern…"))
                        .on_hover_text("This pattern and its kit as you see it here, as a JSON file, with the +Drive files its samples point at")
                        .clicked()
                    {
                        ed.want.push(ElekIntent::SavePattern(i, view(p, &ed.cells[i]).1, false));
                        ui.close();
                    }
                    if ui
                        .add_enabled(one, egui::Button::new("Save pattern with samples…"))
                        .on_hover_text("A folder: the pattern file plus the WAVs of the samples it uses, from local copies or fetched from the device, to carry into other projects")
                        .clicked()
                    {
                        ed.want.push(ElekIntent::SavePattern(i, view(p, &ed.cells[i]).1, true));
                        ui.close();
                    }
                    if ui
                        .add_enabled(editable && one, egui::Button::new("Load pattern here…"))
                        .on_hover_text("Replace this slot with a saved pattern (or the .json inside a bundle folder). Its samples are matched to this pool by +Drive hash; others go into empty slots, found on the device or uploaded from the bundle. Push to send.")
                        .clicked()
                    {
                        ed.want.push(ElekIntent::LoadPattern(i));
                        ui.close();
                    }
                });
                }
            }
        });
        ed.list_rect = Some(sa.inner_rect);
    });
    ed.rows = rows;

    for b in bank_toggles {
        ed.banks[b] = !ed.banks[b];
    }
    for i in toggles {
        if !ed.open.remove(&i) {
            ed.open.insert(i);
        }
    }
    if let Some((i, text)) = inline_done {
        ed.inline = None;
        if let Some(t) = text {
            if t != view(p, &ed.cells[i]).1 {
                status = Some(rename(ed, p, &[i], &t));
            }
        }
    }
    if let Some(from) = ed.drag {
        if ui.input(|i| i.pointer.any_released()) {
            ed.drag = None;
            if let Some(to) = under.filter(|&t| t != from) {
                status = Some(drop_sel(ed, from, to, mods.alt).unwrap_or_else(|e| e));
            }
        } else if let Some(pt) = pointer {
            let n = ed.sel.len();
            let tip = format!("{} {n}{}", if mods.alt { "⧉ copy" } else { "move" }, under.map(|u| format!(" → {}", patterns::label(u))).unwrap_or_default());
            ui.painter().text(pt + egui::vec2(14.0, 8.0), egui::Align2::LEFT_TOP, tip, oled_font(12.0), theme::ACCENT_BRIGHT);
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
    }
    if let Some(s) = status {
        st.status = s;
    }
}

/// One track's steps as dots, from `at` (left, vertical centre), groups of
/// 16 slightly apart. Steps past the track's length aren't drawn. Returns
/// the right edge.
/// Byte cells in a fixed grid; zero bytes dimmed when `raw` is given.
fn hex_row(painter: &egui::Painter, at: egui::Pos2, cells: &[String], color: Color32, raw: Option<&[u8]>) {
    const PITCH: f32 = 20.0;
    for (k, c) in cells.iter().enumerate() {
        let x = at.x + k as f32 * PITCH + (k / 8) as f32 * 6.0;
        let col = if raw.is_some_and(|r| r.get(k) == Some(&0)) { DOT_BEAT } else { color };
        painter.text(egui::pos2(x, at.y), egui::Align2::LEFT_CENTER, c, oled_font(10.0), col);
    }
}

/// Step dot spacing: `(pitch, gap between 16-step groups)`.
fn dot_pitch(n: usize, max_w: f32) -> (f32, f32) {
    let gap = 5.0;
    let groups = n.div_ceil(16).saturating_sub(1) as f32;
    (((max_w - groups * gap) / n.max(1) as f32).clamp(3.0, 9.0), gap)
}

/// The step under `x` in a dot line drawn from `x0`.
fn step_at(n: usize, x0: f32, max_w: f32, x: f32) -> Option<usize> {
    let (pitch, gap) = dot_pitch(n, max_w);
    (0..n).find(|&k| (x - (x0 + k as f32 * pitch + (k / 16) as f32 * gap + pitch / 2.0)).abs() <= pitch / 2.0)
}

/// What's on step `k`: note-ish bytes, flags, p-locks.
fn step_tip(seq: &TrackSeq, k: usize, track: &str, sample_name: &dyn Fn(usize) -> String) -> String {
    let mut s = format!("{track} step {} (bar {}, step {})", k + 1, k / 16 + 1, k % 16 + 1);
    s += &format!("\ntrig flags {:04x}", seq.flags.get(k).copied().unwrap_or(0));
    let locks: Vec<String> = seq
        .locks
        .iter()
        .filter_map(|(p, v)| v.get(k).copied().flatten().map(|v| (*p, v)))
        .map(|(p, v)| {
            if Some(p) == seq.sample_param {
                // DT1 (64 steps) keeps the slot in the high byte.
                let slot = if seq.flags.len() == 64 { v >> 8 } else { v } as usize;
                format!("  sample → {slot:03} {}", sample_name(slot))
            } else {
                format!("  param {p:#04x} ({p}) = {} (raw {v:04x})", v >> 8)
            }
        })
        .collect();
    if locks.is_empty() {
        s += "\nno p-locks";
    } else {
        s += "\np-locks:\n";
        s += &locks.join("\n");
    }
    // Raw per-step bytes, for finding the note: offset in track = byte.
    let raw: Vec<String> = seq.arrays.iter().map(|(o, a)| format!("@{o}:{:02x}", a.get(k).copied().unwrap_or(0))).collect();
    if !raw.is_empty() {
        s += "\nper-step bytes (track offset:value):\n  ";
        s += &raw.chunks(4).map(|c| c.join("  ")).collect::<Vec<_>>().join("\n  ");
    }
    s
}

fn dots(painter: &egui::Painter, at: egui::Pos2, max_w: f32, seq: &TrackSeq) -> f32 {
    let n = seq.steps.len();
    let (pitch, gap) = dot_pitch(n, max_w);
    let groups = n.div_ceil(16).saturating_sub(1) as f32;
    let r = (pitch * 0.32).clamp(1.2, 2.8);
    let x_of = |k: usize| at.x + k as f32 * pitch + (k / 16) as f32 * gap + pitch / 2.0;
    for (k, s) in seq.steps.iter().enumerate().take(seq.len) {
        let c = egui::pos2(x_of(k), at.y);
        match s {
            Step::Note => {
                painter.circle_filled(c, r, OLED);
            }
            Step::Lock => {
                painter.circle_stroke(c, r - 0.3, Stroke::new(1.0, OLED_DIM));
            }
            Step::None => {
                painter.circle_filled(c, 1.0, if k % 4 == 0 { DOT_BEAT } else { DOT_OFF });
            }
        }
    }
    at.x + n as f32 * pitch + groups * gap
}

/// The audio tracks squeezed into a small picture: 2 px per step, one thin
/// line per track. More than 8 tracks (DT2) share lines two at a time, so
/// each line stays tall enough to read.
fn mini(painter: &egui::Painter, at: egui::Pos2, h: f32, tracks: &[&TrackSeq]) {
    const BG: Color32 = Color32::from_rgb(92, 42, 6);
    const LEN: Color32 = Color32::from_rgb(122, 60, 16);
    const LOCK: Color32 = Color32::from_rgb(196, 128, 70);
    let Some(first) = tracks.first() else { return };
    let n = first.steps.len();
    let per_line = if tracks.len() > 8 { 2 } else { 1 };
    let lines = tracks.len().div_ceil(per_line).max(1);
    let line = h / lines as f32;
    let bg = Rect::from_min_size(at, egui::vec2(n as f32 * 2.0, h));
    painter.rect_filled(bg.expand(1.0), 1.0, BG);
    for (l, group) in tracks.chunks(per_line).enumerate() {
        let y = at.y + l as f32 * line;
        // The longest track's length, faintly.
        let len = group.iter().map(|t| t.len).max().unwrap_or(0);
        let lr = Rect::from_min_size(egui::pos2(at.x, y + line * 0.35), egui::vec2(len as f32 * 2.0, (line * 0.3).max(0.5)));
        painter.rect_filled(lr, 0.0, LEN);
        for k in 0..len {
            // A note on either track wins over a lock.
            let step = |t: &&TrackSeq| if k < t.len { t.steps.get(k).copied().unwrap_or(Step::None) } else { Step::None };
            let c = if group.iter().any(|t| step(t) == Step::Note) {
                OLED
            } else if group.iter().any(|t| step(t) == Step::Lock) {
                LOCK
            } else {
                continue;
            };
            painter.rect_filled(Rect::from_min_size(egui::pos2(at.x + k as f32 * 2.0, y + 0.3), egui::vec2(1.6, (line - 0.6).max(1.0))), 0.0, c);
        }
    }
}

/// A fold arrow centred on `c`: pointing down when open.
fn triangle(painter: &egui::Painter, c: egui::Pos2, open: bool, color: Color32) {
    let pts = if open {
        vec![c + egui::vec2(-4.0, -2.5), c + egui::vec2(4.0, -2.5), c + egui::vec2(0.0, 3.5)]
    } else {
        vec![c + egui::vec2(-2.5, -4.0), c + egui::vec2(3.5, 0.0), c + egui::vec2(-2.5, 4.0)]
    };
    painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}
