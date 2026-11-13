//! One file pane: its header, file table, footer and tab strip, and what
//! dropping something on them does.

use super::*;
use std::collections::HashMap;

impl App {
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
        let plural =
            |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
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
                let cwd = if tab.device {
                    crate::device::dev_str(&tab.cwd)
                } else {
                    tab.cwd.display().to_string()
                };
                let size = egui::TextStyle::Small.resolve(ui.style()).size + 1.0;
                let color = ui.visuals().text_color();
                let label =
                    egui::Label::new(egui::RichText::new(cwd.clone()).size(size).color(color))
                        .truncate();
                if on_dev {
                    ui.add(label);
                } else if ui
                    .add(label.sense(egui::Sense::click()))
                    .on_hover_text("Click to type a path")
                    .clicked()
                {
                    self.path_edit = Some((role, cwd));
                }
            }
        });

        self.tab_strip(ui, role);
        ui.separator();

        // Per-pane footer: counts stay visible even while a transfer's
        // progress bar takes over the window's status bar.
        let footer = Self::pane_footer_text(self.pane(role).active_tab());
        egui::Panel::bottom(egui::Id::unique(("pane_footer", role.label())))
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
            other
                .active_tab()
                .cwd
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        };
        if let Some(mr) = self.pane_mut(role).active_tab_mut().multi_rename.as_mut() {
            let outcome = crate::ui::rename_view::show(ui, mr, focused);
            if ui.ui_contains_pointer() && ui.input(|i| i.pointer.any_pressed()) {
                self.focused = role;
                self.editor_focused = false;
            }
            if on_device {
                ui.painter().rect_stroke(
                    area.shrink(1.0),
                    0.0,
                    theme::device_frame(),
                    egui::StrokeKind::Inside,
                );
            }
            match outcome {
                Some(crate::ui::rename_view::Outcome::Apply(rows)) => {
                    self.apply_multi_rename(role, &rows)
                }
                Some(crate::ui::rename_view::Outcome::Cancel) => {
                    self.pane_mut(role).active_tab_mut().multi_rename = None;
                    self.status = "Rename cancelled".to_owned();
                }
                None => {}
            }
            return;
        }

        let slot_clip = if on_device || self.clipboard.is_some() { 0 } else { self.elek.clip_keys().len() };
        let intents = {
            let sizes = &self.sizes;
            let preview = &self.preview;
            let opened = &self.opened;
            let committed = &self.committed;
            let saving = &self.saving;
            let links = &self.links;
            let changed = &self.changed_paths;
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
                    saving,
                    links,
                    changed,
                    pane_focused: focused,
                    clipboard: self.clipboard.as_ref(),
                    slot_clip,
                    other_folder,
                },
            )
        };

        if on_device {
            ui.painter().rect_stroke(
                area.shrink(1.0),
                0.0,
                theme::device_frame(),
                egui::StrokeKind::Inside,
            );
        } else if let Some(keys) = self.elek.dragged_slots().filter(|k| !k.is_empty()) {
            // Pool slots dragged out of the Samples page: download them here.
            // (The table's drop zone eats the slot payload, so go by pointer.)
            if ui.rect_contains_pointer(ui.max_rect()) {
                ui.painter().rect_stroke(
                    ui.max_rect().shrink(1.0),
                    2.0,
                    egui::Stroke::new(2.0, theme::ACCENT_BRIGHT),
                    egui::StrokeKind::Inside,
                );
                ui.ctx().set_cursor_icon(egui::CursorIcon::Copy);
                if ui.input(|i| i.pointer.any_released()) {
                    let dest = self.pane(role).active_tab().cwd.clone();
                    self.slots_dropped(role, dest, keys);
                }
            }
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
                        // A device file we have locally plays like a local one.
                        match self.linked_local(&p) {
                            Some(local) => self.preview_device_sample(&local),
                            None => {
                                self.engine.stop();
                                self.editor.play_when_loaded = false;
                                self.ask_device_load(p);
                            }
                        }
                    } else {
                        self.preview_toggle(&p);
                    }
                }
                Intent::UseAsKit(p) => self.use_as_kit(&p),
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
                    let key = tab
                        .entries
                        .iter()
                        .find(|e| e.path == from && !e.is_dir && e.hash != 0)
                        .map(|e| (e.hash, e.len as u32));
                    self.do_rename(&from, &to, on_dev, key);
                }
                Intent::BeginMultiRename => {
                    self.pane_mut(role).active_tab_mut().begin_multi_rename()
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
                    let on_dev = self.pane(role).active_tab().device;
                    self.trash_now(on_dev, paths);
                }
                Intent::DeleteNow(paths) => {
                    if !paths.is_empty() && self.pane(role).active_tab().device {
                        self.delete_pending = Some(paths);
                    }
                }
                Intent::Dropped(payload) => {
                    let tab = self.pane(role).active_tab();
                    let (dest, to_dev) = (tab.cwd.clone(), tab.device);
                    let kind = self.drop_kind(ui, &payload, to_dev);
                    self.transfer(kind, payload.device, payload.paths, to_dev, dest);
                }
                Intent::DroppedOn { dir, payload } => {
                    let to_dev = self.pane(role).active_tab().device;
                    let kind = self.drop_kind(ui, &payload, to_dev);
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
                Intent::NewFolder { dir, base } => self.new_folder(role, dir, None, base),
                Intent::NewFolderNamed { dir, name } => {
                    self.new_folder(role, dir, Some(name), None)
                }
                Intent::NewFolderWith { dir, paths, name } => {
                    self.new_folder_with(role, dir, paths, name)
                }
                Intent::LoadFromDevice(p) => self.ask_device_load(p),
                Intent::RevealLocal(p) => {
                    if let Some(local) = self.local_copy_of(&p) {
                        reveal(&local);
                    }
                }
                Intent::ShowLocal(p) => {
                    if let Some(local) = self.local_copy_of(&p) {
                        self.show_file(role.other(), false, &local);
                    }
                }
                Intent::ShowOnDevice(p) => {
                    if let Some(dev) = self.links.for_local(&p).map(|l| PathBuf::from(&l.device)) {
                        // Prefer a device tab already open, in either pane.
                        let role = [role.other(), role]
                            .into_iter()
                            .find(|&r| self.pane(r).tabs.iter().any(|t| t.device))
                            .unwrap_or(role.other());
                        self.show_file(role, true, &dev);
                    }
                }
                Intent::MonoCopy { dirs, folds } => {
                    for d in dirs {
                        self.mono_copy(d, folds.clone());
                    }
                }
            }
        }
    }

    /// Connection state of the device, shown in a device tab's header.
    /// React to the device being plugged in or out (see
    /// [`crate::device::presence`]): mark it offline at once, and on the way
    /// back re-list device tabs so they reconnect without a manual Refresh.
    pub(super) fn track_presence(&mut self) {
        use crate::device::Presence;
        use crate::ui::elek_view::DevicePick;
        if self.elek_mode {
            self.device_last = Some(DevicePick::Page(self.elek.page));
        } else if self.drive_in_col() {
            self.device_last = Some(DevicePick::Drive);
        }
        let now = crate::device::presence();
        if now == self.last_presence {
            return;
        }
        let before = std::mem::replace(&mut self.last_presence, now.clone());
        match now {
            Presence::Absent => {
                self.device_status = Some(Err("Disconnected: no Elektron device on USB MIDI".into()));
                // Unplugged: fold the device panels like the switch does,
                // but keep watching, so plugging back in lights it up again.
                if before != Presence::Unknown {
                    self.fold_device();
                }
            }
            Presence::Off => {
                self.device_status = Some(Err("Switched off".into()));
            }
            Presence::One(name) if matches!(before, Presence::Absent | Presence::Many(_) | Presence::Off)
                // Swapped so fast no scan saw the cable empty.
                || matches!(&before, Presence::One(b) if *b != name) =>
            {
                let other = self.last_unit.replace(name.clone()).is_some_and(|u| u != name);
                self.device_status = None;
                for role in [Role::Source, Role::Dest] {
                    for i in 0..self.pane(role).tabs.len() {
                        if self.pane(role).tabs[i].device {
                            self.reload(role, i);
                        }
                    }
                }
                if self.elek.asked_projects {
                    self.elek.projects = None;
                    self.device.elek(crate::device::ElekCmd::ListProjects);
                }
                self.device_reconnected(other);
                // Back where the user was in the device group.
                match self.device_last {
                    Some(DevicePick::Page(page)) => {
                        self.elek.page = page;
                        self.enter_full_elek();
                    }
                    Some(DevicePick::Drive) => self.show_drive(),
                    None => {}
                }
            }
            Presence::One(name) => self.last_unit = Some(name),
            _ => {}
        }
    }

    pub(super) fn device_badge(&mut self, ui: &mut egui::Ui) {
        if let Some(tip) = crate::device::presence().problem() {
            let many = matches!(crate::device::presence(), crate::device::Presence::Many(_));
            let off = crate::device::presence() == crate::device::Presence::Off;
            let text = if many { "⚠ several devices" } else if off { "○ switched off" } else { "● disconnected" };
            ui.label(egui::RichText::new(text).color(ui.visuals().error_fg_color).small())
                .on_hover_text(tip);
            return;
        }
        let (text, color, tip) = match &self.device_status {
            Some(Ok(name)) => (
                format!("● {name}"),
                egui::Color32::from_rgb(90, 210, 110),
                "Connected over USB MIDI".to_owned(),
            ),
            Some(Err(e)) => (
                "● offline".to_owned(),
                ui.visuals().error_fg_color,
                e.clone(),
            ),
            None => (
                "● connecting…".to_owned(),
                ui.visuals().weak_text_color(),
                String::new(),
            ),
        };
        ui.label(egui::RichText::new(text).color(color).small())
            .on_hover_text(tip);
    }

    /// Whether a drop moves or copies. On the same storage (computer to
    /// computer, device to device) it moves, whichever pane; holding ⌥ or
    /// Ctrl makes it a copy. Between the computer and the device, and from
    /// Finder / Explorer, it always copies.
    pub(super) fn drop_kind(&self, ui: &egui::Ui, payload: &DragPayload, to_dev: bool) -> Kind {
        if payload.external || copy_held(ui.ctx()) || payload.device != to_dev {
            Kind::Copy
        } else {
            Kind::Move
        }
    }

    /// Whether `p` (from a drag payload) is a folder. Device paths are looked
    /// up in the listings we already have, since there's no disk to ask.
    fn payload_is_dir(&self, p: &std::path::Path, device: bool) -> bool {
        if !device {
            return p.is_dir();
        }
        [Role::Source, Role::Dest].iter().any(|&r| {
            self.pane(r)
                .tabs
                .iter()
                .filter(|t| t.device)
                .any(|t| t.entries.iter().any(|e| e.is_dir && e.path == p))
        })
    }

    /// Open each folder in `payload` as a new tab in this pane.
    fn open_dropped_as_tabs(&mut self, role: Role, payload: DragPayload) {
        let dirs: Vec<PathBuf> = payload
            .paths
            .into_iter()
            .filter(|p| self.payload_is_dir(p, payload.device))
            .collect();
        if dirs.is_empty() {
            self.status = "Drop folders on the tab bar to open them as tabs".to_owned();
            return;
        }
        for dir in dirs {
            let id = self.new_tab_id();
            let tab = if payload.device {
                DirTab::new_device(id, dir)
            } else {
                DirTab::new(id, dir)
            };
            self.pane_mut(role).add_tab(tab);
            let i = self.pane(role).active;
            self.reload(role, i);
        }
        self.focused = role;
        self.editor_focused = false;
    }

    /// The sub-tab strip. Each tab header is also a drop target, so you can
    /// drag onto a folder you have open without first switching to it.
    pub(super) fn tab_strip(&mut self, ui: &mut egui::Ui, role: Role) {
        let mut switch_to = None;
        let mut close = None;
        let mut close_others = None;
        let mut to_other: Option<(usize, bool)> = None; // (tab, is_move)
        let other_label = role.other().label();
        let mut new_tab = false;
        let mut drop_on: Option<(usize, DragPayload)> = None;
        let mut drop_new_tab: Option<DragPayload> = None;
        let mut new_device_tab = false;

        // Show a copy cursor while a drag would copy because of the modifier.
        if copy_held(ui.ctx()) && egui::DragAndDrop::has_payload_of_type::<DragPayload>(ui.ctx()) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Copy);
        }

        // The Destination's +Drive is a mode of the right column (its
        // +DRIVE switch), not tabs: there the strip shows only local tabs,
        // and none at all while the +Drive shows.
        let local_only = role == Role::Dest;
        if local_only && self.pane(role).active_tab().device {
            return;
        }
        let shown = |t: &DirTab| !(local_only && t.device);

        // One row: tabs scroll sideways when they outgrow the pane, while the
        // + / device / × buttons stay pinned on the right.
        let n_tabs = self.pane(role).tabs.iter().filter(|t| shown(t)).count();
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if n_tabs > 1 && ui.add(crate::ui::theme::close_button(None, true)).on_hover_text("Close tab").clicked() {
                    close = Some(self.pane(role).active);
                }
                if !local_only {
                    let mut dev_btn = theme::device_dimmed(ui, |ui| ui.small_button(theme::DEVICE_ICON))
                        .on_hover_text("Open the Elektron device's +Drive in a new tab (USB MIDI)");
                    if let Some(why) = crate::device::presence().problem() {
                        dev_btn = dev_btn.on_hover_text(why);
                    }
                    if dev_btn.clicked() {
                        new_device_tab = true;
                    }
                }
                if ui.small_button("+").on_hover_text("New tab").clicked() {
                    new_tab = true;
                }

                let active = self.pane(role).active;
                // Bring the active tab into view whenever it changes.
                let seen_id = egui::Id::unique(("tab_strip_active", role.label()));
                let changed = ui.data(|d| d.get_temp::<usize>(seen_id)) != Some(active);
                ui.data_mut(|d| d.insert_temp(seen_id, active));

                // Thin 1px orange scrollbar overlaid on the tab strip, so it
                // doesn't cover the tabs. The tabs themselves keep the normal style.
                let tab_style = ui.style().clone();
                {
                    let s = ui.style_mut();
                    let sc = &mut s.spacing.scroll;
                    sc.floating = true;
                    sc.bar_width = 1.0;
                    sc.floating_width = 1.0;
                    sc.floating_allocated_width = 0.0;
                    sc.bar_inner_margin = 0.0;
                    sc.bar_outer_margin = 0.0;
                    sc.foreground_color = false;
                    sc.dormant_background_opacity = 0.0;
                    sc.active_background_opacity = 0.0;
                    sc.interact_background_opacity = 0.0;
                    sc.dormant_handle_opacity = 1.0;
                    sc.active_handle_opacity = 1.0;
                    sc.interact_handle_opacity = 1.0;
                    let w = &mut s.visuals.widgets;
                    for v in [&mut w.inactive, &mut w.hovered, &mut w.active] {
                        v.bg_fill = crate::ui::theme::ACCENT_BRIGHT;
                        v.corner_radius = egui::CornerRadius::ZERO;
                    }
                }
                egui::ScrollArea::horizontal()
                    .id_salt(("tab_strip", role.label()))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.set_style(tab_style.clone());
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Max), |ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            let titles: Vec<(usize, String, bool, bool)> = self
                                .pane(role)
                                .tabs
                                .iter()
                                .enumerate()
                                .filter(|(_, t)| shown(t))
                                .map(|(i, t)| (i, t.title(), t.loading, t.device))
                                .collect();

                            let mut tab_h = ui.spacing().interact_size.y;
                            for (i, title, loading, is_dev) in titles {
                                let label = if loading {
                                    format!("{title} …")
                                } else {
                                    title
                                };
                                let (inner, payload) = ui
                                    .dnd_drop_zone::<DragPayload, _>(egui::Frame::NONE, |ui| {
                                        if is_dev {
                                            theme::device_dimmed(ui, |ui| ui.add(theme::pane_tab(&label, i == active)))
                                        } else {
                                            ui.add(theme::pane_tab(&label, i == active))
                                        }
                                    });
                                let resp = inner.inner;
                                tab_h = resp.rect.height();
                                if i == active && changed {
                                    resp.scroll_to_me(None);
                                }
                                if resp.clicked() {
                                    switch_to = Some(i);
                                }
                                if resp.middle_clicked() {
                                    close = Some(i);
                                }
                                resp.context_menu(|ui| {
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
                                    ui.separator();
                                    if ui
                                        .add_enabled(
                                            n_tabs > 1,
                                            egui::Button::new(format!("Move to {other_label}")),
                                        )
                                        .clicked()
                                    {
                                        to_other = Some((i, true));
                                        ui.close();
                                    }
                                    if ui.button(format!("Copy to {other_label}")).clicked() {
                                        to_other = Some((i, false));
                                        ui.close();
                                    }
                                });
                                if let Some(p) = payload {
                                    drop_on = Some((i, (*p).clone()));
                                }
                            }
                            // The empty rest of the strip: dropping folders
                            // here opens them as new tabs.
                            let w = ui.available_width().max(40.0);
                            let (_, payload) =
                                ui.dnd_drop_zone::<DragPayload, _>(egui::Frame::NONE, |ui| {
                                    ui.allocate_exact_size(
                                        egui::vec2(w, tab_h),
                                        egui::Sense::hover(),
                                    )
                                });
                            if let Some(p) = payload {
                                drop_new_tab = Some((*p).clone());
                            }
                        });
                    });
                ui.set_style(tab_style);
            });
        });

        if let Some(i) = switch_to {
            self.pane_mut(role).active = i;
            self.focused = role;
            self.editor_focused = false;
        }
        if let Some((i, p)) = drop_on {
            if let Some(tab) = self.pane(role).tabs.get(i) {
                let (dest, to_dev) = (tab.cwd.clone(), tab.device);
                let kind = self.drop_kind(ui, &p, to_dev);
                self.transfer(kind, p.device, p.paths, to_dev, dest);
            }
        }
        if let Some(p) = drop_new_tab {
            self.open_dropped_as_tabs(role, p);
        }
        if new_tab || new_device_tab {
            let cur = self.pane(role).active_tab();
            let (cwd, device) = (cur.cwd.clone(), cur.device);
            let id = self.new_tab_id();
            // When this pane holds only device tabs, "+" brings back a
            // computer folder: the other pane's (active, else any) local tab.
            let only_device = self.pane(role).tabs.iter().all(|t| t.device);
            let local_from_other = || {
                let other = self.pane(role.other());
                let act = other.active_tab();
                if !act.device {
                    Some(act.cwd.clone())
                } else {
                    other.tabs.iter().find(|t| !t.device).map(|t| t.cwd.clone())
                }
            };
            let tab = if new_device_tab {
                DirTab::new_device(id, PathBuf::from("/"))
            } else if only_device {
                DirTab::new(id, local_from_other().unwrap_or_else(dirs_home))
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
        if let Some((i, is_move)) = to_other {
            let other = role.other();
            if is_move {
                if let Some(tab) = self.pane_mut(role).take_tab(i) {
                    self.pane_mut(other).add_tab(tab);
                }
            } else if let Some(src) = self.pane(role).tabs.get(i) {
                let (cwd, device) = (src.cwd.clone(), src.device);
                let id = self.new_tab_id();
                let tab = if device {
                    DirTab::new_device(id, cwd)
                } else {
                    DirTab::new(id, cwd)
                };
                self.pane_mut(other).add_tab(tab);
                let j = self.pane(other).active;
                self.reload(other, j);
            }
        }
    }
}

