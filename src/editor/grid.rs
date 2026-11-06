//! Grid kits: the file is split into 4 / 8 / 16 equal slots, each slice at
//! the start of its slot and padded with silence up to the longest one. A
//! sampler that can only cut a file into equal parts (Digitakt 1) then
//! plays every slice from its real start.
//!
//! The grid isn't stored anywhere: a kit whose markers split it evenly into
//! one of [`GRID_SIZES`] parts is read back as a grid kit.

use super::*;

pub const GRID_SIZES: [usize; 3] = [4, 8, 16];

/// Grid choice meaning "auto": the smallest of [`GRID_SIZES`] that holds the
/// kit's slices, picked again after every add or cut.
pub const AUTO_GRID: usize = 0;

/// The `[N]` slot-count prefix of a file name stem ("[8] drums" → 8, "drums"),
/// as put there when a grid kit is exported. Counts outside 2..=64 don't count.
pub fn slot_prefix(stem: &str) -> Option<(usize, &str)> {
    let (digits, rest) = stem.trim_start().strip_prefix('[')?.split_once(']')?;
    let n: usize = digits.trim().parse().ok()?;
    (2..=64).contains(&n).then(|| (n, rest.trim_start()))
}

/// Smallest grid size that holds `k` slices.
fn auto_size(k: usize) -> Option<usize> {
    GRID_SIZES.into_iter().find(|&g| g >= k)
}

/// One slice, one Vec per channel.
type Slice = Vec<Vec<f32>>;

fn is_empty(s: &Slice) -> bool {
    s.first().is_none_or(Vec::is_empty)
}

/// What removing the selection would do to a grid kit: every slot is as
/// long as the longest slice, so trimming that one's tail shortens them all.
pub struct TrimPreview {
    /// Slot count.
    pub n: usize,
    /// Slot length now and after the cut, in frames.
    pub len: usize,
    pub new_len: usize,
    /// The slot the selection is in.
    pub slot: usize,
    /// The longest sound among the other slots: the slot can't get
    /// shorter than this, so cutting past it is where the savings start.
    pub floor: usize,
    /// The slot whose sound sets `new_len`.
    pub binding: usize,
}

impl TrimPreview {
    /// Frames saved across the whole kit.
    pub fn saved(&self) -> usize {
        (self.len - self.new_len) * self.n
    }
}

impl Editor {
    /// Sound length of each grid slot (up to its last non-zero frame).
    fn slot_sound_lens(&self, n: usize) -> Option<(usize, Vec<usize>)> {
        let full = self.full.as_ref()?;
        let ch = &full.sample.channels;
        let total = full.sample.frames();
        let len = total / n;
        if len == 0 {
            return None;
        }
        let lens = (0..n)
            .map(|i| {
                let a = i * len;
                (a..a + len)
                    .rev()
                    .find(|&f| ch.iter().any(|c| c[f] != 0.0))
                    .map_or(0, |f| f + 1 - a)
            })
            .collect();
        Some((len, lens))
    }

