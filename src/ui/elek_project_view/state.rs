//! Pattern-page state, selection, undo boundaries and staged row descriptions.

use super::*;

#[derive(Default)]
pub struct ProjEd {
    /// `cells[slot]`: what goes into each slot. Empty while there are no
    /// patterns to edit.
    pub cells: Vec<Cell>,
    pub(super) undo: Vec<Vec<Cell>>,
    pub(super) sel: BTreeSet<usize>,
    pub(super) cursor: Option<usize>,
    pub(super) clip: Vec<Cell>,
    /// Selected tracks (folded-out rows), all in one pattern, and the last
    /// one clicked (for ⇧click).
    pub(super) tsel_pat: Option<usize>,
    pub(super) tsel: BTreeSet<u8>,
    tanchor: Option<u8>,
    /// Copied track events and track sounds. These or `clip` are set; copying
    /// events alone drops the copied sounds.
    pub(super) tclip_ev: Option<TrackClip>,
    pub(super) tclip_snd: Option<TrackClip>,
    /// A pool sound copied on the Sounds page. Copying it drops the copied
    /// tracks and patterns, and copying those drops it.
    pub pool_clip: Option<PoolClip>,
    /// A track paste that would lose trigs, until confirmed.
    pub(super) confirm: Option<TrackPaste>,
    /// Row a drag started on.
    pub(super) drag: Option<usize>,
    /// Name typed for several selected slots, and the selection it was filled for.
    pub(super) name_edit: String,
    pub(super) name_for: Vec<usize>,
    pub(super) focus_name: bool,
    /// Row being renamed in place, its text, and whether to focus it.
    pub(super) inline: Option<usize>,
    pub(super) inline_text: String,
    pub(super) inline_focus: bool,
    /// Folded-out banks (the app remembers them).
    pub banks: [bool; BANKS],
    /// Hide empty slots.
    pub(super) used_only: bool,
    /// Folded-out rows show raw track / pattern bytes instead of steps.
    pub(super) bytes: bool,
    /// Folded-out rows.
    pub(super) open: BTreeSet<usize>,
    /// Folded-out rows whose SOUNDS foldout (kit track sounds) is open too.
    pub(super) snd_open: BTreeSet<usize>,
    /// Each row's rect last frame, and the list's visible area, to find the
    /// row under a drag.
    pub(super) rows: Vec<(usize, Rect)>,
    pub(super) list_rect: Option<Rect>,
    pub(super) scroll_to: Option<usize>,
    /// Asked for from the list, for the app (file dialogs).
    pub(super) want: Vec<ElekIntent>,
    /// Banks whose patterns have all arrived: the only ones edits may touch.
    ready: [bool; BANKS],
    /// Why the last edit was turned down, for the status line.
    pub(super) refused: Option<String>,
}

impl ProjEd {
    pub fn reset(&mut self, p: &ElekProject) {
        let (used_only, banks, bytes) = (self.used_only, self.banks, self.bytes);
        let pool_clip = self.pool_clip.clone().filter(|c| matches!(c.cell.src, SoundSrc::Preset { .. }))
            .map(|mut c| {
                c.track_ok = Err("Paste this preset into the sound pool first to resolve its sample in this project".into());
                c
            });
        *self = Self { used_only, banks, bytes, pool_clip, ..Self::default() };
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
    pub(super) fn edit(&mut self, f: impl FnOnce(&mut Vec<Cell>)) {
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

    pub(super) fn undo(&mut self) {
        if let Some(c) = self.undo.pop() {
            self.cells = c;
        }
    }

    /// The selection, else the cursor.
    pub(super) fn targets(&self) -> Vec<usize> {
        if self.sel.is_empty() {
            self.cursor.into_iter().collect()
        } else {
            self.sel.iter().copied().collect()
        }
    }

    pub(super) fn select_only(&mut self, i: usize) {
        self.clear_tracks();
        self.sel.clear();
        self.sel.insert(i);
        self.cursor = Some(i);
    }

    pub(super) fn clear_tracks(&mut self) {
        self.tsel.clear();
        self.tsel_pat = None;
        self.tanchor = None;
    }

    /// Where a track paste goes: the pattern whose tracks are selected, else
    /// the selected patterns.
    pub(super) fn track_targets(&self) -> Vec<usize> {
        match self.tsel_pat {
            Some(i) if !self.tsel.is_empty() => vec![i],
            _ => self.targets(),
        }
    }

    /// A click on track `t` of pattern `i`; `shown` = the row's track lines.
    pub(super) fn click_track(&mut self, i: usize, t: u8, shown: &[u8], m: egui::Modifiers) {
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

    pub(super) fn start_inline(&mut self, p: &Patterns, i: usize) {
        self.select_only(i);
        self.inline = Some(i);
        self.inline_text = view(p, &self.cells[i]).1;
        self.inline_focus = true;
    }

    /// Rows shown, in slot order.
    pub(super) fn visible(&self, p: &Patterns) -> Vec<usize> {
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
pub(super) fn view(p: &Patterns, c: &Cell) -> (PatState, String) {
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
pub(super) fn seq_of<'a>(p: &'a Patterns, c: &Cell, t: usize, blank: &'a TrackSeq) -> &'a TrackSeq {
    c.track_src(t).and_then(|s| p.info[s as usize].seq.get(t)).unwrap_or(blank)
}

/// "TRACK 3" / "MIDI 2" (DT1 tracks past the audio ones).
pub(super) fn track_name(t: usize, audio: usize) -> String {
    if t < audio { format!("TRACK {}", t + 1) } else { format!("MIDI {}", t - audio + 1) }
}

/// An empty track, for tracks of an empty pattern.
pub(super) fn blank_seq(p: &Patterns) -> TrackSeq {
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

pub(super) fn bank_letter(b: usize) -> char {
    (b'A' + b as u8) as char
}

