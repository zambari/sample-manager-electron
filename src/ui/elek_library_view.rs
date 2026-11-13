//! The Sounds page (sibling of Patterns): the project's 128 pool sounds,
//! read by itself the first time the page shows with a device plugged in,
//! with the sample each plays, its machine and SRC page, and the kit tracks
//! that carry the same sound (name and sample), i.e. where it was probably
//! loaded from the pool. Drawn in the device's pixel font, like the Samples
//! and Patterns pages.
//!
//! The sound pool can be edited (with the whole project read): drag a sound
//! onto another to swap them (onto an empty one to move it), click a row to
//! highlight it and click its name again to rename it in place, right-click to rename or delete. Edits sit in `LibraryView::sounds` and go out with the
//! project push (`ElekProject::build_push`), sound locks renumbered. Data
//! comes from `device::library` and the patterns as read (`Patterns::info`).

use crate::device::machines::{plays_sample, SrcParam};
use crate::device::library::{sounds_as_read, sounds_changed, PoolSound, SoundCell, SoundSrc, SOUND_POOL, SRC_NAMES};
use crate::device::patterns::{self, PATTERNS};
use crate::device::ElekProject;
use crate::ui::elek_view::{self, ElekIntent, ElekState};
use crate::ui::theme::{self, oled_font, OLED, OLED_BG, OLED_DIM};
use egui::{Color32, RichText};
use std::collections::HashMap;

const ROW_H: f32 = 15.0;
/// How long a blocked edit's warning stays up.
const BLOCKED_SECS: f64 = 4.0;
/// Pixel-font sizes (before `oled_font`'s own scaling).
const F_NAME: f32 = 14.0;
const F_TEXT: f32 = 12.0;
const F_SMALL: f32 = 10.0;
const WARN: Color32 = Color32::from_rgb(230, 120, 90);
/// A kit track that differs from the most common kit.
const DIFFERS: Color32 = theme::CHANGED;

#[derive(Default)]
pub struct LibraryView {
    filter: String,
    /// The sound pool as last read; cleared when another project loads.
    pub pool_sounds: Option<Result<Vec<PoolSound>, String>>,
    /// `(done, total)` while the sound pool is being read.
    pub sound_progress: Option<(usize, usize)>,
    hide_blank: bool,
    /// The sound pool as edited, by index (empty: as read).
    pub sounds: Vec<SoundCell>,
    /// Earlier `sounds`, for undo.
    sound_undo: Vec<Vec<SoundCell>>,
    /// Sound-locked steps per pool sound (as read), once counted.
    lock_uses: Option<Vec<u32>>,
    /// An empty sound can be sent in place of a deleted one.
    blank_ok: Option<bool>,
    /// Renaming pool sound `.0` in its row: the name typed so far, focus
    /// next frame.
    renaming: Option<(usize, String, bool)>,
    /// The highlighted pool sound (a click on its row); a click on its name
    /// then renames it.
    selected: Option<usize>,
    /// SRC values at their defaults (TUNE 0, LEV 100, …) are left blank.
    pub(crate) show_defaults: bool,
    /// An edit was tried while editing is locked: why, and when (egui time),
    /// shown by the pointer and in the tool row for a few seconds.
    blocked: Option<(String, f64)>,
    /// The sound pool was read (or tried) by itself for this project.
    auto_read: bool,
}

impl LibraryView {
    /// Another project is loaded: what was read of the old one goes, and
    /// the sound pool read with it comes in.
    pub fn project_changed(&mut self, p: &ElekProject) {
        self.pool_sounds = p.pool_sounds().map(Ok);
        self.sound_progress = None;
        self.auto_read = false;
        self.sounds.clear();
        self.sound_undo.clear();
        self.renaming = None;
        self.selected = None;
        self.patterns_changed();
    }

    /// The patterns (or pool sounds) as read changed: recount.
    pub fn patterns_changed(&mut self) {
        self.lock_uses = None;
        self.blank_ok = None;
    }

    /// Pool sounds edited since the read.
    pub fn sound_changed(&self) -> usize {
        sounds_changed(&self.sounds)
    }

    fn cells_mut(&mut self) -> &mut Vec<SoundCell> {
        self.sound_undo.push(self.sounds.clone());
        if self.sounds.len() != SOUND_POOL {
            self.sounds = sounds_as_read();
        }
        &mut self.sounds
    }

    fn swap_sounds(&mut self, a: usize, b: usize) {
        self.cells_mut().swap(a, b);
    }

    fn rename_sound(&mut self, i: usize, name: String) {
        self.cells_mut()[i].name = Some(name);
    }

    fn delete_sound(&mut self, i: usize) {
        self.cells_mut()[i] = SoundCell { src: SoundSrc::Empty, name: None };
    }

    fn undo_sounds(&mut self) {
        if let Some(prev) = self.sound_undo.pop() {
            self.sounds = prev;
        }
    }
}

