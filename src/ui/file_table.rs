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
use egui::{Align, Key, Sense, Stroke, StrokeKind, Ui};
use std::path::PathBuf;

pub const ROW_HEIGHT: f32 = 22.0;

/// What the table is dragging. Must be `Any + Send + Sync` for egui's dnd.
#[derive(Clone, Debug)]
pub struct DragPayload {
    pub paths: Vec<PathBuf>,
    /// The paths are on the device.
    pub device: bool,
}

/// Things the table can't do itself because they need app-wide state
/// (filesystem writes, job spawning, cross-pane moves).
pub enum Intent {
    /// Descend into a directory, or activate a file.
    Open(PathBuf),
    /// This pane's rows were clicked, so it should take keyboard focus.
    TookFocus,
    BeginRename(PathBuf),
    /// Open find-and-replace renaming over the selection.
    BeginMultiRename,
    CommitRename { from: PathBuf, to: String },
    CancelRename,
    /// Something was dropped onto this tab's list.
    Dropped(DragPayload),
    /// Something was dropped onto a folder row.
    DroppedOn { dir: PathBuf, payload: DragPayload },
    /// User wants the recursive size of these directories.
    WantSize(Vec<PathBuf>),
    /// The ".." row was activated.
    GoUp,
    /// Expand this folder in tree mode (needs a listing request).
    Expand(PathBuf),
    AddFavourite(PathBuf),
    /// Move these to the Trash (no confirmation; it can be undone).
    Trash(Vec<PathBuf>),
    /// Delete these from the device right away, skipping /trash (after
    /// confirmation).
    DeleteNow(Vec<PathBuf>),
    /// Put these on the clipboard to be moved on the next Paste.
    Cut(Vec<PathBuf>),
    /// Put these on the clipboard to be copied on the next Paste.
    Copy(Vec<PathBuf>),
    /// Paste the clipboard into this folder (`None`: the tab's current folder).
    Paste(Option<PathBuf>),
    /// Show this item in Finder / Explorer.
    Reveal(PathBuf),
    /// Find identical files among these and offer to trash the extras.
    RemoveDuplicates(Vec<PathBuf>),
    /// Make a new folder in this directory and start renaming it.
    NewFolder(PathBuf),
    /// Make a folder with this exact name in this directory.
    NewFolderNamed { dir: PathBuf, name: String },
    /// Fetch this device file and open it in the editor (after a warning).
    LoadFromDevice(PathBuf),
}

pub struct TableCtx<'a> {
    pub sizes: &'a SizeCache,
    pub preview: &'a Preview,
    /// Files already loaded into the editor; drawn gray.
    pub opened: &'a std::collections::HashSet<PathBuf>,
    /// Files already committed to a destination; drawn with a check badge.
    pub committed: &'a std::collections::HashSet<PathBuf>,
    pub pane_focused: bool,
    /// The clipboard, for dimming cut items and enabling Paste.
    pub clipboard: Option<&'a crate::ops::Clipboard>,
    /// Name of the folder the other pane is showing, offered as a new-folder name.
    pub other_folder: Option<String>,
}

impl TableCtx<'_> {
    fn is_cut(&self, p: &std::path::Path, device: bool) -> bool {
        self.clipboard.is_some_and(|c| {
            c.kind == crate::ops::Kind::Move && c.device == device && c.paths.iter().any(|x| x == p)
        })
    }

    fn paste_label(&self) -> Option<String> {
        let c = self.clipboard?;
        let n = c.paths.len();
        let what = if n == 1 { "1 item".to_owned() } else { format!("{n} items") };
        Some(match c.kind {
            crate::ops::Kind::Move => format!("Move {what} here"),
            crate::ops::Kind::Copy => format!("Paste {what}"),
        })
    }
}

pub fn show(ui: &mut Ui, tab: &mut DirTab, cx: TableCtx<'_>) -> Vec<Intent> {
    let mut intents = Vec::new();

    header(ui, tab);

    if let Some(err) = &tab.error {
        ui.colored_label(ui.visuals().error_fg_color, err);
        return intents;
    }

    let frame = egui::Frame::NONE;
    let (inner, dropped) = ui.dnd_drop_zone::<DragPayload, _>(frame, |ui| {
        rows(ui, tab, &cx, &mut intents);
    });
    let _ = inner;

    if let Some(payload) = dropped {
        intents.push(Intent::Dropped((*payload).clone()));
    }

    intents
}

