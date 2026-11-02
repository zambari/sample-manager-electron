//! Keyboard handling, clipboard and file-browser actions (open, rename, new folder...).

use super::*;

impl App {
    // ---- keyboard ---------------------------------------------------------

    pub(super) fn handle_keys(&mut self, ui: &egui::Ui) {
        // An open rename editor or modal owns the keyboard.
        let tab = self.pane(self.focused).active_tab();
        let renaming = tab.renaming.is_some() || tab.multi_rename.is_some();
        if renaming || self.leave_pending.is_some() || self.pending.is_some() || self.device_load_pending.is_some() || self.trash_pending.is_some() || self.delete_pending.is_some() || self.dupes_pending.is_some() {
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
            Action::Preview => {
                let tab = self.pane(role).active_tab();
                if let Some(e) = tab.cursor_entry().filter(|_| !tab.device) {
                    if e.is_dir {
                        // Space on a folder has no sample to audition; treat it
                        // as "tell me how big this is" instead of doing nothing.
                        let p = e.path.clone();
                        self.request_sizes(vec![p]);
                    } else if e.is_audio {
                        let p = e.path.clone();
                        self.preview_toggle(&p);
                    }
                }
            }
            Action::Rename => {
                let tab = self.pane_mut(role).active_tab_mut();
                if tab.selected.len() > 1 {
                    tab.begin_multi_rename();
                } else if let Some(e) = tab.cursor_entry() {
                    tab.renaming = Some((e.path.clone(), e.name.clone()));
                }
            }
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
                if !sel.is_empty() {
                    self.trash_pending = Some((tab.device, sel));
                }
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
    /// inline rename on it. With `named`, make exactly that folder and just
    /// select it.
    pub(super) fn new_folder(&mut self, role: Role, dir: PathBuf, named: Option<String>) {
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
        const BASE: &str = "New Folder";
        let path = if device {
            let taken: std::collections::HashSet<String> = [&self.source, &self.dest]
                .into_iter()
                .flat_map(|p| p.tabs.iter())
                .filter(|t| t.device)
                .find_map(|t| t.listing_of(&dir))
                .map(|l| l.iter().map(|e| e.name.clone()).collect())
                .unwrap_or_default();
            let name = std::iter::once(BASE.to_owned())
                .chain((2..10_000).map(|n| format!("{BASE} {n}")))
                .find(|n| !taken.contains(n))
                .unwrap_or_else(|| BASE.to_owned());
            let path = dir.join(name);
            self.run_device(device::Task::Mkdir { path: path.clone() }, "Creating folder", 1, 0);
            path
        } else {
            let path = fs_model::unique_sibling(&dir.join(BASE));
            if let Err(e) = std::fs::create_dir(&path) {
                self.errors.push(format!("{}: {e}", path.display()));
                return;
            }
            self.reload_showing(&dir, false);
            path
        };
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
        let renamed = finals.iter().filter(|(_, to)| to.exists()).count();
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
                self.sizes.invalidate_subtree(parent);
                self.reload_showing(parent, false);
            }
            Err(e) => self.errors.push(format!("rename failed: {e}")),
        }
    }
}