/// What is dragged on the sound pool: a pool sound, by index.
#[derive(Clone)]
struct SoundDrag(usize);

/// Why the sound pool can't be edited now, if it can't.
fn sounds_locked(st: &ElekState) -> Option<&'static str> {
    let Some(p) = st.project.as_deref() else { return Some("no project loaded") };
    if p.read_only.is_some() {
        Some("this project is read-only")
    } else if st.reading.is_some() {
        Some("the project is still being read; editing unlocks once every pattern is in")
    } else if p.partial || !p.sounds_read {
        Some("editing needs the whole project read (every pattern and the sound pool): reload it")
    } else if !p.locks_known() {
        Some("the sound locks in these patterns aren't understood (pattern version), so sounds can't be moved safely")
    } else if st.edit_locked() {
        Some("the device is busy")
    } else {
        None
    }
}

/// "A01–A16, B03" for a sorted list of pattern slots.
fn ranges(slots: &[usize]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < slots.len() {
        let mut j = i;
        while j + 1 < slots.len() && slots[j + 1] == slots[j] + 1 {
            j += 1;
        }
        out.push(if j == i {
            patterns::label(slots[i])
        } else {
            format!("{}–{}", patterns::label(slots[i]), patterns::label(slots[j]))
        });
        i = j + 1;
    }
    out.join(", ")
}

fn slot_label(slot: usize, pool_len: usize) -> String {
    if pool_len > 128 {
        format!("{}{:03}", (b'A' + (slot / 128) as u8) as char, slot % 128)
    } else {
        format!("{slot:03}")
    }
}

/// A cell of fixed width, text left-aligned and cut short to fit.
fn cell(ui: &mut egui::Ui, w: f32, text: impl Into<egui::WidgetText>) -> egui::Response {
    ui.allocate_ui_with_layout(egui::vec2(w, ROW_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_width(w);
        ui.add(egui::Label::new(text).truncate().sense(egui::Sense::click()))
    })
    .inner
}

fn matches(filter: &str, texts: &[&str]) -> bool {
    let f = filter.trim().to_lowercase();
    f.is_empty() || texts.iter().any(|t| t.to_lowercase().contains(&f))
}

pub(crate) fn view_tools(ui: &mut egui::Ui, st: &mut ElekState) {
    let lib = &mut st.lib;
    ui.add(egui::TextEdit::singleline(&mut lib.filter).hint_text("filter").desired_width(140.0));
    if !lib.filter.is_empty() && ui.small_button(theme::CLOSE_GLYPH).clicked() {
        lib.filter.clear();
    }
    ui.separator();
    ui.add(theme::soft_toggle(&mut lib.hide_blank, "Hide empty"));
    ui.add(theme::soft_toggle(&mut lib.show_defaults, "Show defaults"))
        .on_hover_text("Off: SRC values left at their defaults (TUNE 0, PLAY FWD, BIT 0, STRT 0, LEN 120, LOOP 0, LEV 100, and 0 in places not identified yet) are left blank, so changed ones stand out");
}

pub fn show(ui: &mut egui::Ui, st: &mut ElekState) -> Vec<ElekIntent> {
    let mut out = Vec::new();
    elek_view::top(ui, st, &mut out, |_, _| {});
    if st.project.is_none() {
        ui.horizontal_wrapped(|ui| view_tools(ui, st));
        elek_view::load_screen(ui, st, &mut out);
        elek_view::finish(ui, st, &mut out);
        return out;
    }
    let mut ping = None;
    sound_pool(ui, st, &mut ping, &mut out);
    // Clicking a sample points it out in the docked pool and plays it
    // (fetched into the preview cache first when there's no local copy).
    if let Some(key) = ping {
        st.ping_key(|k| k == key);
        out.push(ElekIntent::Local(key, false));
    }
    elek_view::finish(ui, st, &mut out);
    out
}

/// Short file name of pool slot `slot` (as read), and its key.
fn sample(p: &ElekProject, names: &HashMap<(u32, u32), String>, slot: usize) -> (String, Option<(u32, u32)>) {
    if slot == 0 {
        return ("—".into(), None);
    }
    let Some(e) = p.pool.get(slot).filter(|e| !e.is_empty()) else {
        return (format!("{} (empty slot)", slot_label(slot, p.pool.len())), None);
    };
    let name = names
        .get(&e.key())
        .map(|n| {
            let f = n.rsplit('/').next().unwrap_or(n);
            f.rsplit_once('.').map_or(f, |(stem, _)| stem).to_owned()
        })
        .unwrap_or_else(|| "?".into());
    (format!("{} {name}", slot_label(slot, p.pool.len())), Some(e.key()))
}