    /// In a grid kit, with part of one slot selected: how the slots would
    /// shrink if the selection were removed.
    pub fn trim_preview(&self) -> Option<TrimPreview> {
        let n = self.grid?;
        if !self.is_kit || self.crop.is_some() {
            return None;
        }
        let (a, b) = self.sel;
        let (len, mut lens) = self.slot_sound_lens(n)?;
        if b <= a {
            return None;
        }
        let slot = a / len;
        let (s0, s1) = (slot * len, (slot + 1) * len);
        // Within one slot, and not the whole of it (that removes the slice).
        if slot >= n || b > s1 || (a == s0 && b == s1) {
            return None;
        }
        let ch = &self.full.as_ref()?.sample.channels;
        let (ra, rb) = (a - s0, b - s0);
        let cur = lens[slot];
        lens[slot] = if rb < cur {
            cur - (rb - ra)
        } else {
            // The cut takes the end of the sound: what's left ends at the
            // last non-zero frame before the selection.
            (s0..a)
                .rev()
                .find(|&f| ch.iter().any(|c| c[f] != 0.0))
                .map_or(0, |f| f + 1 - s0)
        };
        let floor = lens
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != slot)
            .map(|(_, &l)| l)
            .max()
            .unwrap_or(0);
        let (binding, &new_len) = lens.iter().enumerate().max_by_key(|&(_, l)| *l)?;
        Some(TrimPreview {
            n,
            len,
            new_len: new_len.max(1).min(len),
            slot,
            floor,
            binding,
        })
    }

    /// The slices the markers mark out (whole file), trailing silence trimmed,
    /// so a grid slot gives back just its sound.
    fn slices(&self) -> Vec<Slice> {
        let Some(full) = &self.full else {
            return Vec::new();
        };
        let ch = &full.sample.channels;
        let mut bounds = vec![0];
        bounds.extend(&self.markers);
        bounds.push(full.sample.frames());
        bounds
            .windows(2)
            .map(|w| {
                let (a, b) = (w[0], w[1]);
                let end = (a..b)
                    .rev()
                    .find(|&f| ch.iter().any(|c| c[f] != 0.0))
                    .map_or(a, |f| f + 1);
                ch.iter().map(|c| c[a..end].to_vec()).collect()
            })
            .collect()
    }

    /// Rewrite the whole file as `n` equal slots holding `slices` in order.
    /// Empty slots from the end are dropped to make room; with more sounding
    /// slices than slots nothing changes and it returns `None`. Otherwise
    /// returns the slot length in frames.
    /// `names` go with `slices`, one each.
    pub(super) fn lay_out(
        &mut self,
        mut slices: Vec<Slice>,
        mut names: Vec<String>,
        n: usize,
    ) -> Option<usize> {
        let nch = self.full.as_ref()?.sample.channels.len();
        names.resize(slices.len(), String::new());
        while slices.len() > n {
            let i = slices.iter().rposition(is_empty)?;
            slices.remove(i);
            names.remove(i);
        }
        slices.resize(n, vec![Vec::new(); nch]);
        names.resize(n, String::new());
        let len = slices
            .iter()
            .map(|s| s.first().map_or(0, Vec::len))
            .max()
            .unwrap_or(0)
            .max(1);
        let chans: Vec<Vec<f32>> = (0..nch)
            .map(|c| {
                let mut v = Vec::with_capacity(len * n);
                for (i, s) in slices.iter().enumerate() {
                    if let Some(x) = s.get(c) {
                        v.extend_from_slice(x);
                    }
                    v.resize((i + 1) * len, 0.0);
                }
                v
            })
            .collect();
        self.uncrop();
        let total = self.frames();
        self.splice(0, total, &chans);
        self.markers = (1..n).map(|i| i * len).collect();
        self.names = names;
        self.slot_cut = None;
        self.grid = Some(n);
        self.view.fit_pending = true;
        Some(len)
    }

    /// Turn the grid on (re-laying the kit's slices out on `n` slots) or off
    /// (the audio stays as it is).
    pub fn set_grid(&mut self, n: Option<usize>) -> Result<(), String> {
        let Some(n) = n else {
            self.auto_grid = false;
            if self.grid.take().is_some() {
                self.trim_silent_tail();
            }
            return Ok(());
        };
        if !self.is_kit || self.full.is_none() {
            return Err("Grid only applies to a loaded slice kit".to_owned());
        }
        let s = self.slices();
        let sounding = s.iter().filter(|s| !is_empty(s)).count();
        let auto = n == AUTO_GRID;
        let too_many = |n: usize| format!("The kit has {sounding} slices — too many for {n} slots");
        let n = if auto {
            auto_size(sounding).ok_or_else(|| too_many(16))?
        } else {
            n
        };
        self.lay_out(s, self.slice_names(), n)
            .ok_or_else(|| too_many(n))?;
        self.auto_grid = auto;
        Ok(())
    }

    /// How many slices hold any sound.
    pub fn sounding_slices(&self) -> usize {
        let Some(full) = &self.full else { return 0 };
        let ch = &full.sample.channels;
        let mut bounds = vec![0];
        bounds.extend(&self.markers);
        bounds.push(full.sample.frames());
        bounds
            .windows(2)
            .filter(|w| (w[0]..w[1]).any(|f| ch.iter().any(|c| c[f] != 0.0)))
            .count()
    }

    /// Whether the file is cut into slices at all.
    pub fn has_markers(&self) -> bool {
        !self.markers.is_empty()
    }

    /// The grid as picked: [`AUTO_GRID`], a slot count, or free.
    pub fn grid_mode(&self) -> Option<usize> {
        if self.auto_grid {
            Some(AUTO_GRID)
        } else {
            self.grid
        }
    }

    /// How far down the kit you are: the slot after the last one that
    /// sounds (grid), or the number of slices (free).
    pub fn kit_fill(&self) -> usize {
        let Some(full) = &self.full else { return 0 };
        let ch = &full.sample.channels;
        let total = full.sample.frames();
        if total == 0 {
            return 0;
        }
        let Some(n) = self.grid else {
            return self.markers.len() + 1;
        };
        let len = total / n;
        (0..n)
            .rev()
            .find(|&i| {
                ch.iter()
                    .any(|c| c[i * len..(i + 1) * len].iter().any(|&x| x != 0.0))
            })
            .map_or(0, |i| i + 1)
    }

    /// Cut the silence after the last sound: the empty slots and the
    /// padding left at the end when a grid is turned off. Only exact zeros
    /// count, so a quiet tail stays. A kit that's all silence is left alone.
    fn trim_silent_tail(&mut self) {
        self.uncrop();
        let Some(full) = &self.full else { return };
        let ch = &full.sample.channels;
        let total = full.sample.frames();
        let Some(last) = (0..total).rev().find(|&f| ch.iter().any(|c| c[f] != 0.0)) else {
            return;
        };
        let end = last + 1;
        if end >= total {
            return;
        }
        self.splice(end, total, &[]);
        let total = self.frames();
        self.sel = (self.sel.0.min(total), self.sel.1.min(total));
        if self.sel.0 >= self.sel.1 {
            self.sel = (0, total);
        }
        self.playhead = self.playhead.min(total);
        self.view.fit_pending = true;
    }

    /// Add `clip` in the first slot after the last one that sounds, then
    /// select that slot.
    pub(super) fn append_on_grid(&mut self, clip: &AudioClip, n: usize) -> Result<(), String> {
        let Some(full) = &self.full else {
            return Err("No kit loaded".to_owned());
        };
        let ins = clip.adapt(full.sample.rate, full.sample.channels.len());
        let mut s = self.slices();
        let at = s.iter().rposition(|x| !is_empty(x)).map_or(0, |i| i + 1);
        let n = match (at >= n, self.auto_grid) {
            (false, _) => n,
            (true, true) => auto_size(at + 1).ok_or(format!(
                "All {n} slots are full — remove a slice to add more"
            ))?,
            (true, false) => {
                return Err(format!(
                    "All {n} slots are full — remove a slice or pick a bigger grid"
                ));
            }
        };
        s.resize(n.max(s.len()), Vec::new());
        s[at] = ins;
        let mut names = self.slice_names();
        names.resize(s.len(), String::new());
        names[at] = clip.name.clone().unwrap_or_default();
        let len = self
            .lay_out(s, names, n)
            .ok_or("Slice didn't fit the grid")?;
        self.sel = (at * len, (at + 1) * len);
        self.playhead = at * len;
        Ok(())
    }

    /// After a cut / paste / insert in a grid kit: put everything back on
    /// the grid. When it no longer fits, the grid is turned off instead.
    pub(super) fn settle_grid(&mut self) {
        let Some(mut n) = self.grid else { return };
        let (sel, playhead) = (self.sel, self.playhead);
        let s = self.slices();
        if self.auto_grid {
            let sounding = s.iter().filter(|s| !is_empty(s)).count();
            n = auto_size(sounding).unwrap_or(n);
        }
        if self.lay_out(s, self.slice_names(), n).is_none() {
            self.grid = None;
            self.auto_grid = false;
            self.notice = Some(format!(
                "The kit has more than {n} slices now — grid turned off (pick a bigger one to put it back)"
            ));
            return;
        }
        let total = self.frames();
        self.sel = (sel.0.min(total), sel.1.min(total));
        if self.sel.0 >= self.sel.1 {
            self.sel = (0, total);
        }
        self.playhead = playhead.min(total);
    }

    /// The grid this kit's markers describe, if they split it evenly.
    pub(super) fn infer_grid(&self) -> Option<usize> {
        let total = self.full.as_ref()?.sample.frames();
        let n = self.markers.len() + 1;
        if !self.is_kit || !GRID_SIZES.contains(&n) || total % n != 0 {
            return None;
        }
        let len = total / n;
        self.markers
            .iter()
            .enumerate()
            .all(|(i, &m)| m == (i + 1) * len)
            .then_some(n)
    }
}
