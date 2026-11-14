//! Frame geometry, hit priorities, snapping and active waveform drags.

use super::*;

/// Geometry captured before mouse edits, shared by hit detection and painting.
/// In particular, changing the selection or panning during a drag does not
/// change the map halfway through the frame.
pub(super) struct WaveGeometry<'a> {
    pub(super) m: Map,
    pub(super) sel: (usize, usize),
    pub(super) marks: Vec<usize>,
    pub(super) abrupt: &'a [crate::editor::FadeEnd],
    unlocked: bool,
    pub(super) off: usize,
    /// Slot count, slot length, cut length, and each slot's sound length.
    pub(super) slot_cut: Option<(usize, usize, usize, Vec<usize>)>,
}

impl<'a> WaveGeometry<'a> {
    pub(super) fn new(
        ed: &Editor,
        rect: Rect,
        frames: usize,
        abrupt: &'a [crate::editor::FadeEnd],
        trimming: bool,
    ) -> Self {
        let slot_cut = match (trimming && ed.trims_by_length(), ed.grid, ed.slot_sounds()) {
            (true, Some(n), Some((len, sounds))) => {
                Some((n, len, ed.slot_cut_len().unwrap_or(len), sounds))
            }
            _ => None,
        };
        Self {
            m: Map {
                rect,
                start: ed.view.start,
                fpp: ed.view.fpp,
                frames,
            },
            sel: ed.sel,
            marks: if ed.is_kit { ed.marks() } else { Vec::new() },
            abrupt,
            unlocked: ed.is_kit && !ed.markers_locked,
            off: ed.crop.map_or(0, |c| c.0),
            slot_cut,
        }
    }

    fn fade_bounds(&self) -> (f32, f32) {
        let top = self.m.rect.top() + RULER_H + 4.0;
        (top, self.m.rect.bottom() - top)
    }

    pub(super) fn handle_at(&self, p: Pos2) -> Option<crate::editor::FadeEnd> {
        let (top, height) = self.fade_bounds();
        self.abrupt
            .iter()
            .find(|e| {
                let x = self.m.x(e.start as f64 + (e.end - e.start) as f64 / 2.0);
                let y = top + (1.0 - crate::editor::fade_gain(0.5, e.k)) * height;
                Pos2::new(x, y).distance(p) <= HANDLE_R + 4.0
            })
            .copied()
    }

    fn cut_at(&self, p: Pos2) -> Option<usize> {
        let (n, len, cut, _) = self.slot_cut.as_ref()?;
        if p.y <= self.m.rect.top() + RULER_H {
            return None;
        }
        (0..*n)
            .find(|i| (self.m.x((i * len + cut) as f64 - self.off as f64) - p.x).abs() <= GRAB_PX)
    }

