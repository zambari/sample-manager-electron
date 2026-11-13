//! The Elek Project Editor (experimental; DT1 and DT2, DT2 less tested): the loaded
//! project's 128 patterns as one long list, filtered by bank. Each row folds
//! out into a dot view of its tracks' steps. Move, copy, clear and rename
//! whole patterns, or copy single tracks into other patterns; the result goes to the device with the same push as the
//! sample order, so both pages edit one loaded project.

use crate::device::library::PoolSound;
use crate::device::patterns::{self, Patterns, Step, TrackSeq, NAME_MAX, PATTERNS};
use crate::ui::elek_library_view::{is_default, machine_text, param_at, src_detail, SRC_SHOWN};
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
/// Outline of a selected pattern row (no fill, so the row stays readable).
const SEL_EDGE: Color32 = Color32::from_rgb(190, 104, 40);
const ROW_FILL: Color32 = Color32::from_rgb(18, 21, 21);
const DOT_OFF: Color32 = Color32::from_rgb(38, 45, 44);
const DOT_BEAT: Color32 = Color32::from_rgb(62, 72, 70);
/// A selected track line in a folded-out row.
const TRACK_SEL: Color32 = Color32::from_rgb(110, 58, 18);

/// A track paste waiting for the user: what it would overwrite.
struct TrackPaste {
    into: Vec<usize>,
    part: Part,
    lost: Vec<String>,
}

/// What of a track is copied or pasted: all of it (pasting: whatever is
/// copied), its sound (kit sound, MIDI bit) or its events (trigs, p-locks).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Track,
    Sound,
    Events,
}

impl Part {
    fn events(self) -> bool {
        self != Part::Sound
    }
    fn sound(self) -> bool {
        self != Part::Events
    }
}

/// Copied tracks: track → the pattern it comes from (`None`: empty), and the
/// slot they were copied from.
#[derive(Clone)]
struct TrackClip {
    tracks: Vec<(u8, Option<u8>)>,
    from: usize,
}

#[derive(Default)]
pub struct ProjEd {
    /// `cells[slot]`: what goes into each slot. Empty while there are no
    /// patterns to edit.
    pub cells: Vec<Cell>,
    undo: Vec<Vec<Cell>>,
    sel: BTreeSet<usize>,
    cursor: Option<usize>,
    clip: Vec<Cell>,
    /// Selected tracks (folded-out rows), all in one pattern, and the last
    /// one clicked (for ⇧click).
    tsel_pat: Option<usize>,
    tsel: BTreeSet<u8>,
    tanchor: Option<u8>,
    /// Copied track events and track sounds. These or `clip` are set; copying
    /// events alone drops the copied sounds.
    tclip_ev: Option<TrackClip>,
    tclip_snd: Option<TrackClip>,
    /// A track paste that would lose trigs, until confirmed.
    confirm: Option<TrackPaste>,
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
    /// Folded-out rows whose SOUNDS foldout (kit track sounds) is open too.
    snd_open: BTreeSet<usize>,
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
                self.tsel.clear();
                self.tsel_pat = None;
                self.confirm = None;
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
            self.edit(|c| c[slot] = Cell::with_src(src));
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
            .flat_map(|i| std::iter::once(i).chain(self.cells[i].sources().map(usize::from).filter(|&s| s < PATTERNS)).collect::<Vec<_>>())
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
        self.clear_tracks();
        self.sel.clear();
        self.sel.insert(i);
        self.cursor = Some(i);
    }

    fn clear_tracks(&mut self) {
        self.tsel.clear();
        self.tsel_pat = None;
        self.tanchor = None;
    }

    /// Where a track paste goes: the pattern whose tracks are selected, else
    /// the selected patterns.
    fn track_targets(&self) -> Vec<usize> {
        match self.tsel_pat {
            Some(i) if !self.tsel.is_empty() => vec![i],
            _ => self.targets(),
        }
    }

    /// A click on track `t` of pattern `i`; `shown` = the row's track lines.
    fn click_track(&mut self, i: usize, t: u8, shown: &[u8], m: egui::Modifiers) {
        if self.tsel_pat != Some(i) {
            self.clear_tracks();
            self.tsel_pat = Some(i);
        }
        if m.command {
            if !self.tsel.remove(&t) {
                self.tsel.insert(t);
            }
        } else if m.shift {
            let a = self.tanchor.unwrap_or(t);
            let (lo, hi) = (a.min(t), a.max(t));
            self.tsel.extend(shown.iter().copied().filter(|x| (lo..=hi).contains(x)));
        } else {
            self.tsel = [t].into();
        }
        self.tanchor = Some(t);
        self.sel.clear();
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
    let rank = |s: PatState| match s {
        PatState::Blank => 0,
        PatState::Phantom => 1,
        PatState::Used => 2,
    };
    let (mut state, name) = match c.src {
        Some(s) => {
            let info = &p.info[s as usize];
            (info.state, c.name.clone().unwrap_or_else(|| info.name.clone()))
        }
        None => (PatState::Blank, c.name.clone().unwrap_or_default()),
    };
    // Pasted tracks: the pattern is at least as used as what they came from.
    for s in c.tracks.values().chain(c.sounds.values()).flatten() {
        let st = p.info[*s as usize].state;
        if rank(st) > rank(state) {
            state = st;
        }
    }
    (state, name)
}

/// Track `t` of a slot, with pasted tracks applied; `blank` for a track of
/// an empty pattern.
fn seq_of<'a>(p: &'a Patterns, c: &Cell, t: usize, blank: &'a TrackSeq) -> &'a TrackSeq {
    c.track_src(t).and_then(|s| p.info[s as usize].seq.get(t)).unwrap_or(blank)
}

