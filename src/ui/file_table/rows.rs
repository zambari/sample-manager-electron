//! Virtualized file rows, inline rename, selection and folder-drop interactions.

use super::*;

pub(super) fn rows(ui: &mut Ui, tab: &mut DirTab, cx: &TableCtx<'_>, intents: &mut Vec<Intent>) {
    // A ".." row leads the list whenever there is somewhere to go up to. It is
    // not an entry: it can't be selected, dragged or renamed.
    let up = usize::from(tab.parent().is_some());
    let total = tab.entries.len() + up;
    let scroll_target = tab
        .scroll_to_cursor
        .then(|| tab.cursor.map(|c| c + up))
        .flatten();
    tab.scroll_to_cursor = false;

    let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
    let motion_id = ui.scope_id().with("ping_scroll");
    let mut ping_target = None;
    if let Some(c) = scroll_target {
        ui.data_mut(|d| d.remove::<(f64, f32, f32)>(motion_id));
        // Keep the keyboard cursor on screen without snapping the view around
        // when it's already visible.
        let y = c as f32 * (ROW_HEIGHT + ui.spacing().item_spacing.y);
        area = area.vertical_scroll_offset(y - ROW_HEIGHT * 4.0);
    } else if tab.scroll_to_ping {
        // A pinged row: scroll only if it's out of view, then centre it.
        let at = match &tab.ping {
            Some((Some(p), _)) => tab.entries.iter().position(|e| &e.path == p).map(|i| i + up),
            Some((None, _)) => Some(0),
            None => None,
        };
        if let Some(i) = at {
            let row_h = ROW_HEIGHT + ui.spacing().item_spacing.y;
            let y = i as f32 * row_h;
            let (top, h) = tab.view;
            if y < top || y + row_h > top + h {
                ping_target = Some((y - (h - row_h) / 2.0).clamp(0.0, (total as f32 * row_h - h).max(0.0)));
            }
        }
    }
    tab.scroll_to_ping = false;
    if let Some(y) = ping_scroll(ui, motion_id, tab.view.0, ping_target) {
        area = area.vertical_scroll_offset(y);
    }

    // Empty space below (and behind) the rows: right-click to paste into the
    // folder being shown. Registered before the rows, so rows win the hit.
    let bg_id = ui.scope_id().with("table_bg");
    let bg = ui.interact(ui.available_rect_before_wrap(), bg_id, Sense::click());
    if bg.secondary_clicked() {
        intents.push(Intent::TookFocus);
    }
    bg.context_menu(|ui| {
        let with = if tab.device { Vec::new() } else { tab.effective_selection() };
        new_folder_button(ui, cx, tab.cwd.clone(), None, with, intents);
        paste_button(ui, cx, None, intents);
        if !tab.device {
            reveal_button(ui, tab.cwd.clone(), intents);
        }
    });

    // Device listings sit on a dark "screen", like the Elek Sample Order view,
    // so it's obvious at a glance which side is the device.
    if tab.device {
        ui.painter()
            .rect_filled(ui.available_rect_before_wrap(), 2.0, OLED_BG);
    }

    let out = area.show_rows(ui, ROW_HEIGHT, total, |ui, range| {
        ui.set_min_width(ui.available_width());
        for i in range {
            if i < up {
                parent_row(ui, tab, intents);
            } else if i - up >= tab.entries.len() {
                // A row can collapse a folder mid-loop, shrinking the list
                // under `range`. The next frame lays it out afresh.
                ui.ctx().request_repaint();
                break;
            } else {
                row(ui, tab, cx, i - up, intents);
            }
        }
    });
    tab.view = (out.state.clamped_offset().y, out.inner_rect.height());
    if tab.ping.as_ref().is_some_and(|(_, at)| at.elapsed().as_secs_f32() >= PING_SECS) {
        tab.ping = None;
    }

    // Fire an armed slow-click rename once its double-click window is over,
    // provided that row is still the only thing selected.
    if let Some((path, at)) = tab.slow_click_rename.clone() {
        let now = ui.input(|i| i.time);
        let still = tab.selected.len() == 1 && tab.selected.contains(&path);
        if !still || tab.renaming.is_some() {
            tab.slow_click_rename = None;
        } else if now >= at {
            tab.slow_click_rename = None;
            intents.push(Intent::BeginRename(path));
        } else {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(at - now));
        }
    }
}

