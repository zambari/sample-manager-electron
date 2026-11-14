//! Sample-pool bank panes, slot drawing and drag/drop action collection.

use super::*;

/// Shared facts for a sample-slot row's drop feedback, menu and hover text.
pub(super) struct SlotRowInfo<'a> {
    pub(super) slot: usize,
    pub(super) from: usize,
    pub(super) entry: PoolEntry,
    pub(super) linked: Option<&'a (String, std::path::PathBuf, bool)>,
    pub(super) changed: bool,
    pub(super) selected: bool,
    pub(super) moved: bool,
    pub(super) uses: u32,
    pub(super) rect: egui::Rect,
}

pub(super) fn body(ui: &mut egui::Ui, st: &mut ElekState, out: &mut Vec<ElekIntent>, busy: bool) {
    if st.project.as_ref().is_some_and(|p| p.partial) && busy {
        ui.label(RichText::new("Sample pool loaded; use counts fill in as the patterns arrive. You can start arranging, pushing unlocks when the read finishes.").weak());
    } else if st.project.as_ref().is_some_and(|p| p.partial) {
        ui.colored_label(
            Color32::from_rgb(230, 120, 90),
            "The read was stopped: use counts are incomplete and sample order changes can't be pushed. Reload to read the whole project.",
        );
    } else if let Some(why) = st.project.as_ref().and_then(|p| p.read_only.clone()) {
        ui.colored_label(Color32::from_rgb(230, 120, 90), format!("Read-only: {why}"))
            .on_hover_text(
                "Pushing is disabled because the dump doesn't match the layouts we know.",
            );
    }

    if st.project.is_none() {
        load_screen(ui, st, out);
    } else {
        st.keys(ui.ctx());

        let w = (ui.available_width() - 8.0) / 2.0;
        let h = (ui.available_height() - MEM_BAR_H).max(ROW_H * 4.0);
        ui.horizontal_top(|ui| {
            for side in 0..2 {
                ui.allocate_ui_with_layout(
                    egui::vec2(w, h),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_min_size(egui::vec2(w, h));
                        ui.push_id(side, |ui| slot_pane(ui, st, side));
                    },
                );
            }
        });
        memory_bar(ui, st);
    }
}