/// `name` with slot `label` in front (`060_kick.wav`), unless it already
/// starts with it.
pub(super) fn slot_prefixed(label: &str, name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let l = label.to_ascii_lowercase();
    if [format!("{l}_"), format!("{l} "), format!("{l}-")].iter().any(|p| lower.starts_with(p.as_str())) {
        name.to_owned()
    } else {
        format!("{label}_{name}")
    }
}

/// ⌥ or Ctrl held: a drop copies instead of moving.
fn copy_held(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.modifiers.alt || i.modifiers.ctrl)
}

impl App {
    /// The local file a device file was linked to, if it is still on disk
    /// (edited since or not). Says why not in the status bar.
    pub(super) fn local_copy_of(&mut self, device_path: &Path) -> Option<PathBuf> {
        let e = self.device_entry(device_path)?;
        let local = self.links.known(e.hash, e.len as u32).map(|l| l.local.clone());
        match local {
            Some(p) if p.exists() => Some(p),
            Some(p) => {
                self.status = format!("{} is no longer there", p.display());
                None
            }
            None => None,
        }
    }

    /// Pool samples dropped on a local pane: copy their +Drive files into
    /// its folder, unless some are already here locally (then ask first).
    /// `keys` come with their slot labels. Also how pasting copied slots lands.
    pub(super) fn slots_dropped(&mut self, role: Role, dest: PathBuf, keys: Vec<((u32, u32), String)>) {
        let mut paths = Vec::new();
        let mut labels = HashMap::new();
        let mut have = Vec::new();
        let mut unknown = 0;
        for (k, label) in keys {
            let Some(dev) = self.elek.names.get(&k).map(PathBuf::from) else {
                unknown += 1;
                continue;
            };
            if let Some(l) = self.links.user_copy(k.0, k.1) {
                have.push((dev.clone(), l.local.clone()));
            }
            labels.insert(dev.clone(), label);
            paths.push(dev);
        }
        if paths.is_empty() {
            self.status = "The device hasn't said where these samples live on the +Drive yet; try again in a moment".into();
            return;
        }
        if unknown > 0 {
            self.status = format!("{unknown} sample(s) skipped: their +Drive location isn't known yet");
        }
        if have.is_empty() {
            self.download_slots(dest, paths, labels);
        } else {
            self.slot_download_pending = Some(SlotDownload { role, dest, paths, have, labels });
        }
    }