/// Kit tracks carrying pool sound `s` (same name and sample), by pattern:
/// `(pattern, track)` pairs. Empty for a sound without a name.
fn kit_uses(p: &ElekProject, s: &PoolSound) -> Vec<(usize, usize)> {
    let Ok(pats) = &p.patterns else { return Vec::new() };
    if s.name.trim().is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i, info) in pats.info.iter().enumerate().take(PATTERNS) {
        if !pats.loaded.get(i).copied().unwrap_or(false) {
            continue;
        }
        for (t, tr) in info.tracks.iter().enumerate() {
            if tr.slot == s.slot && tr.name == s.name {
                out.push((i, t));
            }
        }
    }
    out
}

/// SRC page places shown as columns (place 3, SMP, is the sample column).
pub(crate) const SRC_SHOWN: [usize; 7] = [0, 1, 2, 4, 5, 6, 7];
const SRC_W: f32 = 40.0;
const MACHINE_W: f32 = 70.0;
const NUM_W: f32 = 20.0;
const NAME_W: f32 = 100.0;
const SAMPLE_W: f32 = 115.0;
const KIT_W: f32 = 30.0;
const PATS_W: f32 = 120.0;
const LOCKS_W: f32 = 36.0;

/// The parameter the sound's machine keeps in SRC place `place`, if any.
pub(crate) fn param_at(s: &PoolSound, place: usize) -> Option<&SrcParam> {
    s.params.iter().find(|q| q.place == place)
}

/// The value a fresh one-shot sound has for this parameter (stored, fine
/// 0). Guessed names (`TUNE?`) count as the parameter they guess.
pub(crate) fn is_default(q: &SrcParam) -> bool {
    let want = match q.name.trim_end_matches('?') {
        "TUNE" => 64,
        "PLAY" | "BIT" | "STRT" | "LOOP" => 0,
        "LEN" => 120,
        "LEV" => 100,
        // Places not identified yet (P1? … P8?): 0 means nothing to see.
        n if n.len() == 2 && n.starts_with('P') && n.as_bytes()[1].is_ascii_digit() => 0,
        _ => return false,
    };
    q.raw == (want, 0)
}

/// The machine name, or "—" when its byte isn't known for this sound version.
pub(crate) fn machine_text(s: &PoolSound) -> String {
    s.machine.as_ref().map(|(_, n)| n.clone()).unwrap_or_else(|| "—".into())
}

/// The whole SRC page, with fine bytes, for a tooltip.
pub(crate) fn src_detail(s: &PoolSound) -> String {
    if s.src.is_empty() {
        return "SRC page layout not known for this sound version".into();
    }
    let head = match &s.machine {
        Some((id, n)) => format!("Machine {n} (byte {id}). SRC page as stored; names ending in ? are guesses, see src/device/elek/machines.rs:"),
        None => "SRC page as stored, read as a one-shot sound (machine byte not known for this sound version):".into(),
    };
    let vals: Vec<String> = s
        .src
        .iter()
        .enumerate()
        .map(|(i, (v, f))| match param_at(s, i) {
            Some(q) => format!("{} {}  (stored {v}, fine {f})", q.name, q.text),
            None => format!("place {}: unused  (stored {v}, fine {f})", i + 1),
        })
        .collect();
    format!("{head}\n{}", vals.join("\n"))
}