fn header(ui: &mut Ui, tab: &mut DirTab) {
    // Wraps rather than forcing a minimum width, so the column can be
    // dragged narrower than a full row of controls.
    ui.horizontal_wrapped(|ui| {
        // One discreet label that cycles the sort key; the arrow flips direction.
        const KEYS: [SortKey; 4] = [SortKey::Name, SortKey::Size, SortKey::Modified, SortKey::Kind];
        let gap = ui.spacing().item_spacing.x;
        ui.spacing_mut().item_spacing.x = 2.0;
        let label = egui::RichText::new(format!("Sort by {}", tab.sort_key.label()))
            .small()
            .weak();
        let r = ui
            .add(egui::Label::new(label).sense(egui::Sense::click()))
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Click to change sort (right-click to go back)");
        let step = if r.clicked() {
            1
        } else if r.secondary_clicked() {
            KEYS.len() - 1
        } else {
            0
        };
        if step != 0 {
            let i = KEYS.iter().position(|k| *k == tab.sort_key).unwrap_or(0);
            tab.sort_key = KEYS[(i + step) % KEYS.len()];
            tab.sort_asc = true;
            tab.resort();
        }
        let arrow = egui::RichText::new(if tab.sort_asc { "▲" } else { "▼" })
            .small()
            .weak();
        if ui
            .add(egui::Label::new(arrow).sense(egui::Sense::click()))
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text("Toggle ascending / descending")
            .clicked()
        {
            tab.sort_asc = !tab.sort_asc;
            tab.resort();
        }
        ui.spacing_mut().item_spacing.x = gap;
        ui.separator();
        let filter = ui.add(
            egui::TextEdit::singleline(&mut tab.filter)
                .hint_text("filter")
                .desired_width(110.0),
        );
        if filter.changed() {
            tab.rebuild();
        }
        ui.separator();
        if ui
            .selectable_label(tab.tree, "🌲 Tree")
            .on_hover_text("Show subfolders inline; click ⏵ or press → to expand")
            .clicked()
        {
            tab.set_tree(!tab.tree);
        }
        if ui
            .checkbox(&mut tab.show_all, "All files")
            .on_hover_text("Also list files the editor can't open")
            .changed()
        {
            tab.rebuild();
        }
        if ui
            .checkbox(&mut tab.shorten, "Shorten")
            .on_hover_text("When 10+ files share a long prefix, show it as X(...) — hover a row for the full name")
            .changed()
        {
            tab.rebuild();
        }
    });
}

fn rows(
    ui: &mut Ui,
    tab: &mut DirTab,
    cx: &TableCtx<'_>,
    intents: &mut Vec<Intent>,
) {
    // A ".." row leads the list whenever there is somewhere to go up to. It is
    // not an entry: it can't be selected, dragged or renamed.
    let up = usize::from(tab.parent().is_some());
    let total = tab.entries.len() + up;
    let scroll_target = tab.scroll_to_cursor.then(|| tab.cursor.map(|c| c + up)).flatten();
    tab.scroll_to_cursor = false;

    let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
    if let Some(c) = scroll_target {
        // Keep the keyboard cursor on screen without snapping the view around
        // when it's already visible.
        let y = c as f32 * (ROW_HEIGHT + ui.spacing().item_spacing.y);
        area = area.vertical_scroll_offset(y - ROW_HEIGHT * 4.0);
    }

    // Empty space below (and behind) the rows: right-click to paste into the
    // folder being shown. Registered before the rows, so rows win the hit.
    let bg_id = ui.scope_id().with("table_bg");
    let bg = ui.interact(ui.available_rect_before_wrap(), bg_id, Sense::click());
    if bg.secondary_clicked() {
        intents.push(Intent::TookFocus);
    }
    bg.context_menu(|ui| {
        new_folder_button(ui, cx, tab.cwd.clone(), intents);
        paste_button(ui, cx, None, intents);
        if !tab.device {
            reveal_button(ui, tab.cwd.clone(), intents);
        }
    });

    area.show_rows(ui, ROW_HEIGHT, total, |ui, range| {
        ui.set_min_width(ui.available_width());
        for i in range {
            if i < up {
                parent_row(ui, tab, intents);
            } else {
                row(ui, tab, cx, i - up, intents);
            }
        }
    });
}