    /// Copies are going to be made: with several samples, ask first whether
    /// to put their slot numbers in front of the file names.
    fn download_slots(&mut self, dest: PathBuf, paths: Vec<PathBuf>, labels: HashMap<PathBuf, String>) {
        if paths.is_empty() {
            return;
        }
        if labels.len() > 1 {
            self.slot_prefix_pending = Some(SlotPrefix { dest, paths, labels });
        } else {
            self.transfer(Kind::Copy, true, paths, false, dest);
        }
    }

    pub(super) fn answer_slot_prefix(&mut self, p: SlotPrefix, choice: dialogs::SlotPrefixChoice) {
        use dialogs::SlotPrefixChoice as C;
        match choice {
            C::Cancel => self.status = "Download cancelled".into(),
            C::Plain => self.transfer(Kind::Copy, true, p.paths, false, p.dest),
            C::Numbered => {
                let names: HashMap<PathBuf, String> = p
                    .paths
                    .iter()
                    .filter_map(|src| {
                        let name = src.file_name()?.to_string_lossy().into_owned();
                        let label = p.labels.get(src)?;
                        Some((src.clone(), slot_prefixed(label, &name)))
                    })
                    .collect();
                self.transfer_as(Kind::Copy, true, p.paths, false, p.dest, &names);
            }
        }
    }