fn sound_pool(ui: &mut egui::Ui, st: &mut ElekState, ping: &mut Option<(u32, u32)>, out: &mut Vec<ElekIntent>) {
    let present = crate::device::presence().available();
    let reading = st.lib.sound_progress.is_some();
    // First look at this project's sound pool: read it without being asked.
    if present && !reading && !st.lib.auto_read && st.lib.pool_sounds.is_none() && st.busy.is_none() && st.project.is_some() {
        st.lib.auto_read = true;
        out.push(ElekIntent::ReadSounds);
    }
    let locked = sounds_locked(st);
    let editable = locked.is_none();
    if editable && st.lib.lock_uses.is_none() {
        if let Some(p) = st.project.as_deref() {
            st.lib.lock_uses = Some(p.sound_lock_uses());
            st.lib.blank_ok = Some(p.blank_sound().is_some());
        }
    }
    // ⌘V: paste copied track sounds into the highlighted (empty) sound.
    if editable && st.keys_on && st.lib.renaming.is_none() && !ui.ctx().egui_wants_keyboard_input() {
        let paste = ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))) || (i.modifiers.command_only() && i.key_pressed(egui::Key::V)));
        if paste {
            st.status = match (st.lib.selected, st.project.as_deref()) {
                (Some(at), Some(p)) => {
                    let sounds = p.edited_sounds(&st.lib.sounds);
                    paste_track_sounds(st, at, &sounds, false)
                }
                _ => "Click a pool sound first, then ⌘V pastes the copied track sound there".into(),
            };
        }
    }
    ui.horizontal(|ui| {
        if editable {
            let lib = &mut st.lib;
            let changed = lib.sound_changed();
            if ui.add_enabled(!lib.sound_undo.is_empty(), egui::Button::new("↶ Undo")).on_hover_text("Undo the last sound pool edit").clicked() {
                lib.undo_sounds();
            }
            if ui.add_enabled(changed > 0, egui::Button::new("↺ Reset sounds")).on_hover_text("Back to the sound pool as read").clicked() {
                lib.sound_undo.push(lib.sounds.clone());
                lib.sounds.clear();
            }
            if changed > 0 {
                ui.label(RichText::new(format!("{changed} pool sound(s) changed: ⬆ Push to device sends them")).color(DIFFERS));
            }
        } else {
            let label = if st.lib.pool_sounds.is_some() { "⟳ Read sound pool again" } else { "⬇ Read sound pool" };
            if ui
                .add_enabled(present && !reading && st.busy.is_none(), egui::Button::new(label))
                .on_hover_text("Asks the device for each of the project's 128 pool sounds. Read-only")
                .clicked()
            {
                out.push(ElekIntent::ReadSounds);
            }
            let now = ui.input(|i| i.time);
            let flash = st.lib.blocked.as_ref().filter(|(_, t)| now - t < BLOCKED_SECS);
            if let Some((msg, _)) = flash {
                ui.colored_label(WARN, msg);
            } else if let Some(why) = locked {
                ui.label(RichText::new(format!("Not editable: {why}")).weak().small());
            }
        }
        if let Some((done, total)) = st.lib.sound_progress {
            ui.add(theme::progress_bar(done as f32 / total.max(1) as f32).desired_width(200.0).text(format!("{done}/{total}")));
        }
    });
    let Some(p) = st.project.as_deref() else { return };
    let sounds: Vec<PoolSound> = if editable {
        p.edited_sounds(&st.lib.sounds)
    } else {
        match &st.lib.pool_sounds {
            None if reading => {
                ui.label(RichText::new("Reading the sound pool…").weak());
                return;
            }
            None => {
                ui.label(RichText::new(if present { "Not read yet." } else { "Not read yet. Plug the device in to read it." }).weak());
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(WARN, e);
                return;
            }
            Some(Ok(s)) => s.clone(),
        }
    };
    let filled = sounds.iter().filter(|s| !s.blank).count();
    let with_sample = sounds.iter().filter(|s| !s.blank && s.slot != 0).count();
    let tip = if editable {
        "Drag a sound by its number or name onto another to swap them (onto an empty one to move it); click a row to highlight it, then click its name to rename it; right-click to rename or delete. A track sound copied on the Patterns page (Copy ▸ track sound) pastes in with right-click or ⌘V on a highlighted empty sound. Click a sample to play it. ⬆ Push to device sends the changes, with every pattern's sound locks following their sounds."
    } else {
        "Read-only until the whole project is read"
    };
    ui.label(RichText::new(format!("{filled} OF {SOUND_POOL} SOUNDS IN USE, {with_sample} WITH A SAMPLE")).font(oled_font(F_TEXT)).color(OLED_DIM)).on_hover_text(tip);
    if p.partial {
        ui.colored_label(WARN, "Not every pattern was read: the kit column is short.");
    }
    let frame = egui::Frame::new().fill(OLED_BG).corner_radius(2.0).inner_margin(4.0);
    let drop = frame
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 0.0);
            sound_rows(ui, st, &sounds, editable, ping)
        })
        .inner;
    // The dragged thing follows the pointer.
    let ctx = ui.ctx().clone();
    if let (Some(d), Some(pos)) = (egui::DragAndDrop::payload::<SoundDrag>(&ctx), ctx.pointer_latest_pos()) {
        let i = d.0;
        let label = format!("{:03} {}", i + 1, sounds.get(i).map(|s| s.name.as_str()).unwrap_or(""));
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("sound_drag")));
        let font = oled_font(F_NAME);
        let galley = painter.layout_no_wrap(label, font, OLED);
        let r = egui::Rect::from_min_size(pos + egui::vec2(14.0, -8.0), galley.size() + egui::vec2(8.0, 4.0));
        painter.rect_filled(r, 2.0, theme::OLED_BG);
        painter.galley(r.min + egui::vec2(4.0, 2.0), galley, OLED);
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    if let Some((what, to)) = drop {
        drop_sound(st, what, to, &sounds);
    }
}