fn parent_row(ui: &mut Ui, tab: &DirTab, intents: &mut Vec<Intent>) {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_HEIGHT),
        Sense::click(),
    );
    // Dropping onto ".." moves the items up into the parent folder.
    if let Some(parent) = tab.parent() {
        if resp.dnd_hover_payload::<DragPayload>().is_some() {
            let color = crate::ui::theme::ACCENT_BRIGHT;
            ui.painter().rect_stroke(rect, 2.0, Stroke::new(2.0, color), StrokeKind::Inside);
        }
        if let Some(p) = resp.dnd_release_payload::<DragPayload>() {
            intents.push(Intent::DroppedOn { dir: parent, payload: (*p).clone() });
            return;
        }
    }
    if resp.hovered() {
        ui.painter().rect_filled(rect, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    // Painted, not label widgets, so the text can't swallow clicks.
    let inner = rect.shrink2(egui::vec2(6.0, 0.0));
    paint_text(ui, inner, "..", body_font(ui, 1.0), egui::Color32::WHITE);
    // Double click, like any other folder row.
    if resp.on_hover_text("Parent folder").double_clicked() {
        intents.push(Intent::GoUp);
    }
}

fn row(
    ui: &mut Ui,
    tab: &mut DirTab,
    cx: &TableCtx<'_>,
    vis_i: usize,
    intents: &mut Vec<Intent>,
) {
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
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(full_width, ROW_HEIGHT),
        Sense::empty(),
    );

    // Backgrounds first, so text draws on top. The hover fill isn't known
    // until the row's response exists, so reserve a slot for it here.
    let bg = ui.painter().add(egui::Shape::Noop);
    if is_selected {
        let fill = if cx.pane_focused {
            ui.visuals().selection.bg_fill
        } else {
            // A selection in the unfocused pane stays visible but recedes --
            // important here, because the source selection is what Enter acts
            // on even while the destination pane has focus.
            ui.visuals().selection.bg_fill.gamma_multiply(0.45)
        };
        ui.painter().rect_filled(rect, 2.0, fill);
    }
    if is_cursor && cx.pane_focused {
        ui.painter().rect_stroke(
            rect,
            2.0,
            Stroke::new(1.0, ui.visuals().strong_text_color()),
            StrokeKind::Inside,
        );
    }

    if renaming {
        rename_editor(ui, rect, tab, &entry.path, intents);
        return;
    }

    // Row contents, laid out inside the rect we already allocated. In tree
    // mode each level indents, and folders get a disclosure arrow.
    const INDENT: f32 = 16.0;
    const ARROW_W: f32 = 14.0;
    let mut inner = rect.shrink2(egui::vec2(6.0, 0.0));
    inner.min.x += depth as f32 * INDENT;
    let mut arrow_rect = None;
    if tab.tree {
        let r = egui::Rect::from_min_size(inner.min, egui::vec2(ARROW_W, rect.height()));
        if entry.is_dir {
            let glyph = if tab.is_expanded(&entry.path) { "⏷" } else { "⏵" };
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                glyph,
                egui::FontId::proportional(12.0),
                ui.visuals().text_color(),
            );
            arrow_rect = Some(r);
        }
        inner.min.x += ARROW_W + 2.0;
    }
    let shown = tab.display_name(idx).map(str::to_owned);
    draw_cells(ui, inner, cx, &entry, shown.as_deref(), tab.device);

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
            let hover = ui.visuals().widgets.hovered.weak_bg_fill;
            ui.painter().set(bg, egui::Shape::rect_filled(rect, 2.0, hover));
        }
    }

    // Folder rows take drops themselves; the list behind them only gets what
    // lands elsewhere (the row claims the payload first). The right third of
    // a folder row means "drop here" -- into the folder the row sits in, not
    // into the folder itself -- since a list full of folders otherwise leaves
    // nowhere to aim at.
    if entry.is_dir {
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
        if resp.dnd_hover_payload::<DragPayload>().is_some_and(|p| !onto_self(&p)) {
            let color = crate::ui::theme::ACCENT_BRIGHT;
            if in_here_zone {
                ui.painter().rect_filled(here_zone, 2.0, color.gamma_multiply(0.15));
                ui.painter().rect_stroke(here_zone, 2.0, Stroke::new(2.0, color), StrokeKind::Inside);
                ui.painter().text(
                    here_zone.center(),
                    egui::Align2::CENTER_CENTER,
                    "drop here",
                    egui::FontId::proportional(11.0),
                    color,
                );
            } else {
                ui.painter().rect_stroke(rect, 2.0, Stroke::new(2.0, color), StrokeKind::Inside);
            }
        }
        if let Some(p) = resp.dnd_release_payload::<DragPayload>() {
            if let Some(dir) = target.filter(|_| !onto_self(&p)) {
                intents.push(Intent::DroppedOn { dir, payload: (*p).clone() });
            }
            return;
        }
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
            device: tab.device,
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

    resp.context_menu(|ui| {
        // Rename and Open only make sense for one item.
        let single = tab.selected.len() <= 1;
        let rename = ui.add_enabled(single, egui::Button::new("Rename"));
        let rename = if single {
            rename.on_hover_text("Rename in place (F2)")
        } else {
            rename.on_disabled_hover_text("Several items selected -- use the Multi-Rename Tool")
        };
        if rename.clicked() {
            intents.push(Intent::BeginRename(entry.path.clone()));
            ui.close();
        }
        if ui.button("Multi-Rename Tool…").on_hover_text("Find and replace across the selected names (⇧F2)").clicked() {
            intents.push(Intent::BeginMultiRename);
            ui.close();
        }
        let local = !tab.device;
        if local && entry.is_dir && ui.button("Calculate size").clicked() {
            intents.push(Intent::WantSize(vec![entry.path.clone()]));
            ui.close();
        }
        if local && entry.is_dir && ui.button("Add to favourites").clicked() {
            intents.push(Intent::AddFavourite(entry.path.clone()));
            ui.close();
        }
        if single && entry.is_dir && ui.button("Open").clicked() {
            intents.push(Intent::Open(entry.path.clone()));
            ui.close();
        }
        if single && !local && !entry.is_dir && ui.button("Load to editor…").clicked() {
            intents.push(Intent::LoadFromDevice(entry.path.clone()));
            ui.close();
        }
        // Next to the row, i.e. in the folder the row sits in.
        if let Some(parent) = entry.path.parent() {
            new_folder_button(ui, cx, parent.to_path_buf(), intents);
        }
        ui.separator();
        let sel = tab.effective_selection();
        if ui.button("Cut").clicked() {
            intents.push(Intent::Cut(sel.clone()));
            ui.close();
        }
        if ui.button("Copy").clicked() {
            intents.push(Intent::Copy(sel.clone()));
            ui.close();
        }
        // On a folder row, paste goes into that folder; on a file, alongside it.
        let target = entry.is_dir.then(|| entry.path.clone());
        paste_button(ui, cx, target, intents);
        ui.separator();
        // Only meaningful with several files selected.
        let files: Vec<PathBuf> =
            if local { sel.iter().filter(|p| !p.is_dir()).cloned().collect() } else { Vec::new() };
        if files.len() > 1 {
            if ui
                .button("Remove duplicates…")
                .on_hover_text("Find identical files in the selection, keep the shortest name of each, trash the rest")
                .clicked()
            {
                intents.push(Intent::RemoveDuplicates(files));
                ui.close();
            }
            ui.separator();
        }
        if local {
            reveal_button(ui, entry.path.clone(), intents);
            ui.separator();
        }
        let bin = if local { "Trash" } else { "/trash" };
        let label = if sel.len() > 1 {
            format!("Move {} items to {bin}", sel.len())
        } else {
            format!("Move to {bin}")
        };
        if ui
            .button(egui::RichText::new(label).color(ui.visuals().error_fg_color))
            .clicked()
        {
            intents.push(Intent::Trash(sel.clone()));
            ui.close();
        }
        if !local {
            let label = if sel.len() > 1 { format!("Delete {} items now", sel.len()) } else { "Delete now".to_owned() };
            if ui
                .button(egui::RichText::new(label).color(ui.visuals().error_fg_color))
                .on_hover_text("Delete from the device right away, without going through /trash (⌥⌘⌫)")
                .clicked()
            {
                intents.push(Intent::DeleteNow(sel));
                ui.close();
            }
        }
    });
}