    pub(super) fn answer_slot_download(&mut self, d: SlotDownload, choice: dialogs::SlotDownloadChoice) {
        use dialogs::SlotDownloadChoice as C;
        match choice {
            C::Cancel => self.status = "Download cancelled".into(),
            C::All => self.download_slots(d.dest, d.paths, d.labels),
            C::Missing => {
                let paths: Vec<PathBuf> =
                    d.paths.into_iter().filter(|p| !d.have.iter().any(|(h, _)| h == p)).collect();
                self.download_slots(d.dest, paths, d.labels);
            }
            C::Show => {
                let Some((_, first)) = d.have.first() else { return };
                self.show_file(d.role, false, first);
                // Select the others too when they sit in the same folder.
                let dir = first.parent().map(Path::to_path_buf);
                for r in [d.role, d.role.other()] {
                    let tab = self.pane_mut(r).active_tab_mut();
                    if !tab.device && Some(&tab.cwd) == dir.as_ref() {
                        for (_, l) in &d.have {
                            if l.parent() == dir.as_deref() {
                                tab.selected.insert(l.clone());
                            }
                        }
                        break;
                    }
                }
                self.focused = d.role;
                self.editor_focused = false;
                self.status = match d.have.len() {
                    1 => format!("Already here: {}", first.display()),
                    n => format!("{n} samples already have local copies; the first is selected"),
                };
            }
        }
    }