/// "TRACK 3" / "MIDI 2" (DT1 tracks past the audio ones).
fn track_name(t: usize, audio: usize) -> String {
    if t < audio { format!("TRACK {}", t + 1) } else { format!("MIDI {}", t - audio + 1) }
}

impl ProjEd {
    /// Copied track sounds (track, pattern as read; `None`: an empty
    /// pattern), for the Sounds page's paste. Empty when none are copied.
    pub fn copied_sounds(&self) -> Vec<(u8, Option<u8>)> {
        self.tclip_snd.as_ref().map(|c| c.tracks.clone()).unwrap_or_default()
    }

    fn has_tclip(&self) -> bool {
        self.tclip_ev.is_some() || self.tclip_snd.is_some()
    }

    /// Whether `part` has something copied to paste.
    fn can_paste(&self, part: Part) -> bool {
        match part {
            Part::Track => self.tclip_ev.is_some() && self.tclip_snd.is_some(),
            Part::Sound => self.tclip_snd.is_some(),
            Part::Events => self.tclip_ev.is_some(),
        }
    }

    /// The copied parts `part` pastes (`Track`: whatever is copied).
    fn clips(&self, part: Part) -> (Option<&TrackClip>, Option<&TrackClip>) {
        (self.tclip_ev.as_ref().filter(|_| part.events()), self.tclip_snd.as_ref().filter(|_| part.sound()))
    }
}

/// Copied tracks, for menus and status lines: "TRACK 3 of A01".
fn clip_desc(c: &TrackClip, audio: usize) -> String {
    match c.tracks.as_slice() {
        [(t, _)] => format!("{} of {}", track_name(*t as usize, audio), patterns::label(c.from)),
        t => format!("{} tracks of {}", t.len(), patterns::label(c.from)),
    }
}

/// What pasting `part` would paste: "TRACK 3 of A01", "sound of …", "events of … + sound of …".
fn tclip_desc(ed: &ProjEd, part: Part, audio: usize) -> String {
    match ed.clips(part) {
        (Some(e), Some(s)) if e.tracks == s.tracks && e.from == s.from => clip_desc(e, audio),
        (Some(e), Some(s)) => format!("events of {} + sound of {}", clip_desc(e, audio), clip_desc(s, audio)),
        (Some(e), None) => format!("events of {}", clip_desc(e, audio)),
        (None, Some(s)) => format!("sound of {}", clip_desc(s, audio)),
        (None, None) => "tracks".into(),
    }
}

fn copy_tracks(ed: &mut ProjEd, part: Part, audio: usize) -> String {
    let Some(i) = ed.tsel_pat.filter(|_| !ed.tsel.is_empty()) else {
        return "Select tracks first (click a track name in a folded-out pattern)".into();
    };
    let c = &ed.cells[i];
    let ev = TrackClip { tracks: ed.tsel.iter().map(|&t| (t, c.track_src(t as usize))).collect(), from: i };
    let snd = TrackClip { tracks: ed.tsel.iter().map(|&t| (t, c.sound_src(t as usize))).collect(), from: i };
    match part {
        Part::Track => (ed.tclip_ev, ed.tclip_snd) = (Some(ev), Some(snd)),
        Part::Sound => ed.tclip_snd = Some(snd),
        Part::Events => (ed.tclip_ev, ed.tclip_snd) = (Some(ev), None),
    }
    ed.clip.clear();
    format!("Copied {}; select a pattern and paste (⌘V or right-click)", tclip_desc(ed, part, audio))
}

/// Paste the copied `part` into `into` (same track numbers). Asks first
/// when pasting events would overwrite tracks with something on them.
fn paste_tracks(ed: &mut ProjEd, p: &Patterns, into: Vec<usize>, part: Part, audio: usize) -> String {
    let (ev, snd) = ed.clips(part);
    if ev.is_none() && snd.is_none() {
        return match part {
            Part::Track => "No tracks copied yet",
            Part::Sound => "No track sound copied yet",
            Part::Events => "No track events copied yet",
        }
        .into();
    }
    if into.is_empty() {
        return "Select a pattern to paste the tracks into".into();
    }
    if let Some(why) = p.track_copy_blocked() {
        return format!("Can't paste tracks: {why}");
    }
    if ev.into_iter().chain(snd).flat_map(|c| &c.tracks).any(|(_, s)| s.is_none()) && !p.can_clear() {
        return "Can't paste an empty pattern's track: no empty pattern found in this project to copy (see the log)".into();
    }
    let blank = blank_seq(p);
    let mut lost = Vec::new();
    for &i in &into {
        let c = &ed.cells[i];
        for &(t, from) in ev.iter().flat_map(|c| &c.tracks) {
            if c.track_src(t as usize) == from {
                continue;
            }
            let seq = seq_of(p, c, t as usize, &blank);
            let trigs = seq.steps.iter().filter(|s| **s == Step::Note).count();
            let locks = seq.steps.iter().filter(|s| **s == Step::Lock).count();
            let what = match (trigs, locks) {
                (0, 0) if seq.locks.is_empty() => continue,
                (0, 0) => "p-locks".to_owned(),
                (n, 0) => format!("{n} trig(s)"),
                (0, l) => format!("{l} lock trig(s)"),
                (n, l) => format!("{n} trig(s), {l} lock trig(s)"),
            };
            lost.push(format!("{} {}: {what}", patterns::label(i), track_name(t as usize, audio)));
        }
    }
    if lost.is_empty() {
        apply_tracks(ed, &into, part, audio)
    } else {
        let n = lost.len();
        ed.confirm = Some(TrackPaste { into, part, lost });
        format!("Pasting would overwrite {n} track(s) with trigs on them: confirm or cancel")
    }
}

