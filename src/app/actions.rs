//! Keyboard handling, clipboard and file-browser actions (open, rename, new folder...).

use super::*;

impl App {
    // ---- keyboard ---------------------------------------------------------

    pub(super) fn handle_keys(&mut self, ui: &egui::Ui) {
        // An open rename editor or modal owns the keyboard.
        let tab = self.pane(self.focused).active_tab();
        let renaming = tab.renaming.is_some() || tab.multi_rename.is_some();
        if renaming || self.leave_pending.is_some() || self.pending.is_some() || self.device_load_pending.is_some() || self.delete_pending.is_some() || self.dupes_pending.is_some() || self.kit_naming.is_some() {
            return;
        }
        if ui.ctx().text_edit_focused() {
            return; // a text field (filter box, typed value) has the keyboard
        }

        // ⌘X / ⌘C / ⌘V arrive as clipboard events, not key presses.
        let clip: Vec<egui::Event> = ui.input(|i| {
            i.events
                .iter()
                .filter(|e| matches!(e, egui::Event::Cut | egui::Event::Copy | egui::Event::Paste(_)))
                .cloned()
                .collect()
        });

        if self.editor_focused {
            let actions = ui.input(keymap::editor_actions);
            let wave = self.editor.wave_rect;
            for a in actions {
                self.perform_editor(a, wave);
            }
            for e in clip {
                self.editor_clipboard(&e);
            }
            return;
        }

        // Space always drives the editor's playhead, whichever pane has focus,
        // so a playing loop can be stopped without clicking into the editor.
        let (space, shift_space) = ui.input(|i| {
            let m = i.modifiers;
            let hit = i.key_pressed(egui::Key::Space);
            (hit && !m.any(), hit && m.shift_only())
        });
        if space || shift_space {
            if self.engine.is_playing() {
                self.engine.stop();
            } else {
                let a = if shift_space { EdAction::TogglePlayAlt } else { EdAction::TogglePlay };
                self.perform_editor(a, self.editor.wave_rect);
            }
        }

        // K / B work from the file panes too, so you can arrow through
        // samples and pick slices without clicking into the editor.
        let (k, b) = ui.input(|i| {
            let none = !i.modifiers.any();
            (none && i.key_pressed(egui::Key::K), none && i.key_pressed(egui::Key::B))
        });
        if k {
            self.add_slice();
        }
        if b {
            self.show_kit(!self.showing_kit);
        }

        let actions = ui.input(|i| self.keymap.actions(i));
        for action in actions {
            self.perform(action);
        }

        let role = self.focused;
        for e in clip {
            match e {
                egui::Event::Cut => self.set_clipboard(Kind::Move, role, self.pane(role).active_tab().effective_selection()),
                egui::Event::Copy => self.set_clipboard(Kind::Copy, role, self.pane(role).active_tab().effective_selection()),
                egui::Event::Paste(_) => self.paste(role, None),
                _ => {}
            }
        }
    }

    // ---- clipboard --------------------------------------------------------

    /// Cut / copy / paste of audio inside the editor.
    pub(super) fn editor_clipboard(&mut self, e: &egui::Event) {
        let ed = &mut self.editor;
        let secs = |f: usize, rate: u32| f as f64 / rate.max(1) as f64;
        match e {
            egui::Event::Cut | egui::Event::Copy => {
                let cut = matches!(e, egui::Event::Cut);
                let ok = if cut { ed.cut() } else { ed.copy() };
                if let (true, Some(c)) = (ok, &ed.clip) {
                    let verb = if cut { "Cut" } else { "Copied" };
                    self.status = format!("{verb} {:.3}s of audio — ⌘V pastes it here or into another file", secs(c.frames(), c.rate));
                } else if cut {
                    self.status = "Can't cut everything — select part of the file first".to_owned();
                }
            }
            egui::Event::Paste(_) => {
                if let Some(c) = &ed.clip {
                    let len = secs(c.frames(), c.rate);
                    if ed.paste() {
                        self.status = format!("Pasted {len:.3}s");
                    }
                } else {
                    self.status = "Nothing to paste — copy some audio in the editor first".to_owned();
                }
            }
            _ => {}
        }
    }

    pub(super) fn set_clipboard(&mut self, kind: Kind, role: Role, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let device = self.pane(role).active_tab().device;
        let n = paths.len();
        let verb = if kind == Kind::Move { "Cut" } else { "Copied" };
        self.status = format!("{verb} {n} item(s) — paste into a folder");
        self.clipboard = Some(Clipboard { kind, device, paths });
    }

