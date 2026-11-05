//! Undo / redo: snapshots of the edit state, with key-repeat runs folded into one step.

use super::*;

/// One undo step: the trim, playhead, gain and crop. Prefs (loop, snap...)
/// are deliberately left out.
#[derive(Clone)]
pub(super) struct EditState {
    sel: (usize, usize),
    playhead: usize,
    gain_db: f32,
    crop: Option<(usize, usize)>,
    /// The (possibly cropped) sample that goes with `crop`. Cheap: Arcs.
    shown: Option<Loaded>,
    /// The whole edited file and its content revision (cut / paste).
    full: Option<Loaded>,
    rev: u64,
    markers: Vec<usize>,
}

pub(super) const CH_IN: u8 = 1;
pub(super) const CH_OUT: u8 = 2;
pub(super) const CH_PLAYHEAD: u8 = 4;
pub(super) const CH_GAIN: u8 = 8;
pub(super) const CH_CROP: u8 = 16;
pub(super) const CH_AUDIO: u8 = 32;
pub(super) const CH_MARKS: u8 = 64;

impl EditState {
    /// Which fields differ, as a CH_* mask.
    pub(super) fn diff(&self, o: &EditState) -> u8 {
        let mut m = 0;
        if self.sel.0 != o.sel.0 {
            m |= CH_IN;
        }
        if self.sel.1 != o.sel.1 {
            m |= CH_OUT;
        }
        if self.playhead != o.playhead {
            m |= CH_PLAYHEAD;
        }
        if self.gain_db.to_bits() != o.gain_db.to_bits() {
            m |= CH_GAIN;
        }
        if self.crop != o.crop {
            m |= CH_CROP;
        }
        if self.rev != o.rev {
            m |= CH_AUDIO;
        }
        if self.markers != o.markers {
            m |= CH_MARKS;
        }
        m
    }
}

#[derive(Default)]
pub(super) struct History {
    pub(super) undo: Vec<EditState>,
    pub(super) redo: Vec<EditState>,
    /// State as of the last recorded step; edits are diffed against it.
    pub(super) committed: Option<EditState>,
    /// What changed in the last step and when, to fold key-repeat runs
    /// (holding `]`, nudging with arrows) into one undo step.
    pub(super) last_mask: u8,
    pub(super) last_time: f64,
}

pub(super) const UNDO_LIMIT: usize = 300;
/// Same-kind edits closer together than this merge into one undo step.
pub(super) const COALESCE_SECS: f64 = 0.6;

impl Editor {
    pub(super) fn state(&self) -> EditState {
        EditState {
            sel: self.sel,
            playhead: self.playhead,
            gain_db: self.gain_db,
            crop: self.crop,
            shown: self.current.clone(),
            full: self.full.clone(),
            rev: self.rev,
            markers: self.markers.clone(),
        }
    }

    pub(super) fn restore(&mut self, st: EditState) {
        if st.rev != self.rev {
            self.full = st.full;
            self.rev = st.rev;
            self.current = st.shown;
            self.crop = st.crop;
            self.refresh_file_stats();
        } else if st.crop != self.crop {
            self.current = st.shown;
            self.crop = st.crop;
            self.view.fit_pending = true;
        }
        self.sel = st.sel;
        self.playhead = st.playhead;
        self.gain_db = st.gain_db;
        self.markers = st.markers;
        self.stats_key = None;
    }

    /// Record an undo step if anything changed since the last one. Call once
    /// per frame; pass `busy` while a drag / slider / typed edit is still in
    /// progress so the whole gesture lands as one step when it ends.
    pub fn track_history(&mut self, busy: bool, now: f64) {
        if busy || self.current.is_none() {
            return;
        }
        let now_state = self.state();
        let h = &mut self.history;
        let Some(prev) = h.committed.take() else {
            h.committed = Some(now_state);
            return;
        };
        let mask = prev.diff(&now_state);
        if mask == 0 {
            h.committed = Some(prev);
            return;
        }
        let coalesce = mask == h.last_mask && mask & (CH_CROP | CH_AUDIO | CH_MARKS) == 0 && now - h.last_time < COALESCE_SECS;
        if !coalesce {
            h.undo.push(prev);
            if h.undo.len() > UNDO_LIMIT {
                h.undo.remove(0);
            }
        }
        h.redo.clear();
        h.committed = Some(now_state);
        h.last_mask = mask;
        h.last_time = now;
    }

    pub fn can_undo(&self) -> bool {
        !self.history.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.history.redo.is_empty()
    }

    pub fn undo(&mut self) {
        self.track_history(false, f64::INFINITY);
        let Some(prev) = self.history.undo.pop() else { return };
        self.history.redo.push(self.state());
        self.restore(prev);
        self.history.committed = Some(self.state());
        self.history.last_mask = 0;
    }

    pub fn redo(&mut self) {
        self.track_history(false, f64::INFINITY);
        let Some(next) = self.history.redo.pop() else { return };
        self.history.undo.push(self.state());
        self.restore(next);
        self.history.committed = Some(self.state());
        self.history.last_mask = 0;
    }
}