fn parent_row(ui: &mut Ui, tab: &DirTab, intents: &mut Vec<Intent>) {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    // Dropping onto ".." moves the items up into the parent folder.
    if let Some(parent) = tab.parent() {
        if resp.dnd_hover_payload::<DragPayload>().is_some() {
            let color = crate::ui::theme::ACCENT_BRIGHT;
            ui.painter()
                .rect_stroke(rect, 2.0, Stroke::new(2.0, color), StrokeKind::Inside);
        }
        if let Some(p) = resp.release_payload::<DragPayload>() {
            intents.push(Intent::DroppedOn {
                dir: parent,
                payload: (*p).clone(),
            });
            return;
        }
    }
    if resp.hovered() {
        let fill = if tab.device {
            OLED_HOVER
        } else {
            ui.visuals().widgets.hovered.weak_bg_fill
        };
        ui.painter().rect_filled(rect, 2.0, fill);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if let Some((None, at)) = &tab.ping {
        paint_ping(ui, rect, *at, crate::ui::theme::ACCENT_BRIGHT);
    }
    // Painted, not label widgets, so the text can't swallow clicks.
    let inner = rect.shrink2(egui::vec2(6.0, 0.0));
    if tab.device {
        paint_text(ui, inner, "..", oled_font(OLED_TEXT), OLED_DIM);
    } else {
        paint_text(ui, inner, "..", body_font(ui, 1.0), egui::Color32::WHITE);
    }
    // Double click, like any other folder row.
    if resp.on_hover_text("Parent folder").double_clicked() {
        intents.push(Intent::GoUp);
    }
}

fn row(ui: &mut Ui, tab: &mut DirTab, cx: &TableCtx<'_>, vis_i: usize, intents: &mut Vec<Intent>) {
    let idx = vis_i;
    let entry = tab.entries[idx].clone();
    let depth = tab.depths.get(idx).copied().unwrap_or(0);
    let is_selected = tab.selected.contains(&entry.path);
    let is_cursor = tab.cursor == Some(vis_i);
    let renaming = tab
        .renaming
        .as_ref()
        .filter(|(p, _)| *p == entry.path)
        .is_some();

    let full_width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(full_width, ROW_HEIGHT), Sense::empty());

    // Backgrounds first, so text draws on top. The hover fill isn't known
    // until the row's response exists, so reserve a slot for it here.
    let bg = ui.painter().add(egui::Shape::Noop);
    if is_selected {
        let fill = if tab.device {
            // Inverted, like a highlighted line on the device screen.
            if cx.pane_focused {
                OLED
            } else {
                OLED_UNFOCUSED_SEL
            }
        } else if cx.pane_focused {
            ui.visuals().selection.bg_fill
        } else {
            // A selection in the unfocused pane stays visible but recedes --
            // important here, because the source selection is what Enter acts
            // on even while the destination pane has focus.
            ui.visuals().selection.bg_fill.gamma_multiply(0.45)
        };
        ui.painter().rect_filled(rect, 2.0, fill);
    }
    if let Some(mark) = cx.saving.get(&entry.path) {
        paint_save_mark(ui, rect, *mark);
    }
    if let Some((Some(p), at)) = &tab.ping {
        if *p == entry.path {
            paint_ping(ui, rect, *at, crate::ui::theme::ACCENT_BRIGHT);
        }
    }
    if is_cursor && cx.pane_focused {
        let color = if tab.device {
            OLED_DIM
        } else {
            ui.visuals().strong_text_color()
        };
        ui.painter().rect_stroke(
            rect,
            2.0,
            Stroke::new(1.0, color),
            StrokeKind::Inside,
        );
    }

    // Row contents, laid out inside the rect we already allocated. In tree
    // mode each level indents, and folders get a disclosure arrow.
    const INDENT: f32 = 16.0;
    const ARROW_W: f32 = 14.0;

    if renaming {
        // Start the field where the name text starts (past indent, arrow and
        // icon), so entering rename doesn't shift the name sideways.
        let mut left = rect.left() + 6.0 + depth as f32 * INDENT;
        if tab.tree {
            left += ARROW_W + 2.0;
        }
        left += if tab.device {
            13.0 + 7.0
        } else {
            let icon_w = ui
                .painter()
                .layout_no_wrap("📁".to_owned(), body_font(ui, 0.0), egui::Color32::PLACEHOLDER)
                .size()
                .x;
            icon_w + ui.spacing().item_spacing.x
        };
        // The field's own inner padding, so its text lines up with the name.
        left -= 4.0;
        let mut field_rect = rect;
        field_rect.min.x = (left - 6.0).clamp(rect.left(), rect.right() - 40.0);
        rename_editor(ui, field_rect, tab, &entry.path, intents);
        return;
    }

    let mut inner = rect.shrink2(egui::vec2(6.0, 0.0));
    inner.min.x += depth as f32 * INDENT;
    let mut arrow_rect = None;
    if tab.tree {
        let r = egui::Rect::from_min_size(inner.min, egui::vec2(ARROW_W, rect.height()));
        if entry.is_dir {
            let glyph = if tab.is_expanded(&entry.path) {
                "⏷"
            } else {
                "⏵"
            };
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                glyph,
                egui::FontId::proportional(12.0),
                if !tab.device {
                    ui.visuals().text_color()
                } else if is_selected {
                    OLED_BG
                } else {
                    OLED_DIM
                },
            );
            arrow_rect = Some(r);
        }
        inner.min.x += ARROW_W + 2.0;
    }
    let shown = tab.display_name(idx).map(str::to_owned);
    if tab.device {
        draw_device_cells(ui, inner, cx, &entry, shown.as_deref(), is_selected);
    } else {
        draw_cells(ui, inner, cx, &entry, shown.as_deref(), false);
    }

    // One interaction region covering the whole row, sensing both so a click
    // selects and a press-and-move starts a drag.
    let id = ui.scope_id().with(("row", idx));
    let mut resp = ui.interact(rect, id, Sense::click_and_drag());
    if shown.is_some() {
        resp = resp.on_hover_text(&entry.name);
    }

    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        // Highlight the full row so it's clear the whole width is a target.
        // Painted behind the text (which was laid out before we knew).
        if !is_selected {
            let hover = if tab.device {
                OLED_HOVER
            } else {
                ui.visuals().widgets.hovered.weak_bg_fill
            };
            ui.painter()
                .set(bg, egui::Shape::rect_filled(rect, 2.0, hover));
        }
    }

    if folder_drop(ui, tab, &entry, &resp, intents) {
        return;
    }

    // The arrow sits on top of the row, so it's checked after (egui gives the
    // later widget the hit).
    if let Some(r) = arrow_rect {
        let a = ui.interact(r, ui.scope_id().with(("arrow", idx)), Sense::click());
        if a.clicked() {
            intents.push(Intent::TookFocus);
            if tab.is_expanded(&entry.path) {
                tab.collapse(&entry.path);
            } else {
                intents.push(Intent::Expand(entry.path.clone()));
            }
            return;
        }
    }

    row_interactions(ui, tab, cx, vis_i, &entry, &resp, intents);
    resp.context_menu(|ui| row_menu(ui, tab, cx, &entry, intents));
}

