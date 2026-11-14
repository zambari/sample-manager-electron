//! Undoable pattern operations: move, copy, clear, pack and rename.

use super::*;

/// Move (swap) or copy the selection so the grabbed row `from` lands on `to`.
pub(super) fn drop_sel(ed: &mut ProjEd, from: usize, to: usize, copy: bool) -> Result<String, String> {
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

pub(super) fn clear(ed: &mut ProjEd, p: &Patterns) -> String {
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

pub(super) fn copy(ed: &mut ProjEd) -> String {
    let t = ed.targets();
    ed.clip = t.iter().map(|&i| ed.cells[i].clone()).collect();
    ed.tclip_ev = None;
    ed.tclip_snd = None;
    ed.pool_clip = None;
    format!("Copied {} pattern(s); select a slot and paste (⌘V)", ed.clip.len())
}

pub(super) fn paste(ed: &mut ProjEd) -> String {
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
pub(super) fn pack(ed: &mut ProjEd, p: &Patterns) -> String {
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
pub(super) fn rename(ed: &mut ProjEd, p: &Patterns, t: &[usize], text: &str) -> String {
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

