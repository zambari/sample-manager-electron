//! Pattern-list orchestration, row layout and deferred row actions.

use super::*;

/// Actions collected while drawing rows, applied after the list is painted.
#[derive(Default)]
pub(super) struct ListActions {
    pub(super) bank_toggles: Vec<usize>,
    pub(super) toggles: Vec<usize>,
    pub(super) track_click: Option<(usize, u8, Vec<u8>)>,
    pub(super) snd_toggles: Vec<usize>,
    pub(super) snd_copy: Option<(usize, usize)>,
    pub(super) snd_paste: Option<(usize, usize)>,
    pub(super) tag_edit: Option<(usize, usize, u32)>,
    pub(super) inline_done: Option<(usize, Option<String>)>,
    pub(super) status: Option<String>,
}

pub(super) fn list(ui: &mut egui::Ui, st: &mut ElekState, busy: bool) {
    let Some(proj) = st.project.as_ref() else { return };
    let Ok(p) = proj.patterns.as_deref() else { return };
    let editable = p.read_only.is_none() && !busy;
    // Audio tracks: 8 on the DT1 (then 8 MIDI tracks), 16 on the DT2.
    let audio = proj.model.kit_tracks();
    let show_defaults = st.lib.show_defaults;
    let ed = &mut st.proj;
    // The sound pool as read, to point kit track sounds at the pool sound
    // they match (same name and sample); only while a SOUNDS foldout is open.
    // Also for tracks given a pool sound, to show what they got.
    let pool_snd: Vec<PoolSound> = if ed.snd_open.is_empty() && ed.cells.iter().all(|c| c.pool.is_empty()) {
        Vec::new()
    } else {
        proj.pool_sounds().unwrap_or_default()
    };
    let pool_label = ed.pool_clip.as_ref().filter(|c| c.track_ok.is_ok()).map(PoolClip::desc);
    let mut actions = ListActions::default();
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
    let mut rows = Vec::new();

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
                let bank_open = ed.banks[b];
                let bank_color = theme::identity_color(b, BANKS);
                let (r, hdr) = bank_header(ui, p, ed, b, w);
                if hdr.clicked() {
                    actions.bank_toggles.push(b);
                }
                if ed.drag.is_some() && !bank_open && pointer.is_some_and(|pt| r.contains(pt)) {
                    actions.bank_toggles.push(b);
                }
                if !bank_open {
                    continue;
                }
                for &i in vis.iter().filter(|&&i| i / PER_BANK == b) {

                let pattern_color = theme::child_identity_color(bank_color, i % PER_BANK, PER_BANK);
                let cell = ed.cells[i].clone();
                let cell = &cell;
                let (state, name) = view(p, cell);
                // Pattern-level things (and the track layout) come from the
                // pattern itself, else from where its pasted tracks came from.
                let info = cell.src.or_else(|| cell.tracks.values().chain(cell.sounds.values()).flatten().next().copied()).map(|s| &p.info[s as usize]);
                let open = ed.open.contains(&i);
                let tracks: Vec<_> = info.map(|n| shown_tracks(n.seq.len(), audio, |t| seq_of(p, cell, t, &blank).is_empty())).unwrap_or_default()
                    .into_iter().filter(|(_, t)| !ed.used_only || seq_of(p, cell, *t, &blank).notes() > 0).collect();
                let mut step_hover: Option<String> = None;
                let byte_rows = if ed.bytes && info.is_some() { 2.0 } else { 0.0 };
                // Kit tracks with a decoded sound, for the SOUNDS foldout under the tracks.
                let kit_n = info.map_or(0, |n| if tracks.is_empty() { 0 } else { n.tracks.len().min(audio) });
                let kit_tracks: Vec<usize> = (0..kit_n).filter(|&t| !ed.used_only || tracks.iter().any(|(_, track)| *track == t)).collect();
                let snd_open = ed.snd_open.contains(&i);
                let snd_rows = if kit_tracks.is_empty() { 0.0 } else { 1.0 + if snd_open { kit_tracks.len() as f32 } else { 0.0 } };
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
                // Pattern hover, selection, dragging and its menu stay on
                // the header; folded-out tracks and sounds handle themselves.
                let resp = ui.interact(rect, ui.scope_id().with(("prow", i)), Sense::hover());
                let head = Rect::from_min_size(rect.left_top(), egui::vec2(rect.width(), ROW_H - 2.0));
                let hresp = ui.interact(head, ui.scope_id().with(("phead", i)), if editable { Sense::click_and_drag() } else { Sense::click() });
                let selected = ed.sel.contains(&i);
                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, 2.0, ROW_FILL);
                if hresp.contains_pointer() {
                    painter.rect_filled(head, 2.0, OLED_HOVER);
                }
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

                theme::color_badge(&painter, head, theme::BadgeEdge::Left, pattern_color);
                let y = head.center().y;
                let x0 = rect.left();
                // Fold-out arrow, its own click target.
                let chev = Rect::from_center_size(egui::pos2(x0 + 14.0, y), egui::vec2(44.0, ROW_H - 4.0));
                let chev_resp = ui.interact(chev, ui.scope_id().with(("pchev", i)), Sense::click());
                triangle(&painter, chev.center() + egui::vec2(12.0, 0.0), open, if chev_resp.hovered() { OLED } else { OLED_DIM });
                if chev_resp.clicked() {
                    actions.toggles.push(i);
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
                            let why = if pending { "not loaded yet" } else if ed.used_only && !n.seq.is_empty() { "no tracks with trigs" } else { "step layout unknown for this version" };
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
                            if let Some(hover) = track_row(ui, ed, TrackRow {
                                painter: &painter,
                                p,
                                cell,
                                pool: &pool_snd,
                                blank: &blank,
                                sample_name: &sample_name,
                                i,
                                t: *t,
                                label,
                                shown: &shown,
                                rect,
                                ty,
                                pattern_color,
                                seq_count: n.seq.len(),
                                editable,
                                audio,
                                pointer,
                            }, &mut actions) {
                                step_hover = Some(hover);
                            }
                            ty += TRACK_H;
                        }
                        if !kit_tracks.is_empty() {
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
                                kit_tracks: &kit_tracks,
                                pattern_color,
                                open: snd_open,
                                show_defaults,
                                pool: &pool_snd,
                                pointer,
                                hover: &mut step_hover,
                                toggle: &mut actions.snd_toggles,
                                copy: &mut actions.snd_copy,
                                paste_label: pool_label.as_deref().filter(|_| editable),
                                paste: &mut actions.snd_paste,
                                editable,
                                tag_edit: &mut actions.tag_edit,
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
                        actions.inline_done = Some((i, (!esc).then(|| ed.inline_text.clone())));
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
                let hresp = hresp.on_hover_text_at_pointer(hover.clone());
                resp.on_hover_text_at_pointer(hover);
                if hresp.drag_started() {
                    ed.clear_tracks();
                    if !ed.sel.contains(&i) {
                        ed.select_only(i);
                    }
                    ed.drag = Some(i);
                }
                if hresp.double_clicked() && editable {
                    ed.start_inline(p, i);
                } else if hresp.clicked() {
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
                hresp.context_menu(|ui| {
                    pattern_menu(ui, ed, p, PatternMenu { i, open, editable, audio }, &mut actions);
                });
                }
            }
        });
        ed.list_rect = Some(sa.inner_rect);
    });
    ed.rows = rows;

    let ListActions { bank_toggles, toggles, track_click, snd_toggles, snd_copy,
        snd_paste, tag_edit, inline_done, mut status } = actions;

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
    if let Some((i, t, tags)) = tag_edit {
        ed.edit(|cells| { cells[i].tags.insert(t as u8, tags); });
        status = Some(format!("{} T{} tags changed. Push to send them", patterns::label(i), t + 1));
    }
    if let Some((i, t)) = snd_paste {
        status = Some(paste_pool_sound(ed, p, i, &[t as u8], audio));
    }
    if let Some((i, t)) = snd_copy.filter(|(i, t)| edited_track_cell(&ed.cells[*i], *t).is_some()) {
        // Copy the sound as edited, including pending tag changes.
        let cell = edited_track_cell(&ed.cells[i], t).expect("overridden sound");
        let at = match cell.src {
            SoundSrc::Read(o) => o as usize,
            _ => 0,
        };
        let name = pool_cell_sound(p, &pool_snd, &cell).map(|s| s.name).unwrap_or_default();
        let ok = proj.pool_sound_for_track(&cell).map(|_| ());
        ed.set_pool_clip(PoolClip { at, cell, name, track_ok: ok });
        let msg = format!("Copied {} (as on {} {})", ed.pool_clip.as_ref().map(PoolClip::desc).unwrap_or_default(), patterns::label(i), track_name(t, audio));
        ui.ctx().copy_text(msg.clone());
        status = Some(msg);
    } else if let Some((i, t)) = snd_copy {
        let src = ed.cells[i].sound_src(t);
        ed.tclip_snd = Some(TrackClip { tracks: vec![(t as u8, src)], from: i, overrides: Default::default(), tags: Default::default() });
        ed.pool_clip = None;
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

