//! Shared selection, ordering and drop feedback for numbered slot lists.
//! Views own their data and undo; these operations only rearrange slot identities.

use super::theme;
use egui::{Modifiers, Rect, Stroke};
use std::collections::{BTreeSet, HashSet};

pub fn click(
    sel: &mut BTreeSet<usize>,
    cursor: &mut Option<usize>,
    slot: usize,
    m: Modifiers,
    first: usize,
) {
    if m.shift {
        let a = cursor.unwrap_or(slot);
        if !m.command {
            sel.clear();
        }
        sel.extend(a.min(slot)..=a.max(slot));
    } else if m.command {
        if !sel.remove(&slot) {
            sel.insert(slot);
        }
        *cursor = Some(slot);
    } else {
        sel.clear();
        sel.insert(slot);
        *cursor = Some(slot);
    }
    sel.retain(|s| *s >= first);
}

/// Insert selected cells at the boundary `to`, closing up their old positions.
pub fn insert<T: Clone>(order: &mut Vec<T>, sel: &[usize], to: usize, first: usize) {
    if sel.is_empty() {
        return;
    }
    let to = to.clamp(first, order.len());
    let moved: Vec<T> = sel.iter().map(|&s| order[s].clone()).collect();
    let set: HashSet<usize> = sel.iter().copied().collect();
    let mut rest: Vec<T> = order
        .iter()
        .enumerate()
        .filter(|(i, _)| !set.contains(i))
        .map(|(_, o)| o.clone())
        .collect();
    let at = to - sel.iter().filter(|&&s| s < to).count();
    rest.splice(at..at, moved);
    *order = rest;
}

/// Place a group in consecutive cells, exchanging displaced cells with the freed ones.
/// Returns the actual first target (clamped so the entire group fits).
pub fn place<T: Clone>(order: &mut [T], sel: &[usize], to: usize, first: usize) -> usize {
    if sel.is_empty() {
        return to;
    }
    let to = to.clamp(first, order.len() - sel.len());
    let targets: Vec<usize> = (to..to + sel.len()).collect();
    let tset: HashSet<usize> = targets.iter().copied().collect();
    let sset: HashSet<usize> = sel.iter().copied().collect();
    let displaced: Vec<usize> = targets
        .iter()
        .copied()
        .filter(|t| !sset.contains(t))
        .collect();
    let freed: Vec<usize> = sel.iter().copied().filter(|s| !tset.contains(s)).collect();
    let old = order.to_vec();
    for (&s, &t) in sel.iter().zip(&targets) {
        order[t] = old[s].clone();
    }
    for (&d, &f) in displaced.iter().zip(&freed) {
        order[f] = old[d].clone();
    }
    to
}

pub fn nudge<T>(order: &mut [T], sel: &[usize], dir: i32, first: usize) {
    let (Some(&lo), Some(&hi)) = (sel.first(), sel.last()) else {
        return;
    };
    if (dir < 0 && lo <= first) || (dir > 0 && hi + 1 >= order.len()) {
        return;
    }
    if dir < 0 {
        for &s in sel {
            order.swap(s - 1, s);
        }
    } else {
        for &s in sel.iter().rev() {
            order.swap(s, s + 1);
        }
    }
}

pub fn insert_empty<T>(
    order: &mut [T],
    at: usize,
    n: usize,
    free: impl Fn(&T) -> bool,
) -> (usize, usize) {
    let (mut made, mut last) = (0, at);
    while made < n {
        let from = at + made.max(1);
        let Some(e) = (from..order.len()).find(|&i| free(&order[i])) else {
            break;
        };
        order[at..=e].rotate_right(1);
        made += 1;
        last = last.max(e);
    }
    (made, last)
}

pub fn close_gap<T>(order: &mut [T], at: usize, free: impl Fn(&T) -> bool) {
    let end = (at + 1..order.len())
        .find(|&i| free(&order[i]))
        .unwrap_or(order.len());
    order[at..end].rotate_left(1);
}

#[derive(Clone, Copy)]
pub enum DropTarget {
    Place(usize),
    Insert(usize),
}

/// Modifier and upper/lower-half handling is identical in each slot view.
pub fn drop_target(ui: &egui::Ui, rect: Rect, slot: usize) -> DropTarget {
    ui.input(|i| {
        if i.modifiers.ctrl || i.modifiers.alt {
            DropTarget::Insert(
                slot + usize::from(
                    i.pointer
                        .interact_pos()
                        .is_some_and(|p| p.y > rect.center().y),
                ),
            )
        } else {
            DropTarget::Place(slot)
        }
    })
}

pub fn paint_drop(ui: &egui::Ui, rect: Rect, slot: usize, target: DropTarget, count: usize) {
    let stroke = Stroke::new(2.0, theme::DEVICE_ACCENT_BRIGHT);
    match target {
        DropTarget::Insert(at) => {
            ui.painter().hline(
                rect.x_range(),
                if at == slot {
                    rect.top()
                } else {
                    rect.bottom()
                },
                stroke,
            );
        }
        DropTarget::Place(_) => {
            let r = Rect::from_min_size(
                rect.min,
                egui::vec2(rect.width(), rect.height() * count.max(1) as f32),
            );
            ui.painter()
                .rect_stroke(r, 0.0, stroke, egui::StrokeKind::Inside);
        }
    }
}