fn apply_tracks(ed: &mut ProjEd, into: &[usize], part: Part, audio: usize) -> String {
    let (ev, snd) = ed.clips(part);
    let (ev, snd) = (ev.cloned(), snd.cloned());
    ed.edit(|cells| {
        for &i in into {
            for &(t, from) in ev.iter().flat_map(|c| &c.tracks) {
                cells[i].set_track(t as usize, from);
            }
            for &(t, from) in snd.iter().flat_map(|c| &c.tracks) {
                cells[i].set_sound(t as usize, from);
            }
        }
    });
    let at = match into {
        [i] => patterns::label(*i),
        _ => format!("{} patterns", into.len()),
    };
    format!("Pasted {} into {at}", tclip_desc(ed, part, audio))
}

/// The Copy track / sound / events items of a track's menu.
fn copy_items(ui: &mut egui::Ui, ed: &mut ProjEd, audio: usize) -> Option<String> {
    let mut status = None;
    for (part, text) in [(Part::Track, "Copy track  ⌘C"), (Part::Sound, "Copy track sound"), (Part::Events, "Copy track events")] {
        let tip = match part {
            Part::Track => "Its trigs, p-locks and sound",
            Part::Sound => "The kit sound (sample, parameters); copied events stay",
            Part::Events => "Trigs, length, scale and p-locks; drops a copied sound",
        };
        if ui.button(text).on_hover_text(tip).clicked() {
            let msg = copy_tracks(ed, part, audio);
            // ⌘V only arrives when the system clipboard holds text.
            ui.ctx().copy_text(msg.clone());
            status = Some(msg);
            ui.close();
        }
    }
    status
}

/// The Paste track / sound / events items, each greyed out until copied.
fn paste_items(ui: &mut egui::Ui, ed: &mut ProjEd, p: &Patterns, into: &[usize], editable: bool, audio: usize) -> Option<String> {
    let mut status = None;
    for (part, text) in [(Part::Track, "Paste track"), (Part::Sound, "Paste track sound"), (Part::Events, "Paste track events")] {
        let ok = ed.can_paste(part);
        let label = if part == Part::Track { format!("{text}  ⌘V") } else { text.to_owned() };
        let tip = if ok { format!("Paste {} into the same track numbers", tclip_desc(ed, part, audio)) } else { "Nothing copied for this yet".to_owned() };
        if ui.add_enabled(editable && ok, egui::Button::new(label)).on_hover_text(tip.clone()).on_disabled_hover_text(tip).clicked() {
            status = Some(paste_tracks(ed, p, into.to_vec(), part, audio));
            ui.close();
        }
    }
    status
}

/// An empty track, for tracks of an empty pattern.
fn blank_seq(p: &Patterns) -> TrackSeq {
    let n = p.info.iter().find_map(|i| i.seq.first()).map_or(64, |s| s.steps.len());
    TrackSeq {
        steps: vec![Step::None; n],
        len: 16,
        len_known: false,
        scale: None,
        tail: Vec::new(),
        flags: vec![0; n],
        arrays: Vec::new(),
        locks: Vec::new(),
        sample_param: None,
    }
}

/// Asks before a track paste overwrites tracks with trigs. `Some(true)`: go ahead.
fn confirm_tracks(ctx: &egui::Context, c: &TrackPaste) -> Option<bool> {
    let mut answer = None;
    egui::Modal::new(egui::Id::unique("elek_track_paste_modal")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.heading(format!("Overwrite {} track(s)?", c.lost.len()));
        ui.add_space(6.0);
        ui.label("These tracks have something on them that the paste replaces (with their sound and p-locks):");
        ui.add_space(4.0);
        egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
            for l in &c.lost {
                ui.label(RichText::new(l).font(oled_font(12.0)));
            }
        });
        ui.add_space(4.0);
        ui.label(RichText::new("Nothing is sent until you push; ⌘Z undoes the paste.").weak());
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(RichText::new("Overwrite").color(theme::DEVICE_ACCENT_BRIGHT)).clicked() {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                answer = Some(false);
            }
        });
    });
    answer
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
    for set in [&mut ed.open, &mut ed.snd_open] {
        let was: BTreeSet<usize> = set.clone();
        for (s, d) in src.iter().zip(&dst) {
            if was.contains(s) {
                set.insert(*d);
            } else {
                set.remove(d);
            }
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
            cells[i] = Cell::empty();
        }
    });
    format!("Cleared {} slot(s), {used} of them had something in them", t.len())
}