/// The pool sound rows. Returns what was dropped on which row.
fn sound_rows(ui: &mut egui::Ui, st: &mut ElekState, sounds: &[PoolSound], editable: bool, ping: &mut Option<(u32, u32)>) -> Option<(SoundDrag, usize)> {
    let p = st.project.as_deref()?;
    let filter = st.lib.filter.clone();
    let lock_uses = st.lib.lock_uses.clone().unwrap_or_default();
    let blank_ok = st.lib.blank_ok.unwrap_or(false);
    let cells = st.lib.sounds.clone();
    let show_defaults = st.lib.show_defaults;
    let mut renaming = st.lib.renaming.take();
    // `Some(Some(name))`: rename committed; `Some(None)`: cancelled.
    let mut rename_done: Option<Option<String>> = None;
    let mut rename_start: Option<usize> = None;
    let mut selected = st.lib.selected;
    // An edit gesture on a locked sound pool: what was tried.
    let mut blocked: Option<&str> = None;
    let sel_bg = Color32::from_rgba_unmultiplied(255, 255, 255, 22);
    let cell_of = |i: usize| cells.get(i).cloned().unwrap_or_else(|| SoundCell::as_read(i));
    // Locks follow the sound: count them by where it was read.
    // A pasted track sound takes over the locks of the sound it replaced,
    // unless that sound moved elsewhere (`sound_moves`).
    let locks_of = |i: usize| match cell_of(i).src {
        SoundSrc::Read(o) if editable => lock_uses.get(o as usize).copied().unwrap_or(0),
        SoundSrc::Track { .. } if editable && !(0..SOUND_POOL).any(|j| cell_of(j).src == SoundSrc::Read(i as u8)) => {
            lock_uses.get(i).copied().unwrap_or(0)
        }
        _ => 0,
    };
    let copied = st.proj.copied_sounds();
    let paste_what = match copied.as_slice() {
        [] => String::new(),
        [(t, Some(pat))] => format!("track {} sound of {}", *t as usize + 1, patterns::label(*pat as usize)),
        [(t, None)] => format!("track {} sound (empty pattern)", *t as usize + 1),
        many => format!("{} track sounds", many.len()),
    };
    let shown: Vec<(&PoolSound, Vec<(usize, usize)>)> = sounds
        .iter()
        .filter(|s| !st.lib.hide_blank || !s.blank)
        .filter(|s| {
            let n = sample(p, &st.names, s.slot).0;
            matches(&filter, &[&s.name, &n])
        })
        .map(|s| (s, kit_uses(p, s)))
        .collect();
    let head = |t: &str| RichText::new(t).font(oled_font(F_SMALL)).color(OLED_DIM);
    ui.horizontal(|ui| {
        cell(ui, NUM_W, head("#"));
        cell(ui, NAME_W, head("SOUND"));
        cell(ui, SAMPLE_W, head("SAMPLE"));
        cell(ui, MACHINE_W, head("MACHINE")).on_hover_text("The sound's machine, from its machine byte. Names ending in ? are guesses");
        for i in SRC_SHOWN {
            cell(ui, SRC_W, head(&format!("SRC{}", i + 1))).on_hover_text(format!(
                "SRC page place {} (one-shot: {}). Each machine keeps its own parameter here, named in the cell; ? = not confirmed yet",
                i + 1,
                SRC_NAMES[i]
            ));
        }
        cell(ui, KIT_W, head("KIT"))
            .on_hover_text("Kit tracks with the same sound name and sample: probably loaded from this pool sound (or saved into it)");
        cell(ui, PATS_W, head("IN PATTERNS"));
        if editable {
            cell(ui, LOCKS_W, head("LOCKS")).on_hover_text("Steps sound-locked to this pool sound, over all patterns. They follow it when it moves");
        }
    });
    ui.add_space(2.0);
    let mut dropped = None;
    let mut menu: Option<(usize, MenuPick)> = None;
    theme::always_show_scrollbar(ui);
    egui::ScrollArea::vertical().id_salt("sound_pool_rows").auto_shrink([false; 2]).show_rows(ui, ROW_H, shown.len(), |ui, range| {
        for (s, uses) in &shown[range] {
            let edited = editable && cell_of(s.index) != SoundCell::as_read(s.index);
            let is_sel = selected == Some(s.index);
            // Highlight goes under the row, so it's painted into this slot later.
            let bg = ui.painter().add(egui::Shape::Noop);
            // A click on the row, outside the sample (which plays) and the
            // number / name (handled below).
            let mut hit = false;
            let mut name_hit = false;
            let row = ui.horizontal(|ui| {
                let color = if s.blank { OLED_DIM } else if edited { DIFFERS } else { OLED };
                let txt = |t: String, c: Color32| RichText::new(t).font(oled_font(F_TEXT)).color(c);
                hit |= cell(ui, NUM_W, txt(format!("{:03}", s.index + 1), OLED_DIM)).clicked();
                match renaming.as_mut().filter(|r| r.0 == s.index) {
                    Some((_, text, focus)) => {
                        let r = ui.add_sized(
                            egui::vec2(NAME_W, ROW_H),
                            egui::TextEdit::singleline(text)
                                .char_limit(patterns::NAME_MAX)
                                .font(oled_font(F_TEXT))
                                .text_color(OLED)
                                .background_color(theme::OLED_BG)
                                .margin(egui::vec2(2.0, 0.0)),
                        );
                        if std::mem::take(focus) {
                            r.request_focus();
                        }
                        if ui.input(|x| x.key_pressed(egui::Key::Escape)) {
                            rename_done = Some(None);
                        } else if r.lost_focus() {
                            // Enter or a click elsewhere keeps what was typed.
                            rename_done = Some(Some(text.clone()));
                        }
                    }
                    None => {
                        let name = if s.name.is_empty() { "—" } else { &s.name };
                        name_hit = cell(ui, NAME_W, RichText::new(name).font(oled_font(F_NAME)).color(color)).on_hover_text(src_detail(s)).clicked();
                    }
                }
                let (n, key) = sample(p, &st.names, s.slot);
                // Blank slots carry whatever machine byte was left there.
                let no_sample = !s.blank && s.machine.as_ref().is_some_and(|(id, _)| !plays_sample(p.model, *id));
                let n = if s.blank { String::new() } else if no_sample { "— (synth)".to_owned() } else { n };
                let r = cell(ui, SAMPLE_W, txt(n, if s.slot == 0 || no_sample { OLED_DIM } else { color }));
                if let (true, Some(k)) = (r.on_hover_text("Click: play it once and point it out in the docked pool (fetched from the device first when there is no local copy)").clicked(), key) {
                    *ping = Some(k);
                }
                hit |= cell(ui, MACHINE_W, txt(if s.blank { String::new() } else { machine_text(s) }, color)).on_hover_text(src_detail(s)).clicked();
                for i in SRC_SHOWN {
                    match param_at(s, i).filter(|q| !s.blank && (show_defaults || !is_default(q))) {
                        Some(q) => {
                            let mut job = egui::text::LayoutJob::default();
                            let (style, fs, al) = (ui.style().clone(), egui::FontSelection::Default, egui::Align::Center);
                            // PLAY's values (FWD LP, …) say what they are.
                            if q.name != "PLAY" {
                                RichText::new(format!("{} ", q.name)).font(oled_font(F_SMALL)).color(OLED_DIM).append_to(&mut job, &style, fs, al);
                            }
                            RichText::new(&q.text).font(oled_font(F_TEXT)).color(color).append_to(&mut job, &style, egui::FontSelection::Default, al);
                            hit |= cell(ui, SRC_W, job).on_hover_text(format!("{} {}: stored {}, fine {}", q.name, q.text, q.raw.0, q.raw.1)).clicked();
                        }
                        None => {
                            hit |= cell(ui, SRC_W, "").clicked();
                        }
                    }
                }
                hit |= cell(ui, KIT_W, txt(if uses.is_empty() { "—".into() } else { uses.len().to_string() }, color)).clicked();
                let mut pats: Vec<usize> = uses.iter().map(|u| u.0).collect();
                pats.dedup();
                let pl = ranges(&pats);
                let detail: Vec<String> = uses.iter().map(|(i, t)| format!("{} T{}", patterns::label(*i), t + 1)).collect();
                let r = cell(ui, PATS_W, txt(if pl.is_empty() { "—".into() } else { pl }, color));
                let r = if detail.is_empty() { r } else { r.on_hover_text(detail.join(", ")) };
                hit |= r.clicked();
                if editable {
                    let n = locks_of(s.index);
                    hit |= cell(ui, LOCKS_W, txt(if n == 0 { "—".into() } else { n.to_string() }, color)).clicked();
                }
            });
            let rect = row.response.rect;
            if is_sel {
                ui.painter().set(bg, egui::Shape::rect_filled(rect.expand2(egui::vec2(2.0, 0.0)), 0.0, sel_bg));
            }
            // Name clicked on the highlighted row: rename (or say why not).
            let click_name = |name_click: bool| {
                if name_click && is_sel && !s.blank {
                    Some(s.index)
                } else {
                    None
                }
            };
            if hit {
                selected = Some(s.index);
            }
            let in_edit = renaming.as_ref().is_some_and(|r| r.0 == s.index);
            if !editable {
                // Same gestures as an editable row, answered with why not.
                let _ = name_hit;
                if !s.blank && !in_edit {
                    let grip = egui::Rect::from_min_max(rect.min, egui::pos2(rect.min.x + NUM_W + NAME_W + 6.0, rect.max.y));
                    let r = ui.interact(grip, ui.scope_id().with(("pool_sound_ro", s.index)), egui::Sense::click_and_drag());
                    if r.clicked() {
                        let on_name = r.interact_pointer_pos().is_some_and(|q| q.x > rect.min.x + NUM_W + 3.0);
                        rename_start = rename_start.or(click_name(on_name));
                        selected = Some(s.index);
                    }
                    if r.drag_started() {
                        blocked = Some("Can't move pool sounds yet");
                    }
                    if r.secondary_clicked() {
                        blocked = Some("Can't rename or delete pool sounds yet");
                    }
                } else if name_hit {
                    selected = Some(s.index);
                }
                continue;
            }
            if in_edit {
                continue;
            }
            // The row as a whole: dragged by its number and name, dropped
            // on anywhere, right-clicked anywhere.
            let rect = row.response.rect;
            let grip = egui::Rect::from_min_max(rect.min, egui::pos2(rect.min.x + NUM_W + NAME_W + 6.0, rect.max.y));
            let id = ui.scope_id().with(("pool_sound", s.index));
            let r = ui.interact(grip, id, egui::Sense::click_and_drag());
            if r.hovered() && !s.blank && egui::DragAndDrop::payload::<SoundDrag>(ui.ctx()).is_none() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            }
            if r.clicked() {
                let on_name = r.interact_pointer_pos().is_some_and(|q| q.x > rect.min.x + NUM_W + 3.0);
                rename_start = rename_start.or(click_name(on_name));
                selected = Some(s.index);
            }
            if r.drag_started() && !s.blank {
                egui::DragAndDrop::set_payload(ui.ctx(), SoundDrag(s.index));
            }
            let over = ui.interact(rect, id.with("drop"), egui::Sense::hover());
            if let Some(d) = over.dnd_hover_payload::<SoundDrag>() {
                let ok = d.0 != s.index;
                let c = if ok { theme::DEVICE_ACCENT_BRIGHT } else { WARN };
                ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(1.0, c), egui::StrokeKind::Inside);
            }
            if let Some(d) = over.dnd_release_payload::<SoundDrag>() {
                dropped = Some(((*d).clone(), s.index));
            }
            let r = if s.blank {
                ui.interact(rect, id.with("menu"), egui::Sense::click())
            } else {
                r.on_hover_text("Drag onto another sound to swap them, or onto an empty one to move it. Click to highlight; click the highlighted name to rename it. Right-click to rename, delete or paste a copied track sound over it")
            };
            r.context_menu(|ui| {
                let label = if s.blank { format!("Pool sound {:03} (empty)", s.index + 1) } else { format!("Pool sound {:03} “{}”", s.index + 1, s.name) };
                ui.label(RichText::new(label).weak());
                let ok = !copied.is_empty();
                let (text, tip) = match (ok, s.blank) {
                    (false, _) => (
                        "Paste track sound".to_owned(),
                        "Nothing copied: on the Patterns page, fold out a pattern, select a track and right-click → Copy ▸ track sound".to_owned(),
                    ),
                    (true, true) => (
                        format!("Paste {paste_what}"),
                        if copied.len() > 1 { "Fills this sound and the next empty ones below it".to_owned() } else { "Puts the copied kit sound here as a pool sound".to_owned() },
                    ),
                    (true, false) => {
                        let locks = locks_of(s.index);
                        let more = if copied.len() > 1 { "; the rest fill the next empty sounds below" } else { "" };
                        let lk = if locks > 0 { format!(". Its {locks} sound-locked step(s) will play the pasted sound") } else { String::new() };
                        (format!("Paste {paste_what} over this one"), format!("Replaces “{}”{lk}{more}", s.name))
                    }
                };
                if ui.add_enabled(ok, egui::Button::new(text)).on_hover_text(tip.clone()).on_disabled_hover_text(tip).clicked() {
                    menu = Some((s.index, MenuPick::Paste));
                    ui.close();
                }
                if !s.blank {
                    ui.separator();
                    if ui.button("Rename…").clicked() {
                        menu = Some((s.index, MenuPick::Rename));
                        ui.close();
                    }
                    let locks = locks_of(s.index);
                    let tip = if !blank_ok {
                        "No empty sound to send in its place: this project has no empty pattern or empty pool sound to copy one from".to_owned()
                    } else if locks > 0 {
                        format!("{locks} sound-locked step(s) in the patterns point at it; those locks are removed")
                    } else {
                        "Empties this pool sound. Kit tracks keep their own copies".to_owned()
                    };
                    let label = if locks > 0 { format!("Delete ({locks} locks removed)") } else { "Delete".to_owned() };
                    if ui.add_enabled(blank_ok, egui::Button::new(label)).on_hover_text(tip.clone()).on_disabled_hover_text(tip).clicked() {
                        menu = Some((s.index, MenuPick::Delete));
                        ui.close();
                    }
                }
            });
        }
    });
    match rename_done {
        Some(Some(text)) => {
            if let Some((i, _, _)) = renaming.take() {
                let name = patterns::clean_name(&text);
                if sounds.get(i).is_none_or(|s| s.name != name) {
                    st.lib.rename_sound(i, name.clone());
                    st.status = format!("Pool sound {:03} renamed “{name}”. Push to send it", i + 1);
                }
            }
        }
        Some(None) => renaming = None,
        None => {}
    }
    st.lib.renaming = renaming;
    st.lib.selected = selected;
    if let Some((i, MenuPick::Rename)) = menu {
        rename_start = Some(i);
    }
    if let Some(i) = rename_start {
        match sounds_locked(st) {
            None => {
                let name = sounds.get(i).map(|s| s.name.clone()).unwrap_or_default();
                st.lib.renaming = Some((i, name, true));
            }
            Some(_) => blocked = Some("Can't rename pool sounds yet"),
        }
    }
    if let (Some(what), Some(why)) = (blocked, sounds_locked(st)) {
        let msg = format!("{what}: {why}");
        st.status = msg.clone();
        st.lib.blocked = Some((msg, ui.input(|i| i.time)));
    }
    blocked_tip(ui.ctx(), st);
    match menu {
        Some((_, MenuPick::Rename)) => {}
        Some((i, MenuPick::Paste)) => st.status = paste_track_sounds(st, i, sounds, true),
        Some((i, MenuPick::Delete)) => {
            let name = sounds.get(i).map(|s| s.name.clone()).unwrap_or_default();
            st.lib.delete_sound(i);
            st.status = format!("Pool sound {:03} “{name}” deleted. Push to send it", i + 1);
        }
        None => {}
    }
    dropped
}

