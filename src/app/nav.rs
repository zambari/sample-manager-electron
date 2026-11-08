//! Getting around: favourites, drives, and the typed path bar.

use super::*;

impl App {
    pub(super) fn add_favourite(&mut self, dir: PathBuf) {
        if !self.favourites.contains(&dir) {
            self.favourites.push(dir);
        }
    }

    /// Star dropdown: jump the pane's active tab to a bookmarked folder, or
    /// bookmark the one it's showing.
    pub(super) fn favourites_menu(&mut self, ui: &mut egui::Ui, role: Role) {
        let cwd = self.pane(role).active_tab().cwd.clone();
        let is_fav = self.favourites.contains(&cwd);
        let mut go = None;
        let mut remove = None;
        let mut toggle_current = false;

        let star = if is_fav { "★ ⏷" } else { "☆ ⏷" };
        ui.menu_button(star, |ui| {
            ui.set_min_width(240.0);
            if self.favourites.is_empty() {
                ui.label(egui::RichText::new("No favourites yet").weak());
            }
            for (i, fav) in self.favourites.iter().enumerate() {
                let name = fav
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| fav.display().to_string());
                // An unplugged drive's folder stays listed but can't be opened.
                let exists = fav.is_dir();
                ui.horizontal(|ui| {
                    if ui
                        .add(crate::ui::theme::close_button(None, true))
                        .on_hover_text("Remove from favourites")
                        .clicked()
                    {
                        remove = Some(i);
                    }
                    let resp = ui
                        .add_enabled(
                            exists,
                            egui::Button::new(name).selected(*fav == cwd).frame(false),
                        )
                        .on_hover_text(fav.display().to_string())
                        .on_disabled_hover_text(format!("{} (not found)", fav.display()));
                    if resp.clicked() {
                        go = Some(fav.clone());
                        ui.close();
                    }
                });
            }
            ui.separator();
            let label = if is_fav {
                "Remove this folder"
            } else {
                "Add this folder"
            };
            if ui.button(label).clicked() {
                toggle_current = true;
                ui.close();
            }
        })
        .response
        .on_hover_text("Favourite folders");

        if let Some(i) = remove {
            self.favourites.remove(i);
        }
        if toggle_current {
            if is_fav {
                self.favourites.retain(|f| *f != cwd);
            } else {
                self.add_favourite(cwd);
            }
        }
        if let Some(p) = go {
            self.focused = role;
            self.editor_focused = false;
            self.open_in(role, p);
        }
    }

    /// Drive dropdown: jump to the home folder or any mounted disk (drive
    /// letters on Windows, /Volumes on macOS).
    pub(super) fn drives_menu(&mut self, ui: &mut egui::Ui, role: Role) {
        let cwd = self.pane(role).active_tab().cwd.clone();
        let mut go = None;
        ui.menu_button("💾 ⏷", |ui| {
            ui.set_min_width(180.0);
            // Probing drives can be slow (a sleeping network share), so only
            // while the menu is open, and not every frame.
            if self
                .volumes
                .as_ref()
                .is_none_or(|(t, _)| t.elapsed().as_secs_f32() > 2.0)
            {
                self.volumes = Some((std::time::Instant::now(), crate::fs_model::volumes()));
            }
            let home = dirs_home();
            if ui
                .add(
                    egui::Button::new("🏠 Home")
                        .selected(cwd == home)
                        .frame(false),
                )
                .on_hover_text(home.display().to_string())
                .clicked()
            {
                go = Some(home);
                ui.close();
            }
            ui.separator();
            let vols = self
                .volumes
                .as_ref()
                .map(|(_, v)| v.as_slice())
                .unwrap_or_default();
            // The disk holding cwd: the longest volume path it sits under.
            let current = vols
                .iter()
                .filter(|v| cwd.starts_with(&v.path))
                .max_by_key(|v| v.path.as_os_str().len())
                .map(|v| &v.path);
            for v in vols {
                let resp = ui
                    .add(
                        egui::Button::new(v.name.as_str())
                            .selected(current == Some(&v.path))
                            .frame(false),
                    )
                    .on_hover_text(v.path.display().to_string());
                if resp.clicked() {
                    go = Some(v.path.clone());
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text("Drives");
        if let Some(p) = go {
            self.focused = role;
            self.editor_focused = false;
            self.open_in(role, p);
        }
    }

    /// Typed path in the pane header. Enter goes there, Esc or clicking
    /// away cancels.
    pub(super) fn path_editor(&mut self, ui: &mut egui::Ui, role: Role) {
        let Some((_, buf)) = self.path_edit.as_mut() else {
            return;
        };
        let id = egui::Id::new(("path_edit", role.label()));
        let resp = ui.add(
            egui::TextEdit::singleline(buf)
                .id(id)
                .font(egui::TextStyle::Small)
                .desired_width(f32::INFINITY),
        );
        // First frame: take focus with the whole path selected.
        if !resp.has_focus() && !resp.lost_focus() {
            ui.memory_mut(|m| m.request_focus(id));
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), id) {
                let n = buf.chars().count();
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(n),
                    )));
                state.store(ui.ctx(), id);
            }
            return;
        }
        // Consumed, so the app's own Enter (commit) doesn't also fire.
        let enter = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
        if resp.lost_focus() {
            ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        }
        if !resp.lost_focus() {
            return;
        }
        let typed = std::mem::take(buf);
        self.path_edit = None;
        if !enter {
            return;
        }
        let Some(path) = typed_path(&typed) else {
            return;
        };
        let target = if path.is_file() {
            path.parent().map(Path::to_path_buf)
        } else {
            Some(path.clone())
        };
        match target {
            Some(dir) if dir.is_dir() => {
                self.focused = role;
                self.editor_focused = false;
                self.open_in(role, dir);
            }
            _ => self
                .errors
                .push(format!("{}: no such folder", path.display())),
        }
    }
}

/// Turn what was typed into the path bar into a path: trims whitespace and
/// the quotes Windows' "Copy as path" adds, expands a leading `~`, and makes
/// a bare drive (`D:`) mean that drive's root.
fn typed_path(s: &str) -> Option<PathBuf> {
    let s = s.trim().trim_matches('"').trim();
    if s.is_empty() {
        return None;
    }
    if s == "~" {
        return Some(dirs_home());
    }
    if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        return Some(dirs_home().join(rest));
    }
    if cfg!(target_os = "windows") && s.len() == 2 && s.ends_with(':') {
        return Some(PathBuf::from(format!("{s}\\")));
    }
    Some(PathBuf::from(s))
}
