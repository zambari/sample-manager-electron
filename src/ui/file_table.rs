//! The file list for one directory tab.
//!
//! Rows are virtualized via `ScrollArea::show_rows`, so a folder with 50k
//! samples costs the same per frame as one with 20 -- only the visible slice is
//! laid out. Nothing expensive happens in here: sizes are read from the cache,
//! never computed.

use crate::fs_model::{self, SortKey};
use crate::preview::Preview;
use crate::sizes::{Size, SizeCache};
use crate::tab::DirTab;
use egui::{Align, Key, RichText, Sense, Stroke, StrokeKind, Ui};
use std::path::PathBuf;

pub const ROW_HEIGHT: f32 = 22.0;

/// What the table is dragging. Must be `Any + Send + Sync` for egui's dnd.
#[derive(Clone, Debug)]
pub struct DragPayload {
    pub paths: Vec<PathBuf>,
}

/// Things the table can't do itself because they need app-wide state
/// (filesystem writes, job spawning, cross-pane moves).
pub enum Intent {
    /// Descend into a directory, or activate a file.
    Open(PathBuf),
    /// This pane's rows were clicked, so it should take keyboard focus.
    TookFocus,
    BeginRename(PathBuf),
    CommitRename { from: PathBuf, to: String },
    CancelRename,
    /// Something was dropped onto this tab's list.
    Dropped(Vec<PathBuf>),
    /// User wants the recursive size of these directories.
    WantSize(Vec<PathBuf>),
}

pub struct TableCtx<'a> {
    pub sizes: &'a SizeCache,
    pub preview: &'a Preview,
    pub pane_focused: bool,
}

pub fn show(ui: &mut Ui, tab: &mut DirTab, cx: TableCtx<'_>) -> Vec<Intent> {
    let mut intents = Vec::new();

    header(ui, tab);

    if let Some(err) = &tab.error {
        ui.colored_label(ui.visuals().error_fg_color, err);
        return intents;
    }

    let vis = tab.visible_indices();
    let frame = egui::Frame::NONE;
    let (inner, dropped) = ui.dnd_drop_zone::<DragPayload, _>(frame, |ui| {
        rows(ui, tab, &cx, &vis, &mut intents);
    });
    let _ = inner;

    if let Some(payload) = dropped {
        intents.push(Intent::Dropped(payload.paths.clone()));
    }

    intents
}

fn header(ui: &mut Ui, tab: &mut DirTab) {
    ui.horizontal(|ui| {
        for key in [SortKey::Name, SortKey::Size, SortKey::Modified, SortKey::Kind] {
            let active = tab.sort_key == key;
            let arrow = if !active {
                ""
            } else if tab.sort_asc {
                " ▲"
            } else {
                " ▼"
            };
            let text = format!("{}{arrow}", key.label());
            if ui.selectable_label(active, text).clicked() {
                if active {
                    tab.sort_asc = !tab.sort_asc;
                } else {
                    tab.sort_key = key;
                    tab.sort_asc = true;
                }
                tab.resort();
            }
        }
        ui.separator();
        ui.add(
            egui::TextEdit::singleline(&mut tab.filter)
                .hint_text("filter")
                .desired_width(110.0),
        );
    });
}

fn rows(
    ui: &mut Ui,
    tab: &mut DirTab,
    cx: &TableCtx<'_>,
    vis: &[usize],
    intents: &mut Vec<Intent>,
) {
    let total = vis.len();
    let scroll_target = tab.scroll_to_cursor.then(|| tab.cursor).flatten();
    tab.scroll_to_cursor = false;

    let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
    if let Some(c) = scroll_target {
        // Keep the keyboard cursor on screen without snapping the view around
        // when it's already visible.
        let y = c as f32 * (ROW_HEIGHT + ui.spacing().item_spacing.y);
        area = area.vertical_scroll_offset(y - ROW_HEIGHT * 4.0);
    }

    area.show_rows(ui, ROW_HEIGHT, total, |ui, range| {
        ui.set_min_width(ui.available_width());
        for vis_i in range {
            let idx = vis[vis_i];
            row(ui, tab, cx, vis_i, idx, intents);
        }
    });
}

