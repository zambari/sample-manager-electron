//! File-row context commands and clipboard/new-folder/reveal menu buttons.

use super::*;

/// Commands available for the clicked row and its effective selection.
pub(super) fn row_menu(
    ui: &mut Ui,
    tab: &DirTab,
    cx: &TableCtx<'_>,
    entry: &fs_model::Entry,
    intents: &mut Vec<Intent>,
) {
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
    if local && entry.is_dir {
        let dirs: Vec<PathBuf> = tab.effective_selection().into_iter().filter(|p| p.is_dir()).collect();
        ui.menu_button("Make mono copy", |ui| {
            use crate::audio::stats::Fold;
            let opts: [(&str, &str, &[Fold]); 4] = [
                ("Left + Right", "Each stereo file becomes two: \"name L\" and \"name R\"", &[Fold::Left, Fold::Right]),
                ("Left only", "Keep the left channel of stereo files", &[Fold::Left]),
                ("Right only", "Keep the right channel of stereo files", &[Fold::Right]),
                ("Mix", "Average both channels of stereo files", &[Fold::Mix]),
            ];
            for (label, hint, folds) in opts {
                if ui.button(label).on_hover_text(hint).clicked() {
                    intents.push(Intent::MonoCopy { dirs: dirs.clone(), folds: folds.to_vec() });
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text("Copy the folder's samples to \"<name> Mono\": mono files as they are, stereo ones folded down");
    }
    if local && entry.is_dir && ui.button("Add to favourites").clicked() {
        intents.push(Intent::AddFavourite(entry.path.clone()));
        ui.close();
    }
    if single && entry.is_dir && ui.button("Open").clicked() {
        intents.push(Intent::Open(entry.path.clone()));
        ui.close();
    }
    if single
        && local
        && !entry.is_dir
        && crate::fs_model::is_audio_path(&entry.path)
        && ui.button("Load as SliceKit").on_hover_text(
            "Open as a slice kit — K then adds selections to it.\n\
             Slices come back from the file's cue points, or, without any, from an [8]-style\n\
             name prefix (8 equal slices)",
        ).clicked()
    {
        intents.push(Intent::UseAsKit(entry.path.clone()));
        ui.close();
    }
    let linked = single && !local && !entry.is_dir && cx.links.known(entry.hash, entry.len as u32).is_some();
    let load_label = if linked { "Load to editor (local copy)" } else { "Load to editor…" };
    if single && !local && !entry.is_dir && ui.button(load_label).clicked() {
        intents.push(Intent::LoadFromDevice(entry.path.clone()));
        ui.close();
    }
    if linked {
        let reveal = if cfg!(target_os = "macos") { "Reveal local copy in Finder" } else { "Show local copy in Explorer" };
        if ui.button(reveal).clicked() {
            intents.push(Intent::RevealLocal(entry.path.clone()));
            ui.close();
        }
        if ui.button("Show local copy in other pane").clicked() {
            intents.push(Intent::ShowLocal(entry.path.clone()));
            ui.close();
        }
    }
    if single && local && !entry.is_dir {
        if let Some(l) = cx.links.for_local(&entry.path) {
            let tip = format!(
                "{} {} (where it was then; it may have moved)",
                if l.uploaded { "Uploaded as" } else { "Fetched from" },
                l.device
            );
            if ui.button("Show on device").on_hover_text(tip).clicked() {
                intents.push(Intent::ShowOnDevice(entry.path.clone()));
                ui.close();
            }
        }
    }
    // Next to the row, i.e. in the folder the row sits in.
    if let Some(parent) = entry.path.parent() {
        let base = entry.is_dir.then(|| entry.name.clone());
        let sel = if local { tab.effective_selection() } else { Vec::new() };
        if sel.len() == 1 {
            new_folder_button(ui, cx, parent.to_path_buf(), base, Vec::new(), intents);
            if ui.button("New folder with this item").on_hover_text("Move it into a new subfolder and rename it").clicked() {
                intents.push(Intent::NewFolderWith { dir: parent.to_path_buf(), paths: sel, name: None });
                ui.close();
            }
        } else {
            new_folder_button(ui, cx, parent.to_path_buf(), base, sel, intents);
        }
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
}

pub(super) fn paste_button(
    ui: &mut Ui,
    cx: &TableCtx<'_>,
    target: Option<PathBuf>,
    intents: &mut Vec<Intent>,
) {
    let label = match (cx.paste_label(), &target) {
        (Some(l), Some(dir)) => {
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            format!("{l} → {name}")
        }
        (Some(l), None) => l,
        (None, _) => "Paste".to_owned(),
    };
    if ui
        .add_enabled(cx.paste_label().is_some(), egui::Button::new(label))
        .clicked()
    {
        intents.push(Intent::Paste(target));
        ui.close();
    }
}

/// "New folder" entries. With several items in `with` (a local selection),
/// they make the folder and move those items into it.
pub(super) fn new_folder_button(
    ui: &mut Ui,
    cx: &TableCtx<'_>,
    dir: PathBuf,
    base: Option<String>,
    with: Vec<PathBuf>,
    intents: &mut Vec<Intent>,
) {
    let n = with.len();
    let multi = n > 1;
    let label = if multi { format!("New folder with {n} items") } else { "New folder".to_owned() };
    let mut b = ui.button(label);
    if multi {
        b = b.on_hover_text("Move the selection into a new subfolder and rename it");
    }
    if b.clicked() {
        intents.push(if multi {
            Intent::NewFolderWith { dir: dir.clone(), paths: with.clone(), name: None }
        } else {
            Intent::NewFolder { dir: dir.clone(), base }
        });
        ui.close();
    }
    if let Some(name) = &cx.other_folder {
        let label = if multi { format!("New folder “{name}” with {n} items") } else { format!("New folder “{name}”") };
        let tip = if multi {
            "Named after the folder open in the other pane; the selection moves into it"
        } else {
            "Named after the folder open in the other pane"
        };
        if ui.button(label).on_hover_text(tip).clicked() {
            intents.push(if multi {
                Intent::NewFolderWith { dir, paths: with, name: Some(name.clone()) }
            } else {
                Intent::NewFolderNamed { dir, name: name.clone() }
            });
            ui.close();
        }
    }
}

pub(super) fn reveal_button(ui: &mut Ui, path: PathBuf, intents: &mut Vec<Intent>) {
    let label = if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else {
        "Show in Explorer"
    };
    if ui.button(label).clicked() {
        intents.push(Intent::Reveal(path));
        ui.close();
    }
}