enum MenuPick {
    Rename,
    Delete,
    Paste,
}

/// Put the Patterns page's copied track sounds into the sound pool: the
/// first at `at` (only over a filled sound with `over`), the rest into the
/// next empty sounds below it. Returns the status line.
fn paste_track_sounds(st: &mut ElekState, at: usize, sounds: &[PoolSound], over: bool) -> String {
    let copied = st.proj.copied_sounds();
    if copied.is_empty() {
        return "No track sound copied: on the Patterns page, select a track and right-click → Copy ▸ track sound".into();
    }
    let Some(p) = st.project.as_deref() else { return "No project loaded".into() };
    let blank = |i: usize| sounds.get(i).is_none_or(|s| s.blank);
    if !over && !blank(at) {
        return format!("Pool sound {:03} isn't empty: right-click it → Paste … over this one to replace it", at + 1);
    }
    let mut problems = Vec::new();
    let good: Vec<(u8, u8)> = copied
        .iter()
        .filter_map(|&(t, pat)| match p.track_sound_for_pool(pat, t, &[]) {
            Ok(_) => pat.map(|pat| (pat, t)),
            Err(e) => {
                problems.push(e);
                None
            }
        })
        .collect();
    let mut targets = vec![at];
    let mut next = at + 1;
    while targets.len() < good.len() {
        match (next..SOUND_POOL).find(|&i| blank(i)) {
            Some(i) => {
                targets.push(i);
                next = i + 1;
            }
            None => break,
        }
    }
    let n = targets.len().min(good.len());
    if n == 0 {
        return format!("Nothing pasted: {}", problems.join("; "));
    }
    let cells = st.lib.cells_mut();
    for (&i, &(pattern, track)) in targets.iter().zip(&good) {
        cells[i] = SoundCell { src: SoundSrc::Track { pattern, track }, name: None };
    }
    st.lib.selected = Some(at);
    let mut msg = if n == 1 {
        let (pat, t) = good[0];
        format!("Track {} sound of {} pasted into pool sound {:03}. Push to send it", t as usize + 1, patterns::label(pat as usize), at + 1)
    } else {
        format!("{n} track sounds pasted into pool sounds {}. Push to send them", targets[..n].iter().map(|i| format!("{:03}", i + 1)).collect::<Vec<_>>().join(", "))
    };
    if good.len() > n {
        msg += &format!("; {} didn't fit (no empty sounds left below)", good.len() - n);
    }
    if !problems.is_empty() {
        msg += &format!("; skipped: {}", problems.join("; "));
    }
    msg
}