    /// Paste into `target`, or the pane's current folder when `None`.
    pub(super) fn paste(&mut self, role: Role, target: Option<PathBuf>) {
        let Some(clip) = self.clipboard.clone() else { return };
        let tab = self.pane(role).active_tab();
        let to_dev = tab.device;
        let dest = target.unwrap_or_else(|| tab.cwd.clone());
        // A cut is used up by its paste, like Finder; a copy can be pasted again.
        if clip.kind == Kind::Move {
            self.clipboard = None;
        }
        self.transfer(clip.kind, clip.device, clip.paths, to_dev, dest);
    }

    pub(super) fn perform(&mut self, action: Action) {
        let role = self.focused;
        match action {
            Action::Commit => self.commit(Kind::Copy),
            Action::Rename => {
                let tab = self.pane_mut(role).active_tab_mut();
                if tab.selected.len() > 1 {
                    self.status = "Rename works on one item -- use the Multi-Rename Tool (⇧F2) for several".to_owned();
                } else if let Some(e) = tab.cursor_entry() {
                    tab.renaming = Some((e.path.clone(), e.name.clone()));
                }
            }
            Action::MultiRename => self.pane_mut(role).active_tab_mut().begin_multi_rename(),
            Action::OpenDir => {
                if let Some(e) = self.pane(role).active_tab().cursor_entry() {
                    if e.is_dir {
                        let p = e.path.clone();
                        self.open_in(role, p);
                    }
                }
            }
            Action::GoUp => self.go_up(role),
            Action::Expand => {
                let tab = self.pane(role).active_tab();
                let Some(e) = tab.cursor_entry() else { return };
                if !e.is_dir {
                    return;
                }
                let p = e.path.clone();
                if !tab.tree {
                    self.open_in(role, p);
                } else if tab.is_expanded(&p) {
                    // Already open: step onto its first child, if it has one.
                    let tab = self.pane_mut(role).active_tab_mut();
                    let c = tab.cursor.unwrap_or(0);
                    if tab.depths.get(c + 1) > tab.depths.get(c) {
                        tab.set_cursor(c + 1);
                    }
                } else {
                    self.expand(role, p);
                }
            }
            Action::Collapse => {
                let tab = self.pane_mut(role).active_tab_mut();
                if !tab.tree {
                    return self.go_up(role);
                }
                let Some(c) = tab.cursor else { return self.go_up(role) };
                let path = tab.entries[c].path.clone();
                if tab.is_expanded(&path) {
                    tab.collapse(&path);
                } else if let Some(pr) = tab.parent_row(c) {
                    tab.set_cursor(pr);
                } else {
                    self.go_up(role);
                }
            }
            Action::ComputeSize => {
                if !self.pane(role).active_tab().device {
                    let sel = self.pane(role).active_tab().effective_selection();
                    self.request_sizes(sel);
                }
            }
            Action::Trash => {
                let tab = self.pane(role).active_tab();
                let sel = tab.effective_selection();
                let on_dev = tab.device;
                self.trash_now(on_dev, sel);
            }
            Action::DeleteNow => {
                let tab = self.pane(role).active_tab();
                let sel = tab.effective_selection();
                if !tab.device {
                    self.status = "Delete now is only for the device; files on the computer go to the Trash".to_owned();
                } else if !sel.is_empty() {
                    self.delete_pending = Some(sel);
                }
            }
            Action::SelectAll => self.pane_mut(role).active_tab_mut().select_all(),
            Action::Undo => self.undo_file(),
            Action::ToggleSelect => {
                let tab = self.pane_mut(role).active_tab_mut();
                if let Some(c) = tab.cursor {
                    tab.toggle_at(c);
                }
            }
            Action::Refresh => {
                let i = self.pane(role).active;
                self.reload(role, i);
            }
            Action::CursorUp => self.pane_mut(role).active_tab_mut().move_cursor(-1, false),
            Action::CursorDown => self.pane_mut(role).active_tab_mut().move_cursor(1, false),
            Action::ExtendUp => self.pane_mut(role).active_tab_mut().move_cursor(-1, true),
            Action::ExtendDown => self.pane_mut(role).active_tab_mut().move_cursor(1, true),
            Action::NewTab => {
                let tab = self.pane(role).active_tab();
                let (cwd, device) = (tab.cwd.clone(), tab.device);
                let id = self.new_tab_id();
                let tab = if device { DirTab::new_device(id, cwd) } else { DirTab::new(id, cwd) };
                self.pane_mut(role).add_tab(tab);
                let i = self.pane(role).active;
                self.reload(role, i);
            }
            Action::CloseTab => {
                let i = self.pane(role).active;
                self.pane_mut(role).close_tab(i);
            }
            Action::NextTab => self.pane_mut(role).cycle(1),
            Action::PrevTab => self.pane_mut(role).cycle(-1),
            // Tab walks the columns left to right: Source → Editor → Destination.
            Action::SwitchPane => match self.focused {
                Role::Source => self.editor_focused = true,
                Role::Dest => self.focused = Role::Source,
            },
            Action::Cancel => {
                self.engine.stop();
                if let Some(a) = self.shown_job() {
                    self.jobs.cancel(a.id);
                }
                self.pane_mut(role).active_tab_mut().renaming = None;
            }
        }
    }

