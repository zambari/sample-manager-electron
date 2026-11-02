//! Drawing the main window pieces: toolbar, status bar, panes, tab strips, favourites.

use super::*;

impl App {
    pub(super) fn device_picker(&mut self, ui: &mut egui::Ui) {
        let mut choice: Option<Option<String>> = None;
        let name = if self.engine.device_name.is_empty() { "no output".to_owned() } else { self.engine.device_name.clone() };
        if ui.small_button("⟳").on_hover_text("Rescan audio devices").clicked() {
            self.devices = Engine::devices();
        }
        egui::ComboBox::from_id_salt("output_device")
            .selected_text(format!("🔈 {name}"))
            .width(220.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(self.output_device.is_none(), "System default").clicked() {
                    choice = Some(None);
                }
                for d in &self.devices {
                    let on = self.output_device.as_deref() == Some(d.id.as_str());
                    if ui.selectable_label(on, &d.name).on_hover_text(&d.id).clicked() {
                        choice = Some(Some(d.id.clone()));
                    }
                }
            });
        if self.engine.device_rate > 0 {
            ui.label(
                egui::RichText::new(format!("{:.1} kHz · {} ch", self.engine.device_rate as f64 / 1000.0, self.engine.device_channels))
                    .weak()
                    .small(),
            );
        }
        if let Some(dev) = choice {
            self.engine.switch(dev.as_deref());
            self.output_device = dev;
            self.last_stream_error = None;
            if let Some(e) = &self.engine.open_error {
                self.errors.push(format!("audio output: {e}"));
            }
        }
    }

    pub(super) fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("SAMPLE MANAGER").size(13.0).strong().color(theme::ACCENT_BRIGHT));
            ui.separator();

            if ui.checkbox(&mut self.show_hidden, "Hidden").changed() {
                self.reload_all();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // Right to left: the first widget lands at the far right.
                let commit_key = self.keymap.describe(Action::Commit);
                let commit_btn = egui::Button::new(
                    egui::RichText::new(format!("Commit to destination  {commit_key}")).color(theme::ON_ACCENT),
                )
                .fill(theme::ACCENT);
                if ui
                    .add(commit_btn)
                    .on_hover_text(
                        "Copy the source selection into the active destination tab.\n\
                         A single file open in the editor is written with its trim, gain and mono settings.",
                    )
                    .clicked()
                {
                    self.commit(Kind::Copy);
                }
                if ui.button("Move").clicked() {
                    self.commit(Kind::Move);
                }
                ui.separator();
                ui.label(
                    egui::RichText::new(format!("keys: {}", self.keymap.name))
                        .weak()
                        .small(),
                );
                ui.separator();
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    self.device_picker(ui);
                });
            });
        });
    }

    /// The job the status bar shows: the oldest one that is running.
    pub(super) fn shown_job(&self) -> Option<&ActiveJob> {
        self.active_jobs.iter().find(|a| a.started).or(self.active_jobs.first())
    }

    pub(super) fn status_bar(&mut self, ui: &mut egui::Ui) {
        // Anything shown as an error also goes to the log, once.
        for e in self.errors.iter().skip(self.errors_logged) {
            crate::log::line(format!("error: {e}"));
        }
        self.errors_logged = self.errors.len();

        let waiting = self.active_jobs.len().saturating_sub(1);
        if let Some(a) = self.shown_job() {
            // The whole queue as one run: finished jobs plus every live one.
            // A waiting device job counts its top-level items until it starts
            // and counts the files inside, so the total can grow.
            let b = &self.batch;
            let done_items = b.done_items + self.active_jobs.iter().map(|j| j.done_items).sum::<usize>();
            let total_items = b.total_items + self.active_jobs.iter().map(|j| j.total_items).sum::<usize>();
            let done_bytes = b.done_bytes + self.active_jobs.iter().map(|j| j.done_bytes).sum::<u64>();
            let total_bytes = b.total_bytes + self.active_jobs.iter().map(|j| j.total_bytes).sum::<u64>();
            let bytes_known = !b.bytes_unknown && self.active_jobs.iter().all(|j| j.total_bytes > 0);
            let frac = if bytes_known && total_bytes > 0 {
                done_bytes as f32 / total_bytes as f32
            } else if total_items > 0 {
                done_items as f32 / total_items as f32
            } else {
                0.0
            };
            let job_id = a.id;
            let file = a.file;
            // Once a device job has shown a per-file bar, keep it for the
            // rest of the queue so the main bar doesn't resize between files.
            let file_bar = self.active_jobs.iter().any(|j| j.file_bar) || self.batch.file_bar;
            // Device folders have no known size up front: show bytes so far.
            let bytes = if bytes_known {
                format!("{} / {}", fs_model::human_bytes(done_bytes), fs_model::human_bytes(total_bytes))
            } else {
                fs_model::human_bytes(done_bytes)
            };
            let label = if a.started || !a.label.is_empty() { a.label.as_str() } else { "Starting…" };
            let text = format!("{}/{} files · {bytes} · {label}", done_items.min(total_items), total_items);
            // Buttons pinned right; the bar takes the rest so long names fit.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if waiting > 0 && ui.button("Cancel all").clicked() {
                    for j in &self.active_jobs {
                        self.jobs.cancel(j.id);
                    }
                }
                if ui.button("Cancel").on_hover_text("Stop the job running now").clicked() {
                    self.jobs.cancel(job_id);
                }
                if waiting > 0 {
                    ui.label(format!("+{waiting} queued"))
                        .on_hover_text("Waiting jobs start when this one finishes. Their files count in the bar.");
                }
                // This file, chunk by chunk; an empty bar between files.
                if file_bar {
                    let (frac, text) = match file.filter(|f| f.2 > 0) {
                        Some((phase, done, total)) => (
                            done as f32 / total as f32,
                            format!("{phase} {} / {}", fs_model::human_bytes(done.min(total)), fs_model::human_bytes(total)),
                        ),
                        None => (0.0, "preparing…".to_owned()),
                    };
                    ui.add(egui::ProgressBar::new(frac.clamp(0.0, 1.0)).desired_width(200.0).text(text))
                        .on_hover_text("Current file");
                }
                ui.add(
                    egui::ProgressBar::new(frac.clamp(0.0, 1.0))
                        .desired_width(ui.available_width())
                        .text(text),
                );
            });
            return;
        }

        ui.horizontal(|ui| {
            // Counts live in each pane's own footer; here just say which
            // side has focus.
            ui.label(if self.editor_focused { "Editor" } else { self.focused.label() });

            if let Some(p) = self.preview.playing() {
                ui.separator();
                ui.label(format!("▶ {}", p.file_name().unwrap_or_default().to_string_lossy()));
            }

            if !self.status.is_empty() {
                ui.separator();
                ui.label(egui::RichText::new(&self.status).weak());
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(last) = self.errors.last() {
                    if ui
                        .button(
                            egui::RichText::new(format!("⚠ {}", truncate(last, 60)))
                                .color(ui.visuals().error_fg_color),
                        )
                        .on_hover_text(format!("{}\n\nclick to clear", self.errors.join("\n")))
                        .clicked()
                    {
                        self.errors.clear();
                        self.errors_logged = 0;
                    }
                }
            });
        });
    }

    /// "12 files · 3 folders · 45.2 MB", plus the selection when there is one.
    /// Counts what's listed, including children of folders expanded in tree mode.
    pub(super) fn pane_footer_text(tab: &DirTab) -> String {
        let (mut files, mut dirs, mut bytes) = (0usize, 0usize, 0u64);
        let (mut n_sel, mut sel_bytes) = (0usize, 0u64);
        for e in &tab.entries {
            if e.is_dir {
                dirs += 1;
            } else {
                files += 1;
                bytes += e.len;
            }
            if tab.selected.contains(&e.path) {
                n_sel += 1;
                if !e.is_dir {
                    sel_bytes += e.len;
                }
            }
        }
        let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let mut text = format!(
            "{} · {} · {}",
            plural(files, "file", "files"),
            plural(dirs, "folder", "folders"),
            fs_model::human_bytes(bytes)
        );
        if n_sel > 0 {
            text += &format!(" — {n_sel} selected ({})", fs_model::human_bytes(sel_bytes));
        }
        text
    }

    pub(super) fn show_pane(&mut self, ui: &mut egui::Ui, role: Role) {
        let focused = self.focused == role && !self.editor_focused;
        let on_device = self.pane(role).active_tab().device;
        let area = ui.max_rect().expand(4.0);
        if on_device {
            ui.painter().rect_filled(area, 0.0, theme::device_wash());
        }

        // Pane header: role, path, and the sub-tab strip.
        ui.horizontal(|ui| {
            if ui.add(theme::pane_title(role.label(), focused)).clicked() {
                self.focused = role;
                self.editor_focused = false;
            }
            let on_dev = self.pane(role).active_tab().device;
            if on_dev {
                self.device_badge(ui);
            } else {
                self.favourites_menu(ui, role);
                self.drives_menu(ui, role);
            }
            ui.separator();
            if !on_dev && self.path_edit.as_ref().is_some_and(|(r, _)| *r == role) {
                self.path_editor(ui, role);
            } else {
                let tab = self.pane(role).active_tab();
                let cwd = if tab.device { crate::device::dev_str(&tab.cwd) } else { tab.cwd.display().to_string() };
                let size = egui::TextStyle::Small.resolve(ui.style()).size + 1.0;
                let color = ui.visuals().text_color();
                let label = egui::Label::new(egui::RichText::new(cwd.clone()).size(size).color(color)).truncate();
                if on_dev {
                    ui.add(label);
                } else if ui.add(label.sense(egui::Sense::click())).on_hover_text("Click to type a path").clicked() {
                    self.path_edit = Some((role, cwd));
                }
            }
        });

        self.tab_strip(ui, role);
        ui.separator();

        // Per-pane footer: counts stay visible even while a transfer's
        // progress bar takes over the window's status bar.
        let footer = Self::pane_footer_text(self.pane(role).active_tab());
        egui::Panel::bottom(egui::Id::new(("pane_footer", role.label())))
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(0, 3)))
            .resizable(false)
            .show(ui, |ui| {
                ui.label(egui::RichText::new(footer).small().weak());
            });

        let other_folder = {
            let other = match role {
                Role::Source => &self.dest,
                Role::Dest => &self.source,
            };
            other.active_tab().cwd.file_name().map(|n| n.to_string_lossy().into_owned())
        };
        let intents = {
            let sizes = &self.sizes;
            let preview = &self.preview;
            let opened = &self.opened;
            let committed = &self.committed;
            // Borrow the pane field directly rather than via `pane_mut`, so
            // the borrow checker can see it is disjoint from `sizes`/`preview`.
            let pane = match role {
                Role::Source => &mut self.source,
                Role::Dest => &mut self.dest,
            };
            let tab = pane.active_tab_mut();
            file_table::show(
                ui,
                tab,
                TableCtx {
                    sizes,
                    preview,
                    opened,
                    committed,
                    pane_focused: focused,
                    clipboard: self.clipboard.as_ref(),
                    other_folder,
                },
            )
        };

        if on_device {
            ui.painter().rect_stroke(area.shrink(1.0), 0.0, theme::device_frame(), egui::StrokeKind::Inside);
        }

        for intent in intents {
            match intent {
                Intent::TookFocus => {
                    self.focused = role;
                    self.editor_focused = false;
                }
                Intent::Open(p) => {
                    let tab = self.pane(role).active_tab();
                    let on_dev = tab.device;
                    let is_dir = tab
                        .entries
                        .iter()
                        .find(|e| e.path == p)
                        .map_or_else(|| !on_dev && p.is_dir(), |e| e.is_dir);
                    if is_dir {
                        self.open_in(role, p);
                    } else if on_dev {
                        self.ask_device_load(p);
                    } else {
                        self.preview_toggle(&p);
                    }
                }
                Intent::BeginRename(p) => {
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.pane_mut(role).active_tab_mut().renaming = Some((p, name));
                }
                Intent::CommitRename { from, to } => {
                    let tab = self.pane_mut(role).active_tab_mut();
                    tab.renaming = None;
                    let on_dev = tab.device;
                    self.do_rename(&from, &to, on_dev);
                }
                Intent::CancelRename => {
                    self.pane_mut(role).active_tab_mut().renaming = None;
                }
                Intent::WantSize(paths) => {
                    if !self.pane(role).active_tab().device {
                        self.request_sizes(paths);
                    }
                }
                Intent::GoUp => self.go_up(role),
                Intent::Expand(p) => self.expand(role, p),
                Intent::AddFavourite(p) => self.add_favourite(p),
                Intent::Trash(paths) => {
                    if !paths.is_empty() {
                        let on_dev = self.pane(role).active_tab().device;
                        self.trash_pending = Some((on_dev, paths));
                    }
                }
                Intent::DeleteNow(paths) => {
                    if !paths.is_empty() && self.pane(role).active_tab().device {
                        self.delete_pending = Some(paths);
                    }
                }
                Intent::Dropped(payload) => {
                    let tab = self.pane(role).active_tab();
                    let (dest, to_dev) = (tab.cwd.clone(), tab.device);
                    let kind = self.drop_kind(ui, role, &payload, to_dev);
                    self.transfer(kind, payload.device, payload.paths, to_dev, dest);
                }
                Intent::DroppedOn { dir, payload } => {
                    let to_dev = self.pane(role).active_tab().device;
                    let kind = self.drop_kind(ui, role, &payload, to_dev);
                    self.transfer(kind, payload.device, payload.paths, to_dev, dir);
                }
                Intent::Cut(paths) => self.set_clipboard(Kind::Move, role, paths),
                Intent::Copy(paths) => self.set_clipboard(Kind::Copy, role, paths),
                Intent::Paste(target) => self.paste(role, target),
                Intent::Reveal(p) => reveal(&p),
                Intent::RemoveDuplicates(paths) => {
                    self.status = format!("Checking {} files for duplicates…", paths.len());
                    self.jobs.find_duplicates(paths);
                }
                Intent::NewFolder(dir) => self.new_folder(role, dir, None),
                Intent::NewFolderNamed { dir, name } => self.new_folder(role, dir, Some(name)),
                Intent::LoadFromDevice(p) => self.ask_device_load(p),
            }
        }
    }

    /// Connection state of the device, shown in a device tab's header.
    pub(super) fn device_badge(&mut self, ui: &mut egui::Ui) {
        let (text, color, tip) = match &self.device_status {
            Some(Ok(name)) => (format!("● {name}"), egui::Color32::from_rgb(90, 210, 110), "Connected over USB MIDI".to_owned()),
            Some(Err(e)) => ("● offline".to_owned(), ui.visuals().error_fg_color, e.clone()),
            None => ("● connecting…".to_owned(), ui.visuals().weak_text_color(), String::new()),
        };
        ui.label(egui::RichText::new(text).color(color).small()).on_hover_text(tip);
    }

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
                    if ui.small_button("×").on_hover_text("Remove from favourites").clicked() {
                        remove = Some(i);
                    }
                    let resp = ui
                        .add_enabled(exists, egui::Button::new(name).selected(*fav == cwd).frame(false))
                        .on_hover_text(fav.display().to_string())
                        .on_disabled_hover_text(format!("{} (not found)", fav.display()));
                    if resp.clicked() {
                        go = Some(fav.clone());
                        ui.close();
                    }
                });
            }
            ui.separator();
            let label = if is_fav { "Remove this folder" } else { "Add this folder" };
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
            if self.volumes.as_ref().is_none_or(|(t, _)| t.elapsed().as_secs_f32() > 2.0) {
                self.volumes = Some((std::time::Instant::now(), crate::fs_model::volumes()));
            }
            let home = dirs_home();
            if ui.add(egui::Button::new("🏠 Home").selected(cwd == home).frame(false)).on_hover_text(home.display().to_string()).clicked() {
                go = Some(home);
                ui.close();
            }
            ui.separator();
            let vols = self.volumes.as_ref().map(|(_, v)| v.as_slice()).unwrap_or_default();
            // The disk holding cwd: the longest volume path it sits under.
            let current = vols.iter().filter(|v| cwd.starts_with(&v.path)).max_by_key(|v| v.path.as_os_str().len()).map(|v| &v.path);
            for v in vols {
                let resp = ui
                    .add(egui::Button::new(v.name.as_str()).selected(current == Some(&v.path)).frame(false))
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
        let Some((_, buf)) = self.path_edit.as_mut() else { return };
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
                state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(n),
                )));
                state.store(ui.ctx(), id);
            }
            return;
        }
        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
        if !resp.lost_focus() {
            return;
        }
        let typed = std::mem::take(buf);
        self.path_edit = None;
        if !enter {
            return;
        }
        let Some(path) = typed_path(&typed) else { return };
        let target = if path.is_file() { path.parent().map(Path::to_path_buf) } else { Some(path.clone()) };
        match target {
            Some(dir) if dir.is_dir() => {
                self.focused = role;
                self.editor_focused = false;
                self.open_in(role, dir);
            }
            _ => self.errors.push(format!("{}: no such folder", path.display())),
        }
    }

    /// Whether a drag dropped into `role` moves or copies. Like Finder: on the
    /// same storage it moves, so the device to the device always moves
    /// (whichever pane), and so does the computer within one pane. Across
    /// storage it copies. Holding ⌥ forces a copy.
    pub(super) fn drop_kind(&self, ui: &egui::Ui, role: Role, payload: &DragPayload, to_dev: bool) -> Kind {
        if ui.input(|i| i.modifiers.alt) || payload.device != to_dev {
            return Kind::Copy;
        }
        let same_pane = self.pane(role).tabs.iter().any(|t| t.id == payload.tab_id);
        if payload.device || same_pane { Kind::Move } else { Kind::Copy }
    }

    /// The sub-tab strip. Each tab header is also a drop target, so you can
    /// drag onto a folder you have open without first switching to it.
    pub(super) fn tab_strip(&mut self, ui: &mut egui::Ui, role: Role) {
        let mut switch_to = None;
        let mut close = None;
        let mut close_others = None;
        let mut new_tab = false;
        let mut drop_on: Option<(usize, DragPayload)> = None;
        let mut new_device_tab = false;

        // Wraps onto extra rows instead of scrolling once tabs outgrow the pane.
        ui.horizontal_wrapped(|ui| {
            let active = self.pane(role).active;
            let titles: Vec<(usize, String, bool)> = self
                .pane(role)
                .tabs
                .iter()
                .enumerate()
                .map(|(i, t)| (i, t.title(), t.loading))
                .collect();

            for (i, title, loading) in titles {
                let label = if loading {
                    format!("{title} …")
                } else {
                    title
                };
                let (inner, payload) =
                    ui.dnd_drop_zone::<DragPayload, _>(egui::Frame::NONE, |ui| {
                        ui.selectable_label(i == active, label)
                    });
                if inner.inner.clicked() {
                    switch_to = Some(i);
                }
                if inner.inner.middle_clicked() {
                    close = Some(i);
                }
                let n_tabs = self.pane(role).tabs.len();
                inner.inner.context_menu(|ui| {
                    if ui
                        .add_enabled(n_tabs > 1, egui::Button::new("Close this"))
                        .clicked()
                    {
                        close = Some(i);
                        ui.close();
                    }
                    if ui
                        .add_enabled(n_tabs > 1, egui::Button::new("Close others"))
                        .clicked()
                    {
                        close_others = Some(i);
                        ui.close();
                    }
                });
                if let Some(p) = payload {
                    drop_on = Some((i, (*p).clone()));
                }
            }

            if ui.small_button("+").on_hover_text("New tab").clicked() {
                new_tab = true;
            }
            if ui
                .small_button("🎹")
                .on_hover_text("Open the Elektron device's +Drive in a new tab (USB MIDI)")
                .clicked()
            {
                new_device_tab = true;
            }
            if self.pane(role).tabs.len() > 1
                && ui.small_button("×").on_hover_text("Close tab").clicked()
            {
                close = Some(self.pane(role).active);
            }
        });

        if let Some(i) = switch_to {
            self.pane_mut(role).active = i;
            self.focused = role;
            self.editor_focused = false;
        }
        if let Some((i, p)) = drop_on {
            if let Some(tab) = self.pane(role).tabs.get(i) {
                let (dest, to_dev) = (tab.cwd.clone(), tab.device);
                let kind = self.drop_kind(ui, role, &p, to_dev);
                self.transfer(kind, p.device, p.paths, to_dev, dest);
            }
        }
        if new_tab || new_device_tab {
            let cur = self.pane(role).active_tab();
            let (cwd, device) = (cur.cwd.clone(), cur.device);
            let id = self.new_tab_id();
            let tab = if new_device_tab {
                DirTab::new_device(id, PathBuf::from("/"))
            } else if device {
                DirTab::new_device(id, cwd)
            } else {
                DirTab::new(id, cwd)
            };
            self.pane_mut(role).add_tab(tab);
            self.focused = role;
            self.editor_focused = false;
            let i = self.pane(role).active;
            self.reload(role, i);
        }
        if let Some(i) = close {
            self.pane_mut(role).close_tab(i);
        }
        if let Some(i) = close_others {
            self.pane_mut(role).close_other_tabs(i);
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