/// Claims a folder-row drop before the list behind it can consume it.
/// Returns whether a payload was consumed, including a refused self-drop.
fn folder_drop(
    ui: &Ui,
    tab: &DirTab,
    entry: &fs_model::Entry,
    resp: &egui::Response,
    intents: &mut Vec<Intent>,
) -> bool {
    // Folder rows take drops themselves; the list behind them only gets what
    // lands elsewhere (the row claims the payload first). The right third of
    // a folder row means "drop here" -- into the folder the row sits in, not
    // into the folder itself -- since a list full of folders otherwise leaves
    // nowhere to aim at.
    if !entry.is_dir {
        return false;
    }
    let rect = resp.rect;
    let here_zone = egui::Rect::from_min_max(
        egui::pos2(rect.right() - rect.width() / 3.0, rect.top()),
        rect.max,
    );
    let in_here_zone = ui
        .ctx()
        .pointer_interact_pos()
        .is_some_and(|p| here_zone.contains(p));
    let target = if in_here_zone {
        entry.path.parent().map(PathBuf::from)
    } else {
        Some(entry.path.clone())
    };
    let onto_self = |p: &DragPayload| {
        !in_here_zone && p.device == tab.device && p.paths.contains(&entry.path)
    };
    // Only outline the folder itself; the "here" zone gets no box, since a
    // box on one row reads as "into this row" rather than "into the parent".
    if !in_here_zone
        && resp
            .dnd_hover_payload::<DragPayload>()
            .is_some_and(|p| !onto_self(&p))
    {
        let color = crate::ui::theme::ACCENT_BRIGHT;
        ui.painter()
            .rect_stroke(rect, 2.0, Stroke::new(2.0, color), StrokeKind::Inside);
    }
    if let Some(p) = resp.release_payload::<DragPayload>() {
        if let Some(dir) = target.filter(|_| !onto_self(&p)) {
            intents.push(Intent::DroppedOn {
                dir,
                payload: (*p).clone(),
            });
        }
        return true;
    }
    false
}

