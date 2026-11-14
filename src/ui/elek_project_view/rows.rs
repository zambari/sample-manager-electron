//! Bank headers, folded-out track rows and pattern context menus.

use super::*;

/// Tracks a folded-out row shows: the audio tracks, then MIDI tracks that
/// have something on them. `(label, index into seq)`.
pub(super) fn shown_tracks(n: usize, audio: usize, empty: impl Fn(usize) -> bool) -> Vec<(String, usize)> {
    let mut v: Vec<(String, usize)> = (0..audio.min(n)).map(|t| (track_name(t, audio), t)).collect();
    v.extend((audio..n).filter(|&t| !empty(t)).map(|t| (track_name(t, audio), t)));
    v
}

/// One bank's fold control and loaded / changed counts.
pub(super) fn bank_header(ui: &mut egui::Ui, p: &Patterns, ed: &ProjEd, b: usize, w: f32) -> (Rect, egui::Response) {
    let (r, hdr) = ui.allocate_exact_size(egui::vec2(w, BANK_H), Sense::click());
    let bank_open = ed.banks[b];
    let used = (b * PER_BANK..(b + 1) * PER_BANK).filter(|&k| view(p, &ed.cells[k]).0 == PatState::Used).count();
    let changed = (b * PER_BANK..(b + 1) * PER_BANK).filter(|&k| ed.cells[k] != Cell::as_read(k)).count();
    if hdr.hovered() {
        ui.painter().rect_filled(r, 2.0, OLED_HOVER);
    }
    let bank_color = theme::identity_color(b, BANKS);
    theme::color_badge(ui.painter(), r, theme::BadgeEdge::Left, bank_color);
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
    (r, hdr)
}

/// Everything needed to draw and interact with one folded-out track.
pub(super) struct TrackRow<'a> {
    pub(super) painter: &'a egui::Painter,
    pub(super) p: &'a Patterns,
    pub(super) cell: &'a Cell,
    pub(super) pool: &'a [PoolSound],
    pub(super) blank: &'a TrackSeq,
    pub(super) sample_name: &'a dyn Fn(usize) -> String,
    pub(super) i: usize,
    pub(super) t: usize,
    pub(super) label: &'a str,
    pub(super) shown: &'a [u8],
    pub(super) rect: Rect,
    pub(super) ty: f32,
    pub(super) pattern_color: Color32,
    pub(super) seq_count: usize,
    pub(super) editable: bool,
    pub(super) audio: usize,
    pub(super) pointer: Option<egui::Pos2>,
}