/// A sound let go over pool sound `to`.
fn drop_sound(st: &mut ElekState, SoundDrag(from): SoundDrag, to: usize, sounds: &[PoolSound]) {
    if from == to {
        return;
    }
    let blank = sounds.get(to).is_none_or(|s| s.blank);
    st.lib.swap_sounds(from, to);
    let name = sounds.get(from).map(|s| s.name.clone()).unwrap_or_default();
    st.status = if blank {
        format!("“{name}” moved {:03} → {:03}. Push to send it", from + 1, to + 1)
    } else {
        format!("Swapped {:03} and {:03}. Push to send it", from + 1, to + 1)
    };
}

/// The latest blocked edit's warning, by the pointer, for a few seconds.
fn blocked_tip(ctx: &egui::Context, st: &ElekState) {
    let Some((msg, t)) = &st.lib.blocked else { return };
    let age = ctx.input(|i| i.time) - t;
    if age >= BLOCKED_SECS {
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(BLOCKED_SECS - age));
    let Some(pos) = ctx.pointer_latest_pos() else { return };
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("sound_blocked")));
    let galley = painter.layout(msg.clone(), egui::FontId::proportional(13.0), Color32::WHITE, 360.0);
    let r = egui::Rect::from_min_size(pos + egui::vec2(14.0, 10.0), galley.size() + egui::vec2(12.0, 8.0));
    painter.rect_filled(r, 3.0, WARN.gamma_multiply(0.9));
    painter.galley(r.min + egui::vec2(6.0, 4.0), galley, Color32::WHITE);
}