fn row(
    ui: &mut Ui,
    tab: &mut DirTab,
    cx: &TableCtx<'_>,
    vis_i: usize,
    idx: usize,
    intents: &mut Vec<Intent>,
) {
    let entry = tab.entries[idx].clone();
    let is_selected = tab.selected.contains(&entry.path);
    let is_cursor = tab.cursor == Some(vis_i);
    let renaming = tab
        .renaming
        .as_ref()
        .filter(|(p, _)| *p == entry.path)
        .is_some();

    let full_width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(full_width, ROW_HEIGHT),
        Sense::empty(),
    );

    // Backgrounds first, so text draws on top.
    if is_selected {
        let fill = if cx.pane_focused {
            ui.visuals().selection.bg_fill
        } else {
            // A selection in the unfocused pane stays visible but recedes --
            // important here, because the source selection is what Enter acts
            // on even while the destination pane has focus.
            ui.visuals().selection.bg_fill.gamma_multiply(0.45)
        };
        ui.painter().rect_filled(rect, 3.0, fill);
    }
    if is_cursor && cx.pane_focused {
        ui.painter().rect_stroke(
            rect,
            3.0,
            Stroke::new(1.0, ui.visuals().strong_text_color()),
            StrokeKind::Inside,
        );
    }

    if renaming {
        rename_editor(ui, rect, tab, &entry.path, intents);
        return;
    }

    // Row contents, laid out inside the rect we already allocated.
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(6.0, 0.0)))
            .layout(egui::Layout::left_to_right(Align::Center)),
    );
    draw_cells(&mut content, cx, &entry, tab);

    // One interaction region covering the whole row, sensing both so a click
    // selects and a press-and-move starts a drag.
    let id = ui.scope_id().with(("row", idx));
    let resp = ui.interact(rect, id, Sense::click_and_drag());

    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    if resp.drag_started() {
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
            tab.selected.clear();
            tab.selected.insert(entry.path.clone());
            tab.cursor = Some(vis_i);
            tab.anchor = Some(vis_i);
        }
    }

    if resp.double_clicked() {
        intents.push(Intent::Open(entry.path.clone()));
    }

    resp.context_menu(|ui| {
        if ui.button("Rename").clicked() {
            intents.push(Intent::BeginRename(entry.path.clone()));
            ui.close();
        }
        if entry.is_dir && ui.button("Calculate size").clicked() {
            intents.push(Intent::WantSize(vec![entry.path.clone()]));
            ui.close();
        }
        if entry.is_dir && ui.button("Open").clicked() {
            intents.push(Intent::Open(entry.path.clone()));
            ui.close();
        }
    });
}

fn draw_cells(ui: &mut Ui, cx: &TableCtx<'_>, entry: &fs_model::Entry, tab: &DirTab) {
    let icon = if entry.is_dir {
        "📁"
    } else if cx.preview.is_playing(&entry.path) {
        "🔊"
    } else if entry.is_audio {
        "🎵"
    } else {
        "·"
    };
    ui.label(icon);

    let name = RichText::new(&entry.name);
    let name = if entry.is_dir { name.strong() } else { name };
    ui.label(name);

    // Right-aligned metadata.
    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
        let size_text = if entry.is_dir {
            match cx.sizes.get(&entry.path) {
                Some(Size::Known { bytes, files }) => {
                    format!("{} · {files} files", fs_model::human_bytes(bytes))
                }
                Some(Size::Pending) => "calculating…".to_owned(),
                // Dash rather than 0: we genuinely don't know yet, and
                // claiming 0 would be a lie.
                None => "—".to_owned(),
            }
        } else {
            fs_model::human_bytes(entry.len)
        };
        ui.add(
            egui::Label::new(RichText::new(size_text).weak().monospace())
                .selectable(false),
        );
        let _ = tab;
    });
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
    let Some((_, current)) = tab.renaming.as_ref() else { return };
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

    let enter = child.input(|i| i.key_pressed(Key::Enter));
    let esc = child.input(|i| i.key_pressed(Key::Escape));

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