pub(super) fn track_row(ui: &mut egui::Ui, ed: &mut ProjEd, row: TrackRow<'_>, actions: &mut ListActions) -> Option<String> {
    let TrackRow { painter, p, cell, pool, blank, sample_name, i, t, label, shown,
        rect, ty, pattern_color, seq_count, editable, audio, pointer } = row;
    let x0 = rect.left();
    let dots_x = x0 + DOTS_X;
    let mut step_hover = None;
    let cy = ty + TRACK_H / 2.0;
    let tt = t as u8;
    let from = cell.tracks.get(&tt).copied();
    let snd_from = cell.sounds.get(&tt).copied();
    let from_pool = cell.pool.get(&tt);
    let line = Rect::from_min_max(egui::pos2(x0 + 18.0, ty), egui::pos2(rect.right() - 4.0, ty + TRACK_H));
    let t_selected = ed.tsel_pat == Some(i) && ed.tsel.contains(&tt);
    if t_selected {
        painter.rect_filled(line, 2.0, TRACK_SEL);
    }
    theme::color_badge(painter, Rect::from_min_max(line.min, egui::pos2(x0 + 21.0, line.bottom())), theme::BadgeEdge::Left, theme::child_identity_color(pattern_color, t, seq_count));
    // The name and sample part of the line selects the track.
    let hit = Rect::from_min_max(line.min, egui::pos2(x0 + DOTS_X - 8.0, line.max.y));
    let tresp = ui.interact(hit, ui.scope_id().with(("ptrack", i, tt)), Sense::click());
    let label_col = if from.is_some() || snd_from.is_some() || from_pool.is_some() {
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
        actions.track_click = Some((i, tt, shown.to_vec()));
    }
    tresp.context_menu(|ui| {
        if !t_selected {
            ed.click_track(i, tt, shown, egui::Modifiers::NONE);
        }
        let what = match ed.tsel.len() {
            1 => label.to_owned(),
            n => format!("{n} tracks"),
        };
        ui.menu_button(format!("Copy {what}"), |ui| {
            if let Some(s) = copy_items(ui, ed, audio) {
                actions.status = Some(s);
            }
        });
        ui.add_enabled_ui(editable && (ed.has_tclip() || ed.pool_clip.is_some()), |ui| {
            ui.menu_button("Paste here", |ui| {
                if let Some(s) = paste_items(ui, ed, p, &[i], editable, audio) {
                    actions.status = Some(s);
                }
            });
        });
    });
    let ti = match from_pool.and_then(|c| pool_cell_sound(p, pool, c)) {
        Some(ps) => Some((ps.slot, ps.name)),
        None => cell.sound_src(t).and_then(|s| p.info[s as usize].tracks.get(t)).map(|ti| (ti.slot, ti.name.clone())),
    };
    if let Some((slot, name)) = ti {
        // The sample, then the sound's own name when it isn't the default.
        let mut text = sample_name(slot);
        if name != format!("SOUND {}", t + 1) && !name.is_empty() {
            text = format!("{text}  ·  {name}");
        }
        let sr = Rect::from_min_size(egui::pos2(x0 + NAME_X, ty), egui::vec2(DOTS_X - NAME_X - 56.0, TRACK_H));
        painter.with_clip_rect(sr).text(sr.left_center(), egui::Align2::LEFT_CENTER, text, egui::FontId::proportional(12.0), OLED);
    }
    let seq = seq_of(p, cell, t, blank);
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
        hex_row(painter, egui::pos2(dots_x, cy), &hx, OLED, Some(&seq.tail));
    } else {
        let max_w = (rect.right() - dots_x - 10.0).max(120.0);
        dots(painter, egui::pos2(dots_x, cy), max_w, seq);
        if let Some(pp) = pointer.filter(|pp| (pp.y - cy).abs() <= TRACK_H / 2.0 && rect.contains(*pp)) {
            let hit = nearest_step(seq.steps.len(), dots_x, max_w, pp.x, |k| k < seq.len && seq.steps[k] != Step::None);
            if let Some((k, sx)) = hit {
                painter.circle_stroke(egui::pos2(sx, cy), 4.5, egui::Stroke::new(1.0, theme::DEVICE_ACCENT_BRIGHT));
                step_hover = Some(step_tip(seq, k, label, sample_name));
            }
        }
    }
    step_hover
}

pub(super) struct PatternMenu {
    pub(super) i: usize,
    pub(super) open: bool,
    pub(super) editable: bool,
    pub(super) audio: usize,
}

/// Pattern-only menu; folded-out track and sound menus have their own targets.
pub(super) fn pattern_menu(ui: &mut egui::Ui, ed: &mut ProjEd, p: &Patterns, menu: PatternMenu, actions: &mut ListActions) {
    let PatternMenu { i, open, editable, audio } = menu;
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
        actions.toggles.push(i);
        ui.close();
    }
    if ui.button("Copy pattern  ⌘C").clicked() {
        actions.status = Some(copy(ed));
        ui.close();
    }
    if ui.add_enabled(editable && !ed.clip.is_empty(), egui::Button::new("Paste here  ⌘V")).clicked() {
        actions.status = Some(paste(ed));
        ui.close();
    }
    if ed.has_tclip() {
        let into = ed.targets();
        let at = if into.len() > 1 { format!("Paste tracks into {} patterns", into.len()) } else { "Paste tracks here".to_owned() };
        ui.add_enabled_ui(editable, |ui| {
            ui.menu_button(at, |ui| {
                if let Some(s) = paste_items(ui, ed, p, &into, editable, audio) {
                    actions.status = Some(s);
                }
            });
        });
    }
    if ui.add_enabled(editable && p.can_clear(), egui::Button::new("Clear  ⌫")).clicked() {
        actions.status = Some(clear(ed, p));
        ui.close();
    }
    if ui.add_enabled(editable, egui::Button::new("Pack bank (used patterns first)")).clicked() {
        actions.status = Some(pack(ed, p));
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
}