/// Draws a row's icon, name and size straight onto the painter. These are
/// deliberately not widgets: a label would sit on top of the row's
/// interaction area and swallow clicks meant for the row.
fn draw_cells(
    ui: &Ui,
    inner: egui::Rect,
    cx: &TableCtx<'_>,
    entry: &fs_model::Entry,
    shown: Option<&str>,
    device: bool,
) {
    let icon = if entry.is_dir {
        "📁"
    } else if !device && cx.preview.is_playing(&entry.path) {
        "🔊"
    } else if entry.is_audio || device {
        "🎵"
    } else {
        "·"
    };
    let gap = ui.spacing().item_spacing.x;

    // Right-aligned size first, so the name knows how much room it has.
    let size_text = if entry.is_dir && device {
        String::new()
    } else if entry.is_dir {
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
    let mono = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Monospace)
        .cloned()
        .unwrap_or_else(|| egui::FontId::monospace(12.0));
    let size_galley =
        ui.painter().layout_no_wrap(size_text, mono, ui.visuals().weak_text_color());
    let size_pos = egui::pos2(
        inner.right() - size_galley.size().x,
        inner.center().y - size_galley.size().y / 2.0,
    );
    let size_left = size_pos.x;
    ui.painter().galley(size_pos, size_galley, egui::Color32::PLACEHOLDER);

    let mut x = paint_text(ui, inner, icon, body_font(ui, 0.0), ui.visuals().text_color());

    // Committed badge: a green check painted right after the icon. Drawn as
    // lines rather than a glyph so it never depends on font coverage.
    if !device && cx.committed.contains(&entry.path) {
        let h = 9.0;
        let left = x + gap;
        let cy = inner.center().y;
        let stroke = Stroke::new(2.0, egui::Color32::from_rgb(90, 210, 110));
        ui.painter().line_segment(
            [egui::pos2(left, cy), egui::pos2(left + h * 0.35, cy + h * 0.4)],
            stroke,
        );
        ui.painter().line_segment(
            [egui::pos2(left + h * 0.35, cy + h * 0.4), egui::pos2(left + h, cy - h * 0.5)],
            stroke,
        );
        x = left + h;
    }

    // Name: white (gray once opened in the editor), one point larger than
    // body text, cut off with "…" if it would run into the size column.
    let mut name_rect = inner;
    name_rect.min.x = x + gap;
    name_rect.max.x = (size_left - gap).max(name_rect.min.x);
    let name_color = if entry.empty {
        // Empty folder: nothing to see inside, so it recedes.
        ui.visuals().weak_text_color()
    } else if !device && cx.opened.contains(&entry.path) {
        egui::Color32::GRAY
    } else {
        egui::Color32::WHITE
    };
    // Cut items fade until they're pasted, like Explorer.
    let name_color = if cx.is_cut(&entry.path, device) { name_color.gamma_multiply(0.4) } else { name_color };
    paint_text(ui, name_rect, shown.unwrap_or(&entry.name), body_font(ui, 1.0), name_color);
}

