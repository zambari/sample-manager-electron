//! Pattern SOUNDS foldouts, sound values, tags and sound copy/paste targets.

use super::*;

/// Where the SOUNDS foldout's columns start, from the row's left edge.
const SND_NAME_X: f32 = 44.0;
const SND_TAG_X: f32 = 160.0;
const SND_POOL_X: f32 = SND_TAG_X + sound_tags::WIDTH + 6.0;
const SND_MACH_X: f32 = SND_POOL_X + 40.0;
const SND_SRC_X: f32 = SND_MACH_X + 74.0;
const SND_SRC_W: f32 = 62.0;
/// A kit track sound value that differs from the pool sound it matches.
const SND_DIFFERS: Color32 = theme::CHANGED;

/// What the SOUNDS foldout of one folded-out pattern row needs.
pub(super) struct SoundFold<'a, 'b> {
    pub(super) ui: &'a egui::Ui,
    pub(super) painter: &'a egui::Painter,
    pub(super) p: &'a Patterns,
    pub(super) cell: &'a Cell,
    /// The pattern slot.
    pub(super) i: usize,
    pub(super) x0: f32,
    pub(super) right: f32,
    pub(super) ty: f32,
    /// Kit (audio) tracks.
    pub(super) kit_n: usize,
    /// Visible kit track indices, retaining their original numbering.
    pub(super) kit_tracks: &'a [usize],
    pub(super) pattern_color: Color32,
    pub(super) open: bool,
    pub(super) show_defaults: bool,
    /// The sound pool as read (empty when not read).
    pub(super) pool: &'a [PoolSound],
    pub(super) pointer: Option<egui::Pos2>,
    pub(super) hover: &'b mut Option<String>,
    pub(super) toggle: &'b mut Vec<usize>,
    /// Set to (pattern slot, track) when a sound line's Copy is picked.
    pub(super) copy: &'b mut Option<(usize, usize)>,
    /// The copied pool sound, when it can be pasted onto a track here.
    pub(super) paste_label: Option<&'a str>,
    /// Set to (pattern slot, track) when a sound line's Paste is picked.
    pub(super) paste: &'b mut Option<(usize, usize)>,
    pub(super) editable: bool,
    pub(super) tag_edit: &'b mut Option<(usize, usize, u32)>,
}