    /// Put the cursor on `file` (a `device` or local path) without taking
    /// any tab away from where the user left it: a tab already in that
    /// folder (`role`'s pane first) just moves its cursor; otherwise `role`
    /// gets a tab for it, reusing the one an earlier jump opened if it
    /// hasn't been navigated by hand since.
    pub(super) fn show_file(&mut self, role: Role, device: bool, file: &Path) {
        let Some(dir) = file.parent().map(Path::to_path_buf) else { return };
        let here = [role, role.other()].into_iter().find_map(|r| {
            let i = self
                .pane(r)
                .tabs
                .iter()
                .position(|t| t.device == device && t.cwd == dir)?;
            Some((r, i))
        });
        if let Some((r, i)) = here {
            self.pane_mut(r).active = i;
            let tab = self.pane_mut(r).active_tab_mut();
            match tab.entries.iter().position(|e| e.path == file) {
                Some(j) => tab.set_cursor(j),
                None => {
                    // Not listed yet (just arrived?): list again, then select.
                    tab.selected.clear();
                    tab.selected.insert(file.to_path_buf());
                    tab.cursor = None;
                    self.reload(r, i);
                }
            }
            return;
        }
        let reuse = self
            .pane(role)
            .tabs
            .iter()
            .position(|t| t.revealed && t.device == device);
        match reuse {
            Some(i) => self.pane_mut(role).active = i,
            None => {
                let id = self.new_tab_id();
                let tab = if device {
                    DirTab::new_device(id, dir.clone())
                } else {
                    DirTab::new(id, dir.clone())
                };
                self.pane_mut(role).add_tab(tab);
            }
        }
        let tab = self.pane_mut(role).active_tab_mut();
        tab.navigate_to(dir);
        tab.revealed = true;
        tab.selected.insert(file.to_path_buf());
        let i = self.pane(role).active;
        self.reload(role, i);
    }
}
