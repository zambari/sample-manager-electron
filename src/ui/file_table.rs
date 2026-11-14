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
use crate::ui::drag_drop::ResponseDnd;
use crate::ui::theme::{oled_font, OLED, OLED_BG, OLED_DIM, OLED_HOVER};
use egui::{Align, Key, Sense, Stroke, StrokeKind, Ui};
use std::path::PathBuf;

mod cells;
mod effects;
mod menu;
mod rows;

use cells::{body_font, draw_cells, draw_device_cells, paint_text, OLED_TEXT, OLED_UNFOCUSED_SEL};
use effects::paint_save_mark;
use menu::{new_folder_button, paste_button, reveal_button, row_menu};
use rows::rows;
pub use effects::{paint_ping, ping_scroll};

pub const ROW_HEIGHT: f32 = 22.0;

/// What the table is dragging. Must be `Any + Send + Sync` for egui's dnd.
#[derive(Clone, Debug)]
pub struct DragPayload {
    pub paths: Vec<PathBuf>,
    /// The paths are on the device.
    pub device: bool,
    /// Dragged in from Finder / Explorer.
    pub external: bool,
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
    CommitRename {
        from: PathBuf,
        to: String,
    },
    CancelRename,
    /// Something was dropped onto this tab's list.
    Dropped(DragPayload),
    /// Something was dropped onto a folder row.
    DroppedOn {
        dir: PathBuf,
        payload: DragPayload,
    },
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
    /// Make a new folder in `dir` and start renaming it. With `base`, the
    /// proposed name starts with that (the right-clicked folder's name).
    NewFolder {
        dir: PathBuf,
        base: Option<String>,
    },
    /// Make a folder with this exact name in this directory.
    NewFolderNamed {
        dir: PathBuf,
        name: String,
    },
    /// Make a new folder in `dir`, move these into it, and start renaming it.
    /// With `name`, make exactly that folder and just select it.
    NewFolderWith {
        dir: PathBuf,
        paths: Vec<PathBuf>,
        name: Option<String>,
    },
    /// Open this file as a slice kit and make it the one K adds to.
    UseAsKit(PathBuf),
    /// Fetch this device file and open it in the editor (after a warning).
    LoadFromDevice(PathBuf),
    /// Device file: reveal its local copy in Finder / Explorer.
    RevealLocal(PathBuf),
    /// Device file: show its local copy in the other pane.
    ShowLocal(PathBuf),
    /// Local file: show the device file it was uploaded as (or fetched from).
    ShowOnDevice(PathBuf),
    /// Make a "<name> Mono" copy of each of these folders, folding stereo
    /// files down each of these ways.
    MonoCopy {
        dirs: Vec<PathBuf>,
        folds: Vec<crate::audio::stats::Fold>,
    },
}

pub struct TableCtx<'a> {
    pub sizes: &'a SizeCache,
    pub preview: &'a Preview,
    /// Files already loaded into the editor; drawn gray.
    pub opened: &'a std::collections::HashSet<PathBuf>,
    /// Files already committed to a destination; drawn with a check badge.
    pub committed: &'a std::collections::HashSet<PathBuf>,
    /// Device files with a known local copy; drawn with a dot badge.
    pub links: &'a crate::links::Links,
    /// Local files edited since their device copy was made, for samples in
    /// the loaded project: drawn amber.
    pub changed: &'a std::collections::HashSet<PathBuf>,
    /// Files being written by the editor (`None`), or written at this
    /// instant: drawn with a pulse, then a fading flash.
    pub saving: &'a std::collections::HashMap<PathBuf, Option<std::time::Instant>>,
    pub pane_focused: bool,
    /// The clipboard, for dimming cut items and enabling Paste.
    pub clipboard: Option<&'a crate::ops::Clipboard>,
    /// Pool slots cut or copied on the device's Samples page (and no file
    /// clipboard): Paste downloads them here. 0 on +Drive tabs.
    pub slot_clip: usize,
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
        if self.clipboard.is_none() && self.slot_clip > 0 {
            return Some(format!("Download {} pool sample(s) here", self.slot_clip));
        }
        let c = self.clipboard?;
        let n = c.paths.len();
        let what = if n == 1 {
            "1 item".to_owned()
        } else {
            format!("{n} items")
        };
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

/// How long a ping (a row pointed out from the device view) lasts.
pub const PING_SECS: f32 = 1.6;

/// How long the "saved" flash takes to fade out.
pub const SAVE_FLASH_SECS: f32 = 1.5;

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
            hash: 0,
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
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 400.0),
            )),
            events,
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| {
            egui::Panel::left("col").default_size(320.0).show(ui, |ui| {
                got = show(
                    ui,
                    tab,
                    TableCtx {
                        sizes: &sizes,
                        preview: &preview,
                        opened: &Default::default(),
                        committed: &Default::default(),
                        links: &Default::default(),
                        changed: &Default::default(),
                        saving: &Default::default(),
                        pane_focused: true,
                        clipboard: None,
                        slot_clip: 0,
                        other_folder: None,
                    },
                );
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
                frame(
                    &ctx,
                    &mut t,
                    vec![egui::Event::PointerMoved(pos), press(pos, true)],
                );
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
            assert!(
                t.selected.contains(&PathBuf::from("/folder")),
                "click at x={x} missed"
            );
            assert!(opened, "double-click at x={x} didn't open");
        }
    }
}