/// The SOUNDS line under a folded-out pattern's tracks and, when open, each
/// kit track's sound decoded as on the Sounds page: name, the pool sound it
/// matches (same name and sample), machine and SRC page. Values that differ
/// from that pool sound are amber. Returns the y below it.
pub(super) fn sound_foldout(f: SoundFold) -> f32 {
    let SoundFold { ui, painter, p, cell, i, x0, right, mut ty, kit_n, kit_tracks, pattern_color, open, show_defaults, pool, pointer, hover, toggle, copy, paste_label, paste, editable, tag_edit } = f;
    let head = Rect::from_min_max(egui::pos2(x0 + 18.0, ty), egui::pos2(x0 + SND_SRC_X, ty + TRACK_H));
    let r = ui.interact(head, ui.scope_id().with(("psnd", i)), Sense::click());
    let col = if r.hovered() || open { OLED } else { OLED_DIM };
    triangle(painter, egui::pos2(x0 + 30.0, head.center().y), open, col);
    painter.text(egui::pos2(x0 + SND_NAME_X, head.center().y), egui::Align2::LEFT_CENTER, "SOUNDS", oled_font(11.0), col);
    if r.on_hover_text("Each kit track's sound: machine and SRC page, decoded like the Sounds page (some values may still read wrong). POOL: the pool sound with the same name and sample; amber values differ from it").clicked() {
        toggle.push(i);
    }
    if open {
        painter.text(egui::pos2(x0 + SND_TAG_X, head.center().y), egui::Align2::LEFT_CENTER, "TAGS", oled_font(9.0), OLED_DIM);
    }
    ty += TRACK_H;
    if !open {
        return ty;
    }
    for &t in kit_tracks {
        let cy = ty + TRACK_H / 2.0;
        let line = Rect::from_min_max(egui::pos2(x0 + 18.0, ty), egui::pos2(right - 4.0, ty + TRACK_H));
        theme::color_badge(painter, line, theme::BadgeEdge::Left, theme::child_identity_color(pattern_color, t, kit_n));
        let from_pool = cell.pool.get(&(t as u8)).and_then(|c| pool_cell_sound(p, pool, c));
        let Some(s) = from_pool.as_ref().or_else(|| cell.sound_src(t).and_then(|src| p.info[src as usize].tracks.get(t)).map(|ti| &ti.sound)) else {
            painter.text(egui::pos2(x0 + 24.0, cy), egui::Align2::LEFT_CENTER, format!("T{}", t + 1), oled_font(10.0), OLED_DIM);
            painter.text(egui::pos2(x0 + SND_NAME_X, cy), egui::Align2::LEFT_CENTER, "—", oled_font(11.0), OLED_DIM);
            ty += TRACK_H;
            continue;
        };
        let mut s = s.clone();
        if let Some(&tags) = cell.tags.get(&(t as u8)) { s.tags = tags; }
        let s = &s;
        let pasted = cell.sounds.contains_key(&(t as u8)) || cell.pool.contains_key(&(t as u8));
        // Right-click: copy this sound (for the sound pool or other patterns).
        let lr = ui.interact(line, ui.scope_id().with(("psnd_line", i, t)), Sense::click());
        lr.context_menu(|ui| {
            let nm = if s.name.is_empty() { "—" } else { s.name.as_str() };
            ui.label(RichText::new(format!("T{} sound “{nm}” of {}", t + 1, patterns::label(i))).weak());
            if ui.button("Copy sound").on_hover_text("Paste it into the sound pool on the Sounds page (⌘V or right-click a sound), or into the same track of other patterns").clicked() {
                *copy = Some((i, t));
                ui.close();
            }
            if let Some(l) = paste_label {
                if ui.button(format!("Paste {l} here")).on_hover_text("This track's kit sound becomes the copied pool sound (a MIDI track becomes an audio track)").clicked() {
                    *paste = Some((i, t));
                    ui.close();
                }
            }
        });
        painter.text(egui::pos2(x0 + 24.0, cy), egui::Align2::LEFT_CENTER, format!("T{}", t + 1), oled_font(10.0), if pasted { theme::DEVICE_ACCENT_BRIGHT } else { OLED_DIM });
        let name = if s.name.is_empty() { "—" } else { s.name.as_str() };
        let nr = Rect::from_min_size(egui::pos2(x0 + SND_NAME_X, ty), egui::vec2(SND_TAG_X - SND_NAME_X - 4.0, TRACK_H));
        painter.with_clip_rect(nr).text(nr.left_center(), egui::Align2::LEFT_CENTER, name, oled_font(12.0), OLED);
        let tags_rect = Rect::from_min_max(egui::pos2(x0 + SND_TAG_X, ty), egui::pos2((x0 + SND_POOL_X - 6.0).min(right - 4.0), ty + TRACK_H));
        if tags_rect.is_positive() {
            if let Some(tags) = sound_tags::show(ui, tags_rect, ui.scope_id().with(("pattern_tags", i, t)), s.tags, editable, cell.tags.contains_key(&(t as u8))) {
                *tag_edit = Some((i, t, tags));
            }
        }
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
            if let Some(c) = cell.pool.get(&(t as u8)) {
                tip += &match c.src {
                    SoundSrc::Read(o) => format!("\n(pool sound {:03} pasted here)", o + 1),
                    _ => "\n(pool sound pasted here)".to_owned(),
                };
            } else if pasted {
                tip += "\n(sound pasted from another pattern)";
            }
            tip += "\n\nRight-click: copy this sound (to paste into the sound pool)";
            *hover = Some(tip);
        }
        ty += TRACK_H;
    }
    ty
}