fn paste_button(ui: &mut Ui, cx: &TableCtx<'_>, target: Option<PathBuf>, intents: &mut Vec<Intent>) {
    let label = match (cx.paste_label(), &target) {
        (Some(l), Some(dir)) => {
            let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            format!("{l} → {name}")
        }
        (Some(l), None) => l,
        (None, _) => "Paste".to_owned(),
    };
    if ui.add_enabled(cx.clipboard.is_some(), egui::Button::new(label)).clicked() {
        intents.push(Intent::Paste(target));
        ui.close();
    }
}

fn new_folder_button(ui: &mut Ui, cx: &TableCtx<'_>, dir: PathBuf, intents: &mut Vec<Intent>) {
    if ui.button("New folder").clicked() {
        intents.push(Intent::NewFolder(dir.clone()));
        ui.close();
    }
    if let Some(name) = &cx.other_folder {
        if ui
            .button(format!("New folder “{name}”"))
            .on_hover_text("Named after the folder open in the other pane")
            .clicked()
        {
            intents.push(Intent::NewFolderNamed { dir, name: name.clone() });
            ui.close();
        }
    }
}

fn reveal_button(ui: &mut Ui, path: PathBuf, intents: &mut Vec<Intent>) {
    let label = if cfg!(target_os = "macos") { "Reveal in Finder" } else { "Show in Explorer" };
    if ui.button(label).clicked() {
        intents.push(Intent::Reveal(path));
        ui.close();
    }
}