    pub(super) fn go_up(&mut self, role: Role) {
        if let Some(p) = self.pane(role).active_tab().parent() {
            self.open_in(role, p);
        }
    }

    pub(super) fn open_in(&mut self, role: Role, path: PathBuf) {
        self.pane_mut(role).active_tab_mut().navigate_to(path);
        let i = self.pane(role).active;
        self.reload(role, i);
    }

    /// Make "New Folder" (or "New Folder 2", ...) in `dir` and open the
    /// inline rename on it. With `base`, propose "<base> 2", "<base> 3", ...
    /// instead. With `named`, make exactly that folder and just select it.
    pub(super) fn new_folder(&mut self, role: Role, dir: PathBuf, named: Option<String>, base: Option<String>) {
        let device = self.pane(role).active_tab().device;
        if let Some(name) = named {
            let path = dir.join(&name);
            if device {
                let exists = [&self.source, &self.dest]
                    .into_iter()
                    .flat_map(|p| p.tabs.iter())
                    .filter(|t| t.device)
                    .find_map(|t| t.listing_of(&dir))
                    .is_some_and(|l| l.iter().any(|e| e.name == name));
                if exists {
                    self.errors.push(format!("{} already exists", path.display()));
                    return;
                }
                self.run_device(device::Task::Mkdir { path: path.clone() }, "Creating folder", 1, 0);
            } else {
                if let Err(e) = std::fs::create_dir(&path) {
                    self.errors.push(format!("{}: {e}", path.display()));
                    return;
                }
                self.push_undo(undo::FileUndo::NewFolder(path.clone()));
                self.reload_showing(&dir, false);
            }
            self.focused = role;
            self.editor_focused = false;
            let tab = self.pane_mut(role).active_tab_mut();
            tab.selected.clear();
            tab.selected.insert(path);
            tab.cursor = None;
            return;
        }
        // With a base (the right-clicked folder, which already exists here),
        // start numbering at 2; otherwise try plain "New Folder" first.
        let (base, first) = match base {
            Some(b) => (b, None),
            None => ("New Folder".to_owned(), Some("New Folder".to_owned())),
        };
        let taken: std::collections::HashSet<String> = if device {
            [&self.source, &self.dest]
                .into_iter()
                .flat_map(|p| p.tabs.iter())
                .filter(|t| t.device)
                .find_map(|t| t.listing_of(&dir))
                .map(|l| l.iter().map(|e| e.name.clone()).collect())
                .unwrap_or_default()
        } else {
            Default::default()
        };
        let free = |n: &String| if device { !taken.contains(n) } else { !dir.join(n).exists() };
        let name = first
            .into_iter()
            .chain((2..10_000).map(|n| format!("{base} {n}")))
            .find(|n| free(n))
            .unwrap_or_else(|| format!("{base} new"));
        let path = dir.join(&name);
        if device {
            self.run_device(device::Task::Mkdir { path: path.clone() }, "Creating folder", 1, 0);
        } else {
            if let Err(e) = std::fs::create_dir(&path) {
                self.errors.push(format!("{}: {e}", path.display()));
                return;
            }
            self.push_undo(undo::FileUndo::NewFolder(path.clone()));
            self.reload_showing(&dir, false);
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.focused = role;
        self.editor_focused = false;
        let tab = self.pane_mut(role).active_tab_mut();
        // The row appears once the listing comes back; the cursor lands on it then.
        tab.selected.clear();
        tab.selected.insert(path.clone());
        tab.cursor = None;
        tab.renaming = Some((path, name));
    }

    /// Make "<parent name> Subfolder" in `dir`, move `paths` into it, and
    /// open the inline rename on the new folder. Local only; the moves stay
    /// inside one folder, so they're plain renames.
    pub(super) fn new_folder_with(&mut self, role: Role, dir: PathBuf, paths: Vec<PathBuf>) {
        if paths.is_empty() || self.pane(role).active_tab().device {
            return;
        }
        let parent_name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "New".to_owned());
        let folder = fs_model::unique_sibling(&dir.join(format!("{parent_name} Subfolder")));
        if let Err(e) = std::fs::create_dir(&folder) {
            self.errors.push(format!("{}: {e}", folder.display()));
            return;
        }
        let mut pairs = Vec::new();
        for src in paths {
            let Some(name) = src.file_name() else { continue };
            let dst = folder.join(name);
            match std::fs::rename(&src, &dst) {
                Ok(()) => {
                    self.follow_rename(&src, &dst);
                    if let Some(p) = src.parent() {
                        self.sizes.invalidate_subtree(p);
                    }
                    pairs.push((src, dst));
                }
                Err(e) => self.errors.push(format!("{}: {e}", src.display())),
            }
        }
        if pairs.is_empty() {
            let _ = std::fs::remove_dir(&folder);
            return;
        }
        let dirs: std::collections::BTreeSet<PathBuf> =
            pairs.iter().filter_map(|(s, _)| s.parent().map(Path::to_path_buf)).chain([dir.clone()]).collect();
        self.push_undo(undo::FileUndo::Grouped { folder: folder.clone(), pairs });
        for d in dirs {
            self.reload_showing(&d, false);
        }
        let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.focused = role;
        self.editor_focused = false;
        let tab = self.pane_mut(role).active_tab_mut();
        tab.selected.clear();
        tab.selected.insert(folder.clone());
        tab.cursor = None;
        tab.renaming = Some((folder, name));
    }