pub(super) fn slot_pane(ui: &mut egui::Ui, st: &mut ElekState, side: usize) {
    // A slot drag ends with the button release (handled the frame it
    // happens) or Escape.
    if ui.input(|i| (!i.pointer.any_down() && !i.pointer.any_released()) || i.key_pressed(egui::Key::Escape)) {
        st.slot_drag = false;
    }
    let Some(p) = &st.project else { return };
    let n = p.pool.len();
    let banks = n.div_ceil(BANK);
    // Keep the keyboard cursor visible in the pane it drives, and a pinged
    // slot in the pane picked for it.
    let target = st
        .reveal
        .filter(|_| side == st.active)
        .or_else(|| st.ping.filter(|_| st.ping_scroll == Some(side)).map(|(pos, _)| pos));
    if let Some(r) = target {
        st.bank[side] = r / BANK;
    }
    if banks > 1 {
        // Device-style bank strip: "< A B C D E F G H >", current one boxed.
        let (strip, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), ROW_H + 6.0),
            Sense::empty(),
        );
        ui.painter().rect_filled(strip, 0.0, OLED_BG);
        // Narrow cells in a narrow pane so the strip never spills past the
        // left edge (it is centred, so overflow would be clipped there).
        let cells = banks as f32 + 2.0;
        let cell = (strip.width() / cells).clamp(12.0, 26.0);
        let x0 = (strip.center().x - cell * cells / 2.0).max(strip.left());
        let font = oled_font(TEXT);
        let arrow = |ui: &mut egui::Ui, i: f32, txt: &str, to: Option<usize>| {
            let r = egui::Rect::from_min_size(
                egui::pos2(x0 + i * cell, strip.top() + 3.0),
                egui::vec2(cell, ROW_H),
            );
            let resp = ui.interact(r, ui.scope_id().with(("bank_tab", txt)), Sense::click());
            let on = to.is_some();
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                txt,
                font.clone(),
                if on && resp.hovered() { OLED } else { OLED_DIM },
            );
            (resp, to)
        };
        let mut pick = None;
        let cur = st.bank[side];
        let (r, to) = arrow(ui, 0.0, "<", cur.checked_sub(1));
        if r.clicked() {
            pick = to;
        }
        for b in 0..banks {
            let r = egui::Rect::from_min_size(
                egui::pos2(x0 + (b + 1) as f32 * cell, strip.top() + 3.0),
                egui::vec2(cell, ROW_H),
            );
            let resp = ui.interact(r, ui.scope_id().with(("bank", b)), Sense::click());
            let letter = ((b'A' + b as u8) as char).to_string();
            let p = ui.painter();
            if b == cur {
                p.rect_stroke(
                    r.shrink2(egui::vec2(4.0, 0.0)),
                    0.0,
                    Stroke::new(2.0, OLED),
                    egui::StrokeKind::Inside,
                );
            } else if resp.hovered() {
                p.rect_filled(
                    r.shrink2(egui::vec2(4.0, 0.0)),
                    0.0,
                    OLED_HOVER,
                );
            }
            p.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                letter,
                font.clone(),
                OLED,
            );
            theme::color_badge(p, r.shrink2(egui::vec2(4.0, 0.0)), theme::BadgeEdge::Bottom, theme::identity_color(b, banks));
            // Dragging slots over a bank tab opens that bank.
            if resp.clicked() || resp.dnd_hover_payload::<SlotDrag>().is_some() {
                pick = Some(b);
            }
        }
        let (r, to) = arrow(
            ui,
            banks as f32 + 1.0,
            ">",
            Some(cur + 1).filter(|&b| b < banks),
        );
        if r.clicked() {
            pick = to;
        }
        if let Some(b) = pick {
            st.bank[side] = b;
            st.active = side;
        }
    }
    let bank = st.bank[side].min(banks - 1);
    let first = bank * BANK;
    let rows = BANK.min(n - first);
    let mut acts = Vec::new();
    let mut scroll = egui::ScrollArea::vertical()
        .id_salt(("elek_slots", side, bank))
        .auto_shrink([false, false]);
    let motion_id = ui.scope_id().with(("pool_ping_scroll", side, bank));
    let mut ping_target = None;
    if let Some(r) = target.filter(|&r| r >= first) {
        let y = (r - first) as f32 * ROW_H;
        let (top, h) = (st.scroll[side], st.view_h[side]);
        let destination = if y < top { Some(y) } else if y + ROW_H > top + h {
            Some(y + ROW_H - h)
        } else { None };
        if st.reveal.is_some() {
            ui.data_mut(|d| d.remove::<(f64, f32, f32)>(motion_id));
            if let Some(y) = destination {
                scroll = scroll.vertical_scroll_offset(y);
            }
        } else {
            ping_target = destination.map(|y| y.clamp(0.0, (rows as f32 * ROW_H - h).max(0.0)));
        }
    }
    if let Some(y) = crate::ui::file_table::ping_scroll(ui, motion_id, st.scroll[side], ping_target) {
        scroll = scroll.vertical_scroll_offset(y);
    }
    let out = egui::Frame::new()
        .fill(OLED_BG)
        .corner_radius(2.0)
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            // show_rows sizes the list with the spacing set *before* it is
            // called, so zero it here, not inside, or the list gets a blank
            // tail of rows × spacing.
            ui.spacing_mut().item_spacing.y = 0.0;
            theme::always_show_scrollbar(ui);
            scroll.show_rows(ui, ROW_H, rows, |ui, range| {
                for i in range {
                    slot_row(ui, st, first + i, &mut acts);
                }
            })
        });
    st.scroll[side] = out.inner.state.clamped_offset().y;
    st.view_h[side] = out.inner.inner_rect.height();
    if side == st.active {
        st.reveal = None;
    }
    if st.ping_scroll == Some(side) {
        st.ping_scroll = None;
    }
    if st.ping.is_some_and(|(_, at)| at.elapsed().as_secs_f32() >= crate::ui::file_table::PING_SECS) {
        st.ping = None;
    }
    let locked = st.edit_locked();
    for a in acts {
        let edits = !matches!(
            a,
            RowAct::Click(..) | RowAct::DragStart(_) | RowAct::Local(..) | RowAct::ShowOriginal(_) | RowAct::Rename(_) | RowAct::Clip(..) | RowAct::DropClip
        );
        if locked && edits {
            st.status = "Wait a moment: the device is busy, and an edit now would be lost".into();
            continue;
        }
        match a {
            RowAct::Click(s, m) => {
                st.active = side;
                st.click(s, m);
                if st.preview && !m.shift && !m.command {
                    if let Some(k) = st.key_at(s) {
                        st.want_local = Some((k, false));
                    }
                }
            }
            RowAct::DragStart(s) => {
                st.slot_drag = true;
                st.active = side;
                if !st.sel.contains(&s) {
                    st.click(s, egui::Modifiers::NONE);
                }
            }
            RowAct::DropOn(s) => {
                st.active = side;
                st.place_sel_at(s);
            }
            RowAct::DropBefore(s) => {
                st.active = side;
                st.move_sel_to(s);
            }
            RowAct::InsertEmpty(s, n) => st.insert_empty(s, n),
            RowAct::CloseGap(s) => st.close_gap(s),
            RowAct::MoveHere(s) => st.move_sel_to(s),
            RowAct::Nudge(d) => st.nudge(d),
            RowAct::Clear(slots) => st.clear(&slots),
            RowAct::ClearUnused => {
                let slots = st.unused_slots();
                st.clear(&slots);
            }
            RowAct::Local(k, open) => st.want_local = Some((k, open)),
            RowAct::ShowOriginal(k) => st.want_original = Some(k),
            RowAct::UpdateChanged(keys) => st.want_update = Some(keys),
            RowAct::Rename(k) => {
                if let Some(path) = st.names.get(&k).cloned() {
                    let name = path.rsplit('/').next().unwrap_or(&path).to_owned();
                    st.renaming = Some((k, path, name, true));
                }
            }
            RowAct::DropFiles(s, files) => {
                st.active = side;
                st.drop_files(s, files);
            }
            RowAct::Clip(slots, cut) => st.set_clip(&slots, cut, ui.ctx()),
            RowAct::Paste(s, shift) => {
                st.active = side;
                st.paste(s, shift);
            }
            RowAct::DropClip => {
                st.clip = None;
                st.status = "Cut cancelled".into();
            }
        }
    }
}