    /// Nearest kit slice marker within the grab radius.
    pub(super) fn mark_at(&self, p: Pos2) -> Option<usize> {
        self.marks
            .iter()
            .copied()
            .map(|f| (f, (self.m.x(f as f64) - p.x).abs()))
            .filter(|&(_, d)| d <= GRAB_PX)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(f, _)| f)
    }

    /// Forgive small misclicks at slice markers and the ends of the file.
    pub(super) fn snapped(&self, x: f32) -> usize {
        self.marks
            .iter()
            .copied()
            .chain([0, self.m.frames])
            .map(|f| (f, (self.m.x(f as f64) - x).abs()))
            .filter(|&(_, d)| d <= SNAP_PX)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map_or_else(|| self.m.frame(x), |(f, _)| f)
    }

    /// Nearest in/out point; ties belong to the in point.
    pub(super) fn endpoint_at(&self, p: Pos2) -> Option<usize> {
        let di = (p.x - self.m.x(self.sel.0 as f64)).abs();
        let dout = (p.x - self.m.x(self.sel.1 as f64)).abs();
        if di <= GRAB_PX && (dout > GRAB_PX || di <= dout) {
            Some(self.sel.0)
        } else if dout <= GRAB_PX {
            Some(self.sel.1)
        } else {
            None
        }
    }

    fn on_bar(&self, p: Pos2) -> bool {
        p.y >= self.m.rect.top()
            && p.y <= self.m.rect.top() + RULER_H
            && p.x > self.m.x(self.sel.0 as f64) + GRAB_PX
            && p.x < self.m.x(self.sel.1 as f64) - GRAB_PX
    }

    fn grab_mark(&self, ed: &Editor, p: Pos2, ruler_only: bool) -> Option<usize> {
        if !self.unlocked || (ruler_only && p.y > self.m.rect.top() + RULER_H) {
            return None;
        }
        self.mark_at(p).and_then(|f| ed.marker_index(f))
    }

    /// Shared hover/drag priority: fades, pan modifiers, marker tabs,
    /// selection ends, marker lines, slot cuts, then the range bar.
    pub(super) fn drag_at(&self, ed: &Editor, p: Pos2, middle: bool, alt: bool) -> Drag {
        if let Some(e) = self.handle_at(p).filter(|_| !middle) {
            Drag::Fade(
                e.slice,
                if e.tail { e.start } else { e.end },
                e.tail,
                alt || e.own,
            )
        } else if middle || alt {
            Drag::Pan
        } else if let Some(i) = self.grab_mark(ed, p, true) {
            Drag::Marker(i)
        } else if let Some(f) = self.endpoint_at(p) {
            if f == self.sel.0 {
                Drag::In
            } else {
                Drag::Out
            }
        } else if let Some(i) = self.grab_mark(ed, p, false) {
            Drag::Marker(i)
        } else if let Some(i) = self.cut_at(p) {
            Drag::SlotCut(i)
        } else if self.on_bar(p) {
            Drag::Move(ed.sel, self.m.frame(p.x))
        } else {
            Drag::Select(self.snapped(p.x))
        }
    }

    pub(super) fn update_drag(&self, ui: &mut Ui, ed: &mut Editor, resp: &egui::Response) {
        let m = &self.m;
        let rect = m.rect;
        let (fade_top, fade_h) = self.fade_bounds();
        let slot_cut = &self.slot_cut;
        let off = self.off;
        if let (Some(d), Some(p)) = (ed.drag, resp.interact_pointer_pos()) {
            let px = p.x.clamp(rect.left(), rect.right());
            let f = self.snapped(px);
            match d {
                Drag::In => ed.set_in(f),
                Drag::Out => ed.set_out(f),
                Drag::Select(a) => ed.set_region(a, f),
                Drag::Move(orig, grab) => ed.move_region(orig, m.frame(px) as i64 - grab as i64),
                Drag::Pan => ed.view.start -= resp.drag_delta().x as f64 * ed.view.fpp,
                Drag::Scrub(..) => {}
                Drag::Marker(i) => ed.move_marker(i, m.frame(px)),
                Drag::Fade(slice, fixed, tail, own) => {
                    // Sideways: the knob is the fade's middle, so the fade
                    // runs twice that far from its fixed end. Up / down:
                    // the gain at the middle, which sets the curve.
                    let len = if tail {
                        2 * (m.frame(px).max(fixed + 1) - fixed)
                    } else {
                        2 * (fixed - m.frame(px).min(fixed.saturating_sub(1)))
                    };
                    let k = crate::editor::fade_k_for_mid(1.0 - (p.y - fade_top) / fade_h);
                    if own {
                        ed.shape_fade(slice, len, k);
                    } else {
                        ed.shape_all_fades(len, k);
                    }
                }
                Drag::SlotCut(i) => {
                    if let Some((_, len, _, sounds)) = &slot_cut {
                        let rel = (m.frame(px) + off).saturating_sub(i * len);
                        // Snap onto where a sound ends: cutting right
                        // there frees the most without touching it.
                        let rel = sounds
                            .iter()
                            .copied()
                            .filter(|&s| {
                                s > 0 && (s.abs_diff(rel) as f64 / m.fpp) <= SNAP_PX as f64
                            })
                            .min_by_key(|&s| s.abs_diff(rel))
                            .unwrap_or(rel)
                            .clamp(1, *len);
                        ed.slot_cut = (rel < *len).then_some(rel);
                    }
                }
            }
            // Dragging a marker past the edge scrolls the view along.
            if !matches!(d, Drag::Pan | Drag::Fade(..) | Drag::SlotCut(..)) {
                let edge = 12.0;
                if p.x > rect.right() - edge {
                    ed.view.start += 8.0 * ed.view.fpp;
                } else if p.x < rect.left() + edge {
                    ed.view.start -= 8.0 * ed.view.fpp;
                }
                ui.ctx().request_repaint();
            }
        }
    }
}