    /// Run the renames the multi-rename panel settled on, then close it.
    pub(super) fn apply_multi_rename(&mut self, role: Role, rows: &[crate::rename::Row]) {
        let tab = self.pane_mut(role).active_tab_mut();
        let device = tab.device;
        tab.multi_rename = None;
        let (steps, finals) = crate::rename::plan(rows);
        if finals.is_empty() {
            return;
        }
        // Keep the renamed items selected under their new names.
        tab.selected = finals.iter().map(|(_, to)| to.clone()).collect();

        if device {
            // Device plans never chain (the preview refuses that), so each
            // step is a final rename. The jobs run one after another.
            let n = steps.len();
            for (from, to) in steps {
                self.run_device(device::Task::Rename { from, to }, "Renaming", 1, 0);
            }
            self.status = format!("Renaming {n} item(s) on the device…");
            return;
        }

        let mut parents: Vec<PathBuf> = Vec::new();
        for (from, to) in &steps {
            // The preview checked against names seen when it opened; check
            // again, since something may have appeared since.
            let case_only = from.to_string_lossy().to_lowercase() == to.to_string_lossy().to_lowercase();
            if !case_only && to.exists() {
                self.errors.push(format!("{} already exists, left {} as it was", to.display(), from.display()));
                continue;
            }
            match std::fs::rename(from, to) {
                Ok(()) => {}
                Err(e) => self.errors.push(format!("rename {} failed: {e}", from.display())),
            }
            if let Some(p) = to.parent() {
                if !parents.iter().any(|x| x == p) {
                    parents.push(p.to_path_buf());
                }
            }
        }
        // Paths the app remembers follow the files.
        for (from, to) in &finals {
            if self.opened.remove(from) {
                self.opened.insert(to.clone());
                self.opened_dirty = true;
            }
            if self.committed.remove(from) {
                self.committed.insert(to.clone());
                self.committed_dirty = true;
            }
        }
        let done: Vec<(PathBuf, PathBuf)> = finals.iter().filter(|(_, to)| to.exists()).cloned().collect();
        let renamed = done.len();
        if !done.is_empty() {
            self.push_undo(undo::FileUndo::Moved { pairs: done, what: "rename", replaced: false });
        }
        self.status = format!("Renamed {renamed} of {} item(s)", finals.len());
        for p in parents {
            self.sizes.invalidate_subtree(&p);
            self.reload_showing(&p, false);
        }
    }

    pub(super) fn do_rename(&mut self, from: &Path, to: &str, device: bool) {
        let to = to.trim();
        if to.is_empty() || to.contains(std::path::MAIN_SEPARATOR) || to.contains('/') {
            self.errors.push(format!("invalid name: {to:?}"));
            return;
        }
        let Some(parent) = from.parent() else { return };
        let target = parent.join(to);
        if target == from {
            return;
        }
        if device {
            let task = device::Task::Rename { from: from.to_path_buf(), to: target };
            self.run_device(task, "Renaming", 1, 0);
            return;
        }
        if target.exists() {
            // Rename is not a transfer, so there's no plan to resolve -- just
            // refuse rather than silently clobbering a neighbour.
            self.errors
                .push(format!("{} already exists", target.display()));
            return;
        }
        match std::fs::rename(from, &target) {
            Ok(()) => {
                self.push_undo(undo::FileUndo::Moved { pairs: vec![(from.to_path_buf(), target)], what: "rename", replaced: false });
                self.sizes.invalidate_subtree(parent);
                self.reload_showing(parent, false);
            }
            Err(e) => self.errors.push(format!("rename failed: {e}")),
        }
    }
}