/// One slot row, laid out like the device's file list.
fn slot_row(ui: &mut egui::Ui, st: &ElekState, slot: usize, acts: &mut Vec<RowAct>) {
    if st.project.is_none() { return; }
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::empty());
    let from = st.order[slot] as usize;
    let e = st.entry(from);
    let filling = st.filling.get(&(from as u16));
    let added = st.fills.contains_key(&(from as u16)) || filling.is_some();
    let off = slot == 0;
    let resp = ui.interact(
        rect,
        ui.scope_id().with(("slot", slot)),
        if off {
            Sense::hover()
        } else {
            Sense::click_and_drag()
        },
    );
    let selected = st.sel.contains(&slot);
    let moved = from != slot || added;

    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, 0.0, OLED);
    } else if resp.hovered() && !off {
        painter.rect_filled(rect, 0.0, OLED_HOVER);
    }
    if st.cursor == Some(slot) && !selected {
        painter.rect_stroke(
            rect.shrink(0.5),
            0.0,
            Stroke::new(1.0, OLED_DIM),
            egui::StrokeKind::Inside,
        );
    }
    if let Some((_, at)) = st.ping.filter(|&(pos, _)| pos == slot) {
        crate::ui::file_table::paint_ping(ui, rect, at, theme::DEVICE_ACCENT_BRIGHT);
    }
    let painter = ui.painter();
    if moved {
        painter.rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
            0.0,
            theme::DEVICE_ACCENT_BRIGHT,
        );
    }
    let (fg, dim) = if selected {
        (OLED_BG, Color32::from_rgb(60, 70, 66))
    } else {
        (OLED, OLED_DIM)
    };
    let font = oled_font(TEXT);
    let y = rect.center().y;
    let label_color = if moved && !selected {
        theme::DEVICE_ACCENT_BRIGHT
    } else {
        dim
    };
    let label_w = 46.0;
    painter.text(
        egui::pos2(rect.left() + 8.0, y),
        egui::Align2::LEFT_CENTER,
        format!("{:03}", slot % BANK),
        font.clone(),
        label_color,
    );
    let name = if off {
        "OFF".to_owned()
    } else if let Some(n) = filling {
        format!("+ {n} …")
    } else if e.is_empty() {
        "-".to_owned()
    } else {
        match (st.names.get(&e.key()), st.linked.get(&e.key())) {
            (Some(path), _) => path.rsplit('/').next().unwrap_or(path).to_owned(),
            // Known from our own transfers while the device lookup runs.
            (None, Some((name, ..))) => name.clone(),
            (None, None) => format!("#{:08x}", e.hash),
        }
    };
    let linked = (!off && !e.is_empty()).then(|| st.linked.get(&e.key())).flatten();
    let uses = st.uses(from);
    let size = if e.is_empty() || off {
        String::new()
    } else {
        format!("{:.2}MB", e.size.saturating_sub(16) as f64 / 1_048_576.0)
    };
    let size_w = 58.0;
    let uses_w = 30.0;
    let name_x = rect.left() + 8.0 + label_w;
    // The sample moved and patterns point at it: the push renumbers them.
    let dot_c = if selected { OLED_BG } else { theme::DEVICE_ACCENT_BRIGHT };
    if !off && from != slot && uses > 0 {
        painter.circle_filled(egui::pos2(name_x - 7.0, y), 2.5, dot_c);
    }
    // Name brightness shows the preview cache: full white plays at once,
    // 0.8 opacity still has to be pulled from the device.
    let changed = (!off && !e.is_empty()).then(|| st.changed_local.get(&e.key())).flatten();
    // Cut and waiting for a paste: faded, like cut files.
    let is_cut = st.clip.as_ref().is_some_and(|c| c.cut && !e.is_empty() && c.olds.contains(&(from as u16)));
    let name_c = if selected {
        fg
    } else if is_cut {
        OLED_DIM
    } else if changed.is_some() {
        theme::CHANGED
    } else if linked.is_some() {
        Color32::WHITE
    } else {
        fg.gamma_multiply(0.8)
    };
    let clip = egui::Rect::from_min_max(
        egui::pos2(name_x, rect.top()),
        egui::pos2(rect.right() - size_w - uses_w, rect.bottom()),
    );
    painter.with_clip_rect(clip).text(
        egui::pos2(name_x, y),
        egui::Align2::LEFT_CENTER,
        name,
        font.clone(),
        if (e.is_empty() && filling.is_none()) || off { dim } else { name_c },
    );
    if uses > 0 {
        painter.text(
            egui::pos2(rect.right() - size_w - 4.0, y),
            egui::Align2::RIGHT_CENTER,
            uses.to_string(),
            oled_font(TEXT - 4.0),
            dim,
        );
    }
    painter.text(
        egui::pos2(rect.right() - 8.0, y),
        egui::Align2::RIGHT_CENTER,
        size,
        font,
        fg,
    );

    if off {
        return;
    }
    let info = SlotRowInfo {
        slot,
        from,
        entry: e,
        linked,
        changed: changed.is_some(),
        selected,
        moved,
        uses,
        rect,
    };
    let resp = match changed {
        Some(local) => resp.on_hover_text(format!(
            "Edited since it went to the device: the device still plays the older version.\n{}\nRight-click › Update on device from edited file",
            local.display()
        )),
        None => resp,
    };
    // With preview on, the first click of a double-click already played it.
    if resp.clicked() && !(st.preview && resp.double_clicked()) {
        acts.push(RowAct::Click(slot, ui.input(|i| i.modifiers)));
    }
    if resp.double_clicked() && !st.preview && !e.is_empty() {
        acts.push(RowAct::Local(e.key(), false));
    }
    if resp.drag_started() {
        acts.push(RowAct::DragStart(slot));
        resp.dnd_set_drag_payload(SlotDrag);
    }
    // Drop: by default the selection lands in this slot and the ones after it,
    // swapping with what's there. With ⌃ or ⌥ held it's inserted instead,
    // before this row (top half) or after it (bottom half), shifting the rest.
    let target = slot_list::drop_target(ui, rect, slot);
    if resp.dnd_hover_payload::<SlotDrag>().is_some() {
        let n = st.sel.len().max(1).min(st.order.len() - slot);
        slot_list::paint_drop(ui, rect, slot, target, n);
    }
    slot_file_drop(ui, st, &info, &resp, acts);
    // Our own drag flag too: if anything else cleared egui's payload this
    // frame, the drop still lands.
    let released = resp.contains_pointer() && ui.input(|i| i.pointer.any_released());
    if resp.release_payload::<SlotDrag>().is_some() || (released && st.slot_drag) {
        acts.push(match target {
            DropTarget::Insert(at) => RowAct::DropBefore(at),
            DropTarget::Place(at) => RowAct::DropOn(at),
        });
    }

    resp.context_menu(|ui| slot_menu(ui, st, &info, acts));
    if !e.is_empty() {
        resp.on_hover_text(slot_tooltip(st, &info));
    } else if filling.is_some() {
        resp.on_hover_text("Uploading to the +Drive, then checking the device can find it. Pushing waits for this.");
    }
}