/// Selection, dragging and Finder-style delayed rename share one row response.
fn row_interactions(
    ui: &Ui,
    tab: &mut DirTab,
    cx: &TableCtx<'_>,
    vis_i: usize,
    entry: &fs_model::Entry,
    resp: &egui::Response,
    intents: &mut Vec<Intent>,
) {
    let is_selected = tab.selected.contains(&entry.path);
    if resp.drag_started() {
        tab.slow_click_rename = None;
        // If the row isn't part of the selection, dragging it selects it first
        // -- otherwise you'd drag something you can't see highlighted.
        if !is_selected {
            tab.selected.clear();
            tab.selected.insert(entry.path.clone());
            tab.cursor = Some(vis_i);
            tab.anchor = Some(vis_i);
        }
        resp.dnd_set_drag_payload(DragPayload {
            paths: tab.effective_selection(),
            device: tab.device,
            external: false,
        });
    }

    if resp.clicked() {
        intents.push(Intent::TookFocus);
        let mods = ui.input(|i| i.modifiers);
        if mods.command {
            tab.toggle_at(vis_i);
            tab.cursor = Some(vis_i);
            tab.anchor = Some(vis_i);
        } else if mods.shift {
            let anchor = tab.anchor.unwrap_or(vis_i);
            tab.select_range(anchor, vis_i);
            tab.cursor = Some(vis_i);
        } else {
            // Clicking the row that is already the only selection (in the
            // focused pane) renames it, Finder-style -- but only after the
            // double-click window, so a double-click still opens.
            let sole = is_selected && tab.selected.len() == 1;
            tab.slow_click_rename = (sole && cx.pane_focused).then(|| {
                let wait = ui.ctx().options(|o| o.input_options.max_double_click_delay);
                (entry.path.clone(), ui.input(|i| i.time) + wait)
            });
            tab.selected.clear();
            tab.selected.insert(entry.path.clone());
            tab.cursor = Some(vis_i);
            tab.anchor = Some(vis_i);
        }
    }

    if resp.double_clicked() {
        tab.slow_click_rename = None;
        intents.push(Intent::Open(entry.path.clone()));
    }

    // Right-clicking outside the selection selects that row first, like
    // Finder, so the menu always acts on what's highlighted.
    if resp.secondary_clicked() {
        intents.push(Intent::TookFocus);
        if !is_selected {
            tab.selected.clear();
            tab.selected.insert(entry.path.clone());
            tab.cursor = Some(vis_i);
            tab.anchor = Some(vis_i);
        }
    }
}

fn rename_editor(
    ui: &mut Ui,
    rect: egui::Rect,
    tab: &mut DirTab,
    path: &std::path::Path,
    intents: &mut Vec<Intent>,
) {
    // Edit a local copy and write it back: handing the field straight to
    // `TextEdit` would borrow `tab` for the rest of the function.
    let Some((_, current)) = tab.renaming.as_ref() else {
        return;
    };
    let mut buf = current.clone();

    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(6.0, 1.0)))
            .layout(egui::Layout::left_to_right(Align::Center)),
    );
    let id = ui.scope_id().with(("rename", path));
    let resp = child.add(
        egui::TextEdit::singleline(&mut buf)
            .id(id)
            .desired_width(f32::INFINITY),
    );

    // On the first frame the editor exists, take focus and preselect the stem
    // so typing replaces the name but keeps the extension -- Finder behaviour.
    if !resp.has_focus() && !resp.lost_focus() {
        child.memory_mut(|m| m.request_focus(id));
        if let Some(mut state) = egui::TextEdit::load_state(child.ctx(), id) {
            let stem_len = std::path::Path::new(buf.as_str())
                .file_stem()
                .map(|s| s.to_string_lossy().chars().count())
                .unwrap_or_else(|| buf.chars().count());
            let range = egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(stem_len),
            );
            state.cursor.set_char_range(Some(range));
            state.store(child.ctx(), id);
        }
    }

    // Consumed, so the app's own Enter (commit) doesn't also fire once the
    // rename has closed later this frame.
    let (enter, esc) = child.input_mut(|i| {
        let m = egui::Modifiers::NONE;
        (i.consume_key(m, Key::Enter), i.consume_key(m, Key::Escape))
    });

    if esc {
        intents.push(Intent::CancelRename);
    } else if enter {
        intents.push(Intent::CommitRename {
            from: path.to_path_buf(),
            to: buf,
        });
    } else if resp.lost_focus() {
        // Clicking away cancels rather than silently renaming.
        intents.push(Intent::CancelRename);
    } else if let Some((_, slot)) = tab.renaming.as_mut() {
        *slot = buf;
    }
}