fn copy(ed: &mut ProjEd) -> String {
    let t = ed.targets();
    ed.clip = t.iter().map(|&i| ed.cells[i].clone()).collect();
    ed.tclip_ev = None;
    ed.tclip_snd = None;
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
    let audio = st.project.as_ref().map_or(8, |p| p.model.kit_tracks());
    if let Some(c) = &st.proj.confirm {
        if let Some(yes) = confirm_tracks(ui.ctx(), c) {
            let c = st.proj.confirm.take().expect("checked above");
            st.status = if yes && !busy { apply_tracks(&mut st.proj, &c.into, c.part, audio) } else { "Paste cancelled".into() };
        }
    }
    keys(ui.ctx(), st);
    action_bar(ui, st, busy);
    ui.label(
        RichText::new("Click / ⇧click / ⌘click to select · drag to move (swaps with what's there) · ⌥ while dropping copies · ⌘C / ⌘V · click a track name in a folded-out row to select tracks (⌘ / ⇧ for more), ⌘C, then ⌘V or right-click a pattern to paste them in · ⌫ clears · ⏎ or double-click renames · ←/→ fold · renaming sets the pattern name and the kit name · patterns in songs aren't updated when moved")
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
    if ctx.egui_wants_keyboard_input() || st.edit_locked() || st.proj.confirm.is_some() {
        return;
    }
    let audio = st.project.as_ref().map_or(8, |p| p.model.kit_tracks());
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
    let tracks_on = !ed.tsel.is_empty();
    if esc {
        ed.sel.clear();
        ed.clear_tracks();
    }
    // With tracks selected, ⌫ and ⏎ would hit the whole pattern: leave them.
    if del && !tracks_on && !ed.targets().is_empty() {
        status = Some(clear(ed, p));
    }
    if ev_copy || (m.command && c) {
        let msg = if tracks_on { copy_tracks(ed, Part::Track, audio) } else { copy(ed) };
        // ⌘V only arrives when the system clipboard holds text.
        ctx.copy_text(msg.clone());
        status = Some(msg);
    }
    if ev_paste || (m.command && v) {
        status = Some(if !ed.has_tclip() {
            paste(ed)
        } else {
            let into = ed.track_targets();
            paste_tracks(ed, p, into, Part::Track, audio)
        });
    }
    if enter && !tracks_on {
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
    let audio = st.project.as_ref().map_or(8, |p| p.model.kit_tracks());
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
        if ed.has_tclip() {
            let into = ed.track_targets();
            ui.add_enabled_ui(editable, |ui| {
                ui.menu_button("Paste tracks ▾", |ui| {
                    if let Some(s) = paste_items(ui, ed, p, &into, editable, audio) {
                        status = Some(s);
                    }
                })
                .response
                .on_hover_text("Paste copied tracks into the selected pattern(s)");
            });
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
fn shown_tracks(n: usize, audio: usize, empty: impl Fn(usize) -> bool) -> Vec<(String, usize)> {
    let mut v: Vec<(String, usize)> = (0..audio.min(n)).map(|t| (track_name(t, audio), t)).collect();
    v.extend((audio..n).filter(|&t| !empty(t)).map(|t| (track_name(t, audio), t)));
    v
}

fn list(ui: &mut egui::Ui, st: &mut ElekState, busy: bool) {
    let Some(proj) = st.project.as_ref() else { return };
    let Ok(p) = proj.patterns.as_deref() else { return };
    let editable = p.read_only.is_none() && !busy;
    // Audio tracks: 8 on the DT1 (then 8 MIDI tracks), 16 on the DT2.
    let audio = proj.model.kit_tracks();
    let show_defaults = st.lib.show_defaults;
    let ed = &mut st.proj;
    // The sound pool as read, to point kit track sounds at the pool sound
    // they match (same name and sample); only while a SOUNDS foldout is open.
    let pool_snd: Vec<PoolSound> = if ed.snd_open.is_empty() { Vec::new() } else { proj.pool_sounds().unwrap_or_default() };
    let mut snd_toggles = Vec::new();
    // A SOUNDS foldout line's Copy: (pattern slot, track).
    let mut snd_copy: Option<(usize, usize)> = None;
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

    let blank = blank_seq(p);
    let mut status = None;
    let mut rows = Vec::new();
    let mut toggles = Vec::new();
    let mut track_click: Option<(usize, u8, Vec<u8>)> = None;
    let mut bank_toggles = Vec::new();
    let mut inline_done: Option<(usize, Option<String>)> = None;

    let frame = egui::Frame::new().fill(OLED_BG).corner_radius(2.0).inner_margin(4.0);
    frame.show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        theme::always_show_scrollbar(ui);
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
                ui.painter().text(egui::pos2(r.left() + 100.0, r.center().y), egui::Align2::LEFT_CENTER, note, oled_font(10.0), if changed > 0 { theme::DEVICE_ACCENT_BRIGHT } else { OLED_DIM });
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

                let cell = ed.cells[i].clone();
                let cell = &cell;
                let (state, name) = view(p, cell);
                // Pattern-level things (and the track layout) come from the
                // pattern itself, else from where its pasted tracks came from.
                let info = cell.src.or_else(|| cell.tracks.values().chain(cell.sounds.values()).flatten().next().copied()).map(|s| &p.info[s as usize]);
                let open = ed.open.contains(&i);
                let tracks = info.map(|n| shown_tracks(n.seq.len(), audio, |t| seq_of(p, cell, t, &blank).is_empty())).unwrap_or_default();
                let mut step_hover: Option<String> = None;
                let byte_rows = if ed.bytes && info.is_some() { 2.0 } else { 0.0 };
                // Kit tracks with a decoded sound, for the SOUNDS foldout under the tracks.
                let kit_n = info.map_or(0, |n| if tracks.is_empty() { 0 } else { n.tracks.len().min(audio) });
                let snd_open = ed.snd_open.contains(&i);
                let snd_rows = if kit_n == 0 { 0.0 } else { 1.0 + if snd_open { kit_n as f32 } else { 0.0 } };
                let extra = if open { 4.0 + (tracks.len().max(1) as f32 + byte_rows + snd_rows) * TRACK_H + 4.0 } else { 0.0 };
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
                let fill = if resp.hovered() {
                    OLED_HOVER
                } else {
                    ROW_FILL
                };
                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, 2.0, fill);
                let changed = *cell != Cell::as_read(i);
                if landing.contains(&i) {
                    painter.rect_stroke(rect, 2.0, Stroke::new(2.0, theme::DEVICE_ACCENT_BRIGHT), egui::StrokeKind::Inside);
                } else if selected {
                    painter.rect_stroke(rect, 2.0, Stroke::new(1.0, SEL_EDGE), egui::StrokeKind::Inside);
                } else if ed.cursor == Some(i) {
                    painter.rect_stroke(rect, 2.0, Stroke::new(1.0, OLED_DIM), egui::StrokeKind::Inside);
                }
                if changed {
                    painter.rect_filled(Rect::from_min_size(rect.left_top(), egui::vec2(3.0, rect.height())), 1.0, theme::DEVICE_ACCENT);
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
                let pasted_n = cell.pasted_tracks().len();
                let pasted = (pasted_n > 0).then(|| format!("+{pasted_n} trk"));
                for bt in badge.into_iter().chain(pasted).chain(cell.name.is_some().then(|| "✎".to_owned())) {
                    let r = painter.text(egui::pos2(bx, y), egui::Align2::LEFT_CENTER, bt, oled_font(11.0), theme::DEVICE_ACCENT_BRIGHT);
                    bx = r.right() + 6.0;
                }

                // Folded: a tiny picture of the audio tracks. Folded out: one
                // dot line per track.
                if let Some(n) = info {
                    let dots_x = x0 + DOTS_X;
                    if !open && state != PatState::Blank {
                        let tr: Vec<&TrackSeq> = (0..audio.min(n.seq.len())).map(|t| seq_of(p, cell, t, &blank)).collect();
                        mini(&painter, egui::pos2(dots_x, head.top() + 3.0), head.height() - 6.0, &tr);
                        let longest = tr.iter().map(|t| t.len).max().unwrap_or(0);
                        let notes: usize = tr.iter().map(|t| t.notes()).sum();
                        if !tr.is_empty() {
                            let right = dots_x + n.seq[0].steps.len() as f32 * 2.0 + 10.0;
                            // One bar (16 steps) at 1x is the default: say nothing.
                            let mut parts: Vec<String> = Vec::new();
                            if longest != 16 {
                                parts.push(len_label(longest));
                            }
                            if let Some(sc) = pattern_scale(n) {
                                parts.push(sc);
                            }
                            parts.push(format!("{notes} trigs"));
                            let r = painter.text(egui::pos2(right, y), egui::Align2::LEFT_CENTER, parts.join(" · "), oled_font(10.0), OLED_DIM);
                            if pointer.is_some_and(|pp| r.expand(3.0).contains(pp)) {
                                step_hover = Some(timing_tip(&patterns::label(i), n, &tr));
                            }
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
                        let shown: Vec<u8> = tracks.iter().map(|(_, t)| *t as u8).collect();
                        for (label, t) in &tracks {
                            let cy = ty + TRACK_H / 2.0;
                            let tt = *t as u8;
                            let from = cell.tracks.get(&tt).copied();
                            let snd_from = cell.sounds.get(&tt).copied();
                            let line = Rect::from_min_max(egui::pos2(x0 + 18.0, ty), egui::pos2(rect.right() - 4.0, ty + TRACK_H));
                            let t_selected = ed.tsel_pat == Some(i) && ed.tsel.contains(&tt);
                            if t_selected {
                                painter.rect_filled(line, 2.0, TRACK_SEL);
                            }
                            // The name and sample part of the line selects the track.
                            let hit = Rect::from_min_max(line.min, egui::pos2(x0 + DOTS_X - 8.0, line.max.y));
                            let tresp = ui.interact(hit, ui.scope_id().with(("ptrack", i, tt)), Sense::click());
                            let label_col = if from.is_some() || snd_from.is_some() {
                                theme::DEVICE_ACCENT_BRIGHT
                            } else if t_selected || tresp.hovered() {
                                OLED
                            } else {
                                OLED_DIM
                            };
                            painter.text(egui::pos2(x0 + 24.0, cy), egui::Align2::LEFT_CENTER, label, oled_font(11.0), label_col);
                            let src_name = |f: Option<u8>| f.map_or("an empty pattern".to_owned(), |s| patterns::label(s as usize));
                            let tip = match (from, snd_from) {
                                (Some(e), Some(s)) if e == s => format!("{label}: pasted from {}\n", src_name(e)),
                                (e, s) => {
                                    let mut t = String::new();
                                    if let Some(e) = e {
                                        t += &format!("{label}: events pasted from {}\n", src_name(e));
                                    }
                                    if let Some(s) = s {
                                        t += &format!("{label}: sound pasted from {}\n", src_name(s));
                                    }
                                    t
                                }
                            };
                            let tresp = tresp.on_hover_text(format!("{tip}Click to select (⌘ / ⇧ for more) · right-click to copy or paste tracks"));
                            if tresp.clicked() {
                                track_click = Some((i, tt, shown.clone()));
                            }
                            tresp.context_menu(|ui| {
                                if !t_selected {
                                    ed.click_track(i, tt, &shown, egui::Modifiers::NONE);
                                }
                                let what = match ed.tsel.len() {
                                    1 => label.clone(),
                                    n => format!("{n} tracks"),
                                };
                                ui.menu_button(format!("Copy {what}"), |ui| {
                                    if let Some(s) = copy_items(ui, ed, audio) {
                                        status = Some(s);
                                    }
                                });
                                ui.add_enabled_ui(editable && ed.has_tclip(), |ui| {
                                    ui.menu_button("Paste here", |ui| {
                                        if let Some(s) = paste_items(ui, ed, p, &[i], editable, audio) {
                                            status = Some(s);
                                        }
                                    });
                                });
                            });
                            let ti = cell.sound_src(*t).and_then(|s| p.info[s as usize].tracks.get(*t));
                            if let Some(ti) = ti {
                                // The sample, then the sound's own name when it isn't the default.
                                let mut text = sample_name(ti.slot);
                                if ti.name != format!("SOUND {}", t + 1) && !ti.name.is_empty() {
                                    text = format!("{text}  ·  {}", ti.name);
                                }
                                let sr = Rect::from_min_size(egui::pos2(x0 + NAME_X, ty), egui::vec2(DOTS_X - NAME_X - 56.0, TRACK_H));
                                painter.with_clip_rect(sr).text(sr.left_center(), egui::Align2::LEFT_CENTER, text, egui::FontId::proportional(12.0), OLED);
                            }
                            let seq = seq_of(p, cell, *t, &blank);
                            // Track length in steps (16 = one bar); ≈ when
                            // guessed from the last used step.
                            // 16 steps is the default: only other lengths are shown.
                            let mut len = match (seq.len, seq.len_known) {
                                (16, _) => String::new(),
                                (n, true) => n.to_string(),
                                (n, false) => format!("≈{n}"),
                            };
                            if let Some(sc) = seq.scale.filter(|&s| s != 2).and_then(patterns::scale_label) {
                                len = if len.is_empty() { sc.to_owned() } else { format!("{sc}  {len}") };
                            }
                            painter.text(egui::pos2(x0 + DOTS_X - 10.0, cy), egui::Align2::RIGHT_CENTER, len, oled_font(10.0), OLED_DIM);
                            if ed.bytes {
                                let hx: Vec<String> = seq.tail.iter().map(|b| format!("{b:02x}")).collect();
                                hex_row(&painter, egui::pos2(dots_x, cy), &hx, OLED, Some(&seq.tail));
                            } else {
                                let max_w = (rect.right() - dots_x - 10.0).max(120.0);
                                dots(&painter, egui::pos2(dots_x, cy), max_w, seq);
                                if let Some(pp) = pointer.filter(|pp| (pp.y - cy).abs() <= TRACK_H / 2.0 && rect.contains(*pp)) {
                                    let hit = nearest_step(seq.steps.len(), dots_x, max_w, pp.x, |k| k < seq.len && seq.steps[k] != Step::None);
                                    if let Some((k, sx)) = hit {
                                        painter.circle_stroke(egui::pos2(sx, cy), 4.5, egui::Stroke::new(1.0, theme::DEVICE_ACCENT_BRIGHT));
                                        step_hover = Some(step_tip(seq, k, label, &sample_name));
                                    }
                                }
                            }
                            ty += TRACK_H;
                        }
                        if kit_n > 0 {
                            ty = sound_foldout(SoundFold {
                                ui,
                                painter: &painter,
                                p,
                                cell,
                                i,
                                x0,
                                right: rect.right(),
                                ty,
                                kit_n,
                                open: snd_open,
                                show_defaults,
                                pool: &pool_snd,
                                pointer,
                                hover: &mut step_hover,
                                toggle: &mut snd_toggles,
                                copy: &mut snd_copy,
                            });
                            let _ = ty;
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
                    ed.clear_tracks();
                    if !ed.sel.contains(&i) {
                        ed.select_only(i);
                    }
                    ed.drag = Some(i);
                }
                if resp.double_clicked() && editable {
                    ed.start_inline(p, i);
                } else if resp.clicked() {
                    ed.clear_tracks();
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
                    if ed.has_tclip() {
                        let into = ed.targets();
                        let at = if into.len() > 1 { format!("Paste tracks into {} patterns", into.len()) } else { "Paste tracks here".to_owned() };
                        ui.add_enabled_ui(editable, |ui| {
                            ui.menu_button(at, |ui| {
                                if let Some(s) = paste_items(ui, ed, p, &into, editable, audio) {
                                    status = Some(s);
                                }
                            });
                        });
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

    if let Some((i, t, shown)) = track_click {
        ed.click_track(i, t, &shown, mods);
    }
    for b in bank_toggles {
        ed.banks[b] = !ed.banks[b];
    }
    for i in toggles {
        if !ed.open.remove(&i) {
            ed.open.insert(i);
        }
    }
    for i in snd_toggles {
        if !ed.snd_open.remove(&i) {
            ed.snd_open.insert(i);
        }
    }
    if let Some((i, t)) = snd_copy {
        let src = ed.cells[i].sound_src(t);
        ed.tclip_snd = Some(TrackClip { tracks: vec![(t as u8, src)], from: i });
        let msg = format!("Copied the sound of {}; paste it into the sound pool on the Sounds page (⌘V on a highlighted sound, or right-click → Paste), or into other patterns", clip_desc(ed.tclip_snd.as_ref().expect("just set"), audio));
        ui.ctx().copy_text(msg.clone());
        status = Some(msg);
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
            ui.painter().text(pt + egui::vec2(14.0, 8.0), egui::Align2::LEFT_TOP, tip, oled_font(12.0), theme::DEVICE_ACCENT_BRIGHT);
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

/// The step accepted by `want` nearest to `x` in a dot line drawn from
/// `x0`, within a reach wider than the dot itself so it's easy to hover.
/// Returns the step and its centre x.
fn nearest_step(n: usize, x0: f32, max_w: f32, x: f32, want: impl Fn(usize) -> bool) -> Option<(usize, f32)> {
    let (pitch, gap) = dot_pitch(n, max_w);
    let reach = (pitch * 2.0).max(14.0);
    (0..n)
        .filter(|&k| want(k))
        .map(|k| (k, x0 + k as f32 * pitch + (k / 16) as f32 * gap + pitch / 2.0))
        .filter(|&(_, cx)| (x - cx).abs() <= reach)
        .min_by(|a, b| (x - a.1).abs().total_cmp(&(x - b.1).abs()))
}

/// What's on step `k`: note-ish bytes, flags, p-locks.
/// A track or pattern length: "2 bar" for whole bars past one, else steps.
fn len_label(n: usize) -> String {
    if n > 16 && n % 16 == 0 { format!("{} bar", n / 16) } else { format!("{n} steps") }
}

/// The scales other than 1x in use (master and tracks), fastest first,
/// e.g. "2x 1/2x"; `None` when everything runs at 1x.
fn pattern_scale(n: &patterns::PatternInfo) -> Option<String> {
    let used: BTreeSet<u8> = n.timing.map(|t| t.scale).into_iter().chain(n.seq.iter().filter_map(|t| t.scale)).filter(|&s| s != 2).collect();
    let labels: Vec<&str> = used.into_iter().filter_map(patterns::scale_label).collect();
    (!labels.is_empty()).then(|| labels.join(" "))
}

/// Hover text for the folded length / scale summary.
fn timing_tip(label: &str, n: &patterns::PatternInfo, tr: &[&TrackSeq]) -> String {
    let mut out = vec![label.to_owned()];
    if let Some(t) = n.timing {
        let len = match t.master_len {
            1 => "INF".to_owned(),
            l => format!("{l} steps{}", if l % 16 == 0 && l > 16 { format!(" ({})", len_label(l as usize)) } else { String::new() }),
        };
        out.push(format!("scale mode: {}", if t.per_track { "per track" } else { "normal" }));
        out.push(format!("reset (master length): {len}"));
        out.push(format!("change: {}", if t.change <= 1 { "OFF".to_owned() } else { t.change.to_string() }));
        out.push(format!("master scale: {}", patterns::scale_label(t.scale).unwrap_or("?")));
        out.push(format!("tempo: {:.1} BPM", t.tempo_x120 as f32 / 120.0));
    } else {
        out.push("master length / change / scale: layout unknown for this version".to_owned());
    }
    // Per track, grouped when several share a length and scale.
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (k, t) in tr.iter().enumerate() {
        let len = if t.len_known { t.len.to_string() } else { format!("≈{}", t.len) };
        let sc = t.scale.and_then(patterns::scale_label).map_or(String::new(), |s| format!(" at {s}"));
        let key = format!("{len} steps{sc}");
        match groups.iter_mut().find(|g| g.0 == key) {
            Some(g) => g.1.push(k + 1),
            None => groups.push((key, vec![k + 1])),
        }
    }
    if groups.len() == 1 {
        out.push(format!("all tracks: {}", groups[0].0));
    } else {
        for (key, ts) in groups {
            let names: Vec<String> = ts.iter().map(|t| t.to_string()).collect();
            out.push(format!("track {}: {key}", names.join(", ")));
        }
    }
    out.join("\n")
}

/// Where the SOUNDS foldout's columns start, from the row's left edge.
const SND_NAME_X: f32 = 44.0;
const SND_POOL_X: f32 = 160.0;
const SND_MACH_X: f32 = 200.0;
const SND_SRC_X: f32 = 274.0;
const SND_SRC_W: f32 = 62.0;
/// A kit track sound value that differs from the pool sound it matches.
const SND_DIFFERS: Color32 = theme::CHANGED;

/// What the SOUNDS foldout of one folded-out pattern row needs.
struct SoundFold<'a, 'b> {
    ui: &'a egui::Ui,
    painter: &'a egui::Painter,
    p: &'a Patterns,
    cell: &'a Cell,
    /// The pattern slot.
    i: usize,
    x0: f32,
    right: f32,
    ty: f32,
    /// Kit (audio) tracks.
    kit_n: usize,
    open: bool,
    show_defaults: bool,
    /// The sound pool as read (empty when not read).
    pool: &'a [PoolSound],
    pointer: Option<egui::Pos2>,
    hover: &'b mut Option<String>,
    toggle: &'b mut Vec<usize>,
    /// Set to (pattern slot, track) when a sound line's Copy is picked.
    copy: &'b mut Option<(usize, usize)>,
}

/// The SOUNDS line under a folded-out pattern's tracks and, when open, each
/// kit track's sound decoded as on the Sounds page: name, the pool sound it
/// matches (same name and sample), machine and SRC page. Values that differ
/// from that pool sound are amber. Returns the y below it.
fn sound_foldout(f: SoundFold) -> f32 {
    let SoundFold { ui, painter, p, cell, i, x0, right, mut ty, kit_n, open, show_defaults, pool, pointer, hover, toggle, copy } = f;
    let head = Rect::from_min_max(egui::pos2(x0 + 18.0, ty), egui::pos2(x0 + SND_SRC_X, ty + TRACK_H));
    let r = ui.interact(head, ui.scope_id().with(("psnd", i)), Sense::click());
    let col = if r.hovered() || open { OLED } else { OLED_DIM };
    triangle(painter, egui::pos2(x0 + 30.0, head.center().y), open, col);
    painter.text(egui::pos2(x0 + SND_NAME_X, head.center().y), egui::Align2::LEFT_CENTER, "SOUNDS", oled_font(11.0), col);
    if r.on_hover_text("Each kit track's sound: machine and SRC page, decoded like the Sounds page (some values may still read wrong). POOL: the pool sound with the same name and sample; amber values differ from it").clicked() {
        toggle.push(i);
    }
    ty += TRACK_H;
    if !open {
        return ty;
    }
    for t in 0..kit_n {
        let cy = ty + TRACK_H / 2.0;
        let line = Rect::from_min_max(egui::pos2(x0 + 18.0, ty), egui::pos2(right - 4.0, ty + TRACK_H));
        let Some(s) = cell.sound_src(t).and_then(|src| p.info[src as usize].tracks.get(t)).map(|ti| &ti.sound) else {
            painter.text(egui::pos2(x0 + 24.0, cy), egui::Align2::LEFT_CENTER, format!("T{}", t + 1), oled_font(10.0), OLED_DIM);
            painter.text(egui::pos2(x0 + SND_NAME_X, cy), egui::Align2::LEFT_CENTER, "—", oled_font(11.0), OLED_DIM);
            ty += TRACK_H;
            continue;
        };
        let pasted = cell.sounds.contains_key(&(t as u8));
        // Right-click: copy this sound (for the sound pool or other patterns).
        let lr = ui.interact(line, ui.scope_id().with(("psnd_line", i, t)), Sense::click());
        lr.context_menu(|ui| {
            let nm = if s.name.is_empty() { "—" } else { s.name.as_str() };
            ui.label(RichText::new(format!("T{} sound “{nm}” of {}", t + 1, patterns::label(i))).weak());
            if ui.button("Copy sound").on_hover_text("Paste it into the sound pool on the Sounds page (⌘V or right-click a sound), or into the same track of other patterns").clicked() {
                *copy = Some((i, t));
                ui.close();
            }
        });
        painter.text(egui::pos2(x0 + 24.0, cy), egui::Align2::LEFT_CENTER, format!("T{}", t + 1), oled_font(10.0), if pasted { theme::DEVICE_ACCENT_BRIGHT } else { OLED_DIM });
        let name = if s.name.is_empty() { "—" } else { s.name.as_str() };
        let nr = Rect::from_min_size(egui::pos2(x0 + SND_NAME_X, ty), egui::vec2(SND_POOL_X - SND_NAME_X - 4.0, TRACK_H));
        painter.with_clip_rect(nr).text(nr.left_center(), egui::Align2::LEFT_CENTER, name, oled_font(12.0), OLED);
        // The pool sound this track was probably loaded from.
        let from_pool = (!s.name.trim().is_empty()).then(|| pool.iter().find(|q| !q.blank && q.name == s.name && q.slot == s.slot)).flatten();
        let pool_txt = from_pool.map_or_else(|| "—".to_owned(), |q| format!("{:03}", q.index + 1));
        painter.text(egui::pos2(x0 + SND_POOL_X, cy), egui::Align2::LEFT_CENTER, pool_txt, oled_font(10.0), OLED_DIM);
        let machine_differs = from_pool.is_some_and(|q| q.machine.as_ref().map(|m| m.0) != s.machine.as_ref().map(|m| m.0));
        let mr = Rect::from_min_size(egui::pos2(x0 + SND_MACH_X, ty), egui::vec2(SND_SRC_X - SND_MACH_X - 4.0, TRACK_H));
        painter.with_clip_rect(mr).text(mr.left_center(), egui::Align2::LEFT_CENTER, machine_text(s), oled_font(11.0), if machine_differs { SND_DIFFERS } else { OLED });
        let mut diffs: Vec<String> = Vec::new();
        for (k, place) in SRC_SHOWN.iter().enumerate() {
            let Some(q) = param_at(s, *place) else { continue };
            let pool_q = from_pool.and_then(|ps| param_at(ps, *place));
            let differs = pool_q.is_some_and(|pq| pq.raw != q.raw);
            if differs {
                diffs.push(format!("{} {} (pool {})", q.name, q.text, pool_q.map_or("?", |pq| pq.text.as_str())));
            }
            if !show_defaults && is_default(q) && !differs {
                continue;
            }
            let x = x0 + SND_SRC_X + k as f32 * SND_SRC_W;
            if x + SND_SRC_W > right {
                break;
            }
            let cr = Rect::from_min_size(egui::pos2(x, ty), egui::vec2(SND_SRC_W - 4.0, TRACK_H));
            let cp = painter.with_clip_rect(cr);
            let mut tx = cr.left();
            if q.name != "PLAY" {
                tx = cp.text(egui::pos2(tx, cy), egui::Align2::LEFT_CENTER, format!("{} ", q.name), oled_font(9.0), OLED_DIM).right();
            }
            cp.text(egui::pos2(tx, cy), egui::Align2::LEFT_CENTER, &q.text, oled_font(11.0), if differs { SND_DIFFERS } else { OLED });
        }
        if pointer.is_some_and(|pp| line.contains(pp)) {
            painter.rect_stroke(line, 2.0, Stroke::new(1.0, OLED_DIM), egui::StrokeKind::Inside);
            let mut tip = format!("T{} “{name}”: {}", t + 1, src_detail(s));
            match from_pool {
                Some(q) if diffs.is_empty() && !machine_differs => tip += &format!("\n\nSame SRC page as pool sound {:03}", q.index + 1),
                Some(q) => {
                    tip += &format!("\n\nDiffers from pool sound {:03}:", q.index + 1);
                    if machine_differs {
                        tip += &format!("\n  machine (pool {})", machine_text(q));
                    }
                    for d in &diffs {
                        tip += &format!("\n  {d}");
                    }
                }
                None => tip += "\n\nNo pool sound with this name and sample",
            }
            if pasted {
                tip += "\n(sound pasted from another pattern)";
            }
            tip += "\n\nRight-click: copy this sound (to paste into the sound pool)";
            *hover = Some(tip);
        }
        ty += TRACK_H;
    }
    ty
}

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