/// Local files from a file pane (or Finder) onto an empty slot.
fn slot_file_drop(ui: &mut egui::Ui, st: &ElekState, row: &SlotRowInfo<'_>, resp: &egui::Response, acts: &mut Vec<RowAct>) {
    let SlotRowInfo { slot, from, rect, .. } = *row;
    if let Some(d) = resp.dnd_hover_payload::<crate::ui::file_table::DragPayload>().filter(|d| !d.device) {
        let free = st.free(from as u16);
        let held: Vec<usize> = d.paths.iter().filter_map(|p| st.slot_of_local(p)).collect();
        let fresh = d.paths.len() - held.len();
        let ok = free && fresh > 0;
        let n = if ok { fresh.min(st.order.len() - slot) } else { 1 };
        let r = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), ROW_H * n as f32));
        let stroke = if ok { Stroke::new(2.0, theme::DEVICE_ACCENT_BRIGHT) } else { theme::deny_frame() };
        ui.painter().rect_stroke(r, 0.0, stroke, egui::StrokeKind::Inside);
        let note = match (free, held.as_slice()) {
            (false, _) => Some("Drop onto an empty slot (\"-\")".to_owned()),
            (true, [one]) if fresh == 0 => Some(format!("Already in the pool (slot {})", st.label(*one))),
            (true, _) if fresh == 0 => Some(format!("All {} already in the pool", held.len())),
            (true, []) => None,
            (true, _) => Some(format!("{} already in the pool, left out", held.len())),
        };
        if let Some(msg) = note {
            theme::pointer_note(ui.ctx(), &msg, if ok { theme::DEVICE_ACCENT } else { theme::DENY });
        }
    }
    if let Some(d) = resp.release_payload::<crate::ui::file_table::DragPayload>().filter(|d| !d.device) {
        acts.push(RowAct::DropFiles(slot, d.paths.clone()));
    }
}