/// The body font, `extra` points larger.
fn body_font(ui: &Ui, extra: f32) -> egui::FontId {
    let mut f = egui::TextStyle::Body.resolve(ui.style());
    f.size += extra;
    f
}

/// Paints one line of text left-aligned and vertically centred in `rect`,
/// eliding with "…" if it doesn't fit. Returns the x where the text ends.
fn paint_text(ui: &Ui, rect: egui::Rect, text: &str, font: egui::FontId, color: egui::Color32) -> f32 {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width().max(0.0));
    let galley = ui.painter().layout_job(job);
    let pos = egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0);
    let end = pos.x + galley.size().x;
    ui.painter().with_clip_rect(rect.intersect(ui.clip_rect())).galley(pos, galley, color);
    end
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs_model::Entry;

    fn tab() -> DirTab {
        let mut t = DirTab::new(1, PathBuf::from("/"));
        let g = t.begin_reload();
        let e = |n: &str, d: bool| Entry {
            name: n.into(),
            path: PathBuf::from("/").join(n),
            is_dir: d,
            len: 1234,
            modified: None,
            is_audio: !d,
            empty: false,
        };
        t.accept_listing(g, Ok(vec![e("folder", true), e("kick.wav", false)]));
        t.selected.clear();
        t
    }

    /// One frame with the table in a left side panel, like the app.
    fn frame(ctx: &egui::Context, tab: &mut DirTab, events: Vec<egui::Event>) -> Vec<Intent> {
        let (sizes, preview) = (SizeCache::default(), Preview::default());
        let mut got = Vec::new();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 400.0))),
            events,
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            egui::Panel::left("col").default_size(320.0).show(ui, |ui| {
                got = show(ui, tab, TableCtx { sizes: &sizes, preview: &preview, opened: &Default::default(), committed: &Default::default(), pane_focused: true, clipboard: None, other_folder: None });
            });
            egui::CentralPanel::default().show(ui, |_| {});
        });
        out.textures_delta.clear();
        got
    }

    fn press(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }

    /// Clicking anywhere across a row -- icon, name, gaps, size column, empty
    /// space -- selects it, and double-clicking opens it.
    #[test]
    fn whole_row_is_clickable() {
        // Find the first row's top edge by clicking down through the name.
        let y = (20..200)
            .map(|y| y as f32)
            .find(|&y| {
                let mut t = tab();
                let ctx = egui::Context::default();
                frame(&ctx, &mut t, vec![]);
                let pos = egui::pos2(40.0, y);
                frame(&ctx, &mut t, vec![egui::Event::PointerMoved(pos), press(pos, true)]);
                frame(&ctx, &mut t, vec![press(pos, false)]);
                t.selected.contains(&PathBuf::from("/folder"))
            })
            .expect("row not found")
            + ROW_HEIGHT / 2.0;

        for x in (12..300).step_by(6) {
            let ctx = egui::Context::default();
            let mut t = tab();
            frame(&ctx, &mut t, vec![]);
            let pos = egui::pos2(x as f32, y);
            let mut opened = false;
            frame(&ctx, &mut t, vec![egui::Event::PointerMoved(pos)]);
            for _ in 0..2 {
                for down in [true, false] {
                    let i = frame(&ctx, &mut t, vec![press(pos, down)]);
                    opened |= i.iter().any(|i| matches!(i, Intent::Open(_)));
                }
            }
            assert!(t.selected.contains(&PathBuf::from("/folder")), "click at x={x} missed");
            assert!(opened, "double-click at x={x} didn't open");
        }
    }
}
