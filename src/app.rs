//! Application state and the frame loop.
//!
//! Two structural rules this file exists to enforce:
//!
//! 1. `ui()` only reads cached state and emits requests. Every filesystem
//!    read or write goes through [`crate::jobs`] on a worker thread.
//! 2. Worker results are absorbed in [`App::logic`], which eframe calls before
//!    each `ui()` pass *and* while the window is hidden -- so results are
//!    never dropped on the floor.

use crate::fs_model::{self, SortKey};
use crate::jobs::{Ev, Jobs};
use crate::keymap::{Action, Keymap};
use crate::ops::{self, Kind, Plan, Policy};
use crate::pane::{Pane, Role};
use crate::preview::Preview;
use crate::sizes::SizeCache;
use crate::state;
use crate::tab::DirTab;
use crate::ui::dialogs::{self, Decision};
use crate::ui::file_table::{self, DragPayload, Intent, TableCtx};
use std::path::{Path, PathBuf};

/// A transfer the user has been asked about and not yet answered.
struct PendingPlan {
    plan: Plan,
}

struct ActiveJob {
    id: u64,
    label: String,
    done_bytes: u64,
    total_bytes: u64,
    done_items: usize,
    total_items: usize,
}

pub struct App {
    jobs: Jobs,
    keymap: Keymap,
    sizes: SizeCache,
    preview: Preview,
    source: Pane,
    dest: Pane,
    focused: Role,
    next_tab_id: u64,
    show_hidden: bool,
    pending: Option<PendingPlan>,
    active_job: Option<ActiveJob>,
    status: String,
    errors: Vec<String>,
    /// Last state seen by `logic`, to notice changes that need saving.
    last_state: Option<state::AppState>,
}

impl App {
    pub fn new(ctx: egui::Context, storage: Option<&dyn eframe::Storage>) -> Self {
        let home = dirs_home();
        let saved = state::load(storage);
        let mut next_tab_id = 0;
        let mut mk = |p: PathBuf| {
            next_tab_id += 1;
            DirTab::new(next_tab_id, p)
        };
        let mut restore = |role: Role, ps: Option<&state::PaneState>| -> Pane {
            let mut tabs: Vec<DirTab> = ps
                .map(|ps| ps.tabs.as_slice())
                .unwrap_or_default()
                .iter()
                .filter_map(|t| {
                    let mut tab = mk(state::existing_dir(&t.cwd)?);
                    tab.sort_key = t.sort_key;
                    tab.sort_asc = t.sort_asc;
                    Some(tab)
                })
                .collect();
            if tabs.is_empty() {
                tabs.push(mk(home.clone()));
            }
            let active = ps.map_or(0, |ps| ps.active).min(tabs.len() - 1);
            Pane { role, tabs, active }
        };
        let source = restore(Role::Source, saved.as_ref().map(|s| &s.source));
        let dest = restore(Role::Dest, saved.as_ref().map(|s| &s.dest));

        let mut app = Self {
            jobs: Jobs::new(ctx),
            keymap: Keymap::platform_default(),
            sizes: SizeCache::default(),
            preview: Preview::default(),
            source,
            dest,
            focused: saved.as_ref().map_or(Role::Source, |s| s.focused),
            next_tab_id,
            show_hidden: saved.as_ref().is_some_and(|s| s.show_hidden),
            pending: None,
            active_job: None,
            status: String::new(),
            errors: Vec::new(),
            last_state: None,
        };
        app.reload_all();
        app
    }

    fn snapshot(&self) -> state::AppState {
        let pane = |p: &Pane| state::PaneState {
            tabs: p
                .tabs
                .iter()
                .map(|t| state::TabState {
                    cwd: t.cwd.clone(),
                    sort_key: t.sort_key,
                    sort_asc: t.sort_asc,
                })
                .collect(),
            active: p.active,
        };
        state::AppState {
            source: pane(&self.source),
            dest: pane(&self.dest),
            focused: self.focused,
            show_hidden: self.show_hidden,
        }
    }

    fn pane(&self, role: Role) -> &Pane {
        match role {
            Role::Source => &self.source,
            Role::Dest => &self.dest,
        }
    }

    fn pane_mut(&mut self, role: Role) -> &mut Pane {
        match role {
            Role::Source => &mut self.source,
            Role::Dest => &mut self.dest,
        }
    }

    fn new_tab_id(&mut self) -> u64 {
        self.next_tab_id += 1;
        self.next_tab_id
    }

    // ---- requests ---------------------------------------------------------

    fn reload(&mut self, role: Role, tab_idx: usize) {
        let show_hidden = self.show_hidden;
        let pane = self.pane_mut(role);
        if let Some(tab) = pane.tabs.get_mut(tab_idx) {
            let generation = tab.begin_reload();
            let cwd = tab.cwd.clone();
            self.jobs.list(cwd, generation, show_hidden);
        }
    }

    fn reload_all(&mut self) {
        for role in [Role::Source, Role::Dest] {
            for i in 0..self.pane(role).tabs.len() {
                self.reload(role, i);
            }
        }
    }

    /// Re-list any tab showing a directory at or under `dir`.
    fn reload_showing(&mut self, dir: &Path) {
        let mut todo = Vec::new();
        for role in [Role::Source, Role::Dest] {
            for (i, tab) in self.pane(role).tabs.iter().enumerate() {
                if tab.cwd == dir || dir.starts_with(&tab.cwd) {
                    todo.push((role, i));
                }
            }
        }
        for (role, i) in todo {
            self.reload(role, i);
        }
    }

    fn request_sizes(&mut self, paths: Vec<PathBuf>) {
        for p in paths {
            if p.is_dir() && self.sizes.request(&p) {
                self.jobs.dir_size(p);
            }
        }
    }

    // ---- the core verb ----------------------------------------------------

    /// Commit the source pane's selection into the active destination tab.
    ///
    /// Deliberately directional rather than Commander's "active pane -> other
    /// pane": the panes have fixed roles here. To get the Commander behaviour,
    /// swap the two role arguments for `(self.focused, self.focused.other())`.
    fn commit(&mut self, kind: Kind) {
        let sources = self.pane(Role::Source).active_tab().effective_selection();
        if sources.is_empty() {
            self.status = "Nothing selected in the source pane".to_owned();
            return;
        }
        let dest_dir = self.pane(Role::Dest).active_tab().cwd.clone();
        self.start_transfer(kind, sources, dest_dir);
    }

    fn start_transfer(&mut self, kind: Kind, sources: Vec<PathBuf>, dest_dir: PathBuf) {
        let plan = ops::plan(kind, &sources, &dest_dir, |p| self.sizes.bytes_of(p));

        for (path, why) in &plan.rejected {
            self.errors.push(format!("{}: {why}", path.display()));
        }
        if plan.ops.is_empty() {
            self.status = "Nothing to transfer".to_owned();
            return;
        }
        if plan.conflicts() > 0 {
            // Hold the plan and let the modal decide. No bytes move until then.
            self.pending = Some(PendingPlan { plan });
            return;
        }
        self.launch(&plan, Policy::KeepBoth);
    }

    fn launch(&mut self, plan: &Plan, policy: Policy) {
        let resolved = ops::resolve(plan, policy);
        if resolved.is_empty() {
            self.status = "All items skipped".to_owned();
            return;
        }
        let total_items = resolved.len();
        let total_bytes = resolved.iter().map(|o| o.approx_bytes).sum();
        let id = self.jobs.execute(plan.kind, resolved);
        self.active_job = Some(ActiveJob {
            id,
            label: String::new(),
            done_bytes: 0,
            total_bytes,
            done_items: 0,
            total_items,
        });
    }

    // ---- worker results ---------------------------------------------------

    fn absorb(&mut self, events: Vec<Ev>) {
        for ev in events {
            match ev {
                Ev::Listed { generation, path, result } => {
                    for role in [Role::Source, Role::Dest] {
                        for tab in self.pane_mut(role).tabs.iter_mut() {
                            if tab.cwd == path {
                                tab.accept_listing(generation, result.clone());
                            }
                        }
                    }
                }
                Ev::DirSize { path, bytes, files } => {
                    self.sizes.set(path, bytes, files);
                }
                Ev::Progress {
                    job,
                    done_bytes,
                    total_bytes,
                    done_items,
                    total_items,
                    label,
                } => {
                    if let Some(a) = self.active_job.as_mut() {
                        if a.id == job {
                            a.done_bytes = done_bytes;
                            a.total_bytes = total_bytes;
                            a.done_items = done_items;
                            a.total_items = total_items;
                            if !label.is_empty() {
                                a.label = label;
                            }
                        }
                    }
                }
                Ev::Finished {
                    job,
                    touched,
                    errors,
                    cancelled,
                } => {
                    self.jobs.forget(job);
                    if self.active_job.as_ref().is_some_and(|a| a.id == job) {
                        self.active_job = None;
                    }
                    self.status = if cancelled {
                        "Cancelled".to_owned()
                    } else if errors.is_empty() {
                        "Done".to_owned()
                    } else {
                        format!("Finished with {} error(s)", errors.len())
                    };
                    self.errors.extend(errors);
                    for dir in touched {
                        // A transfer changes sizes up the whole chain, so the
                        // cached values above it are now wrong too.
                        self.sizes.invalidate_subtree(&dir);
                        for anc in dir.ancestors() {
                            self.sizes.invalidate_subtree(anc);
                            break;
                        }
                        self.reload_showing(&dir);
                    }
                }
            }
        }
    }

    // ---- keyboard ---------------------------------------------------------

    fn handle_keys(&mut self, ui: &egui::Ui) {
        // An open rename editor or modal owns the keyboard.
        let renaming = self.pane(self.focused).active_tab().renaming.is_some();
        if renaming || self.pending.is_some() {
            return;
        }
        if ui.memory(|m| m.focused().is_some()) {
            return; // a text field (filter box) has focus
        }

        let actions = ui.input(|i| self.keymap.actions(i));
        for action in actions {
            self.perform(action);
        }
    }

    fn perform(&mut self, action: Action) {
        let role = self.focused;
        match action {
            Action::Commit => self.commit(Kind::Copy),
            Action::Preview => {
                if let Some(e) = self.pane(role).active_tab().cursor_entry() {
                    if e.is_dir {
                        // Space on a folder has no sample to audition; treat it
                        // as "tell me how big this is" instead of doing nothing.
                        let p = e.path.clone();
                        self.request_sizes(vec![p]);
                    } else {
                        let p = e.path.clone();
                        self.preview.toggle(&p);
                    }
                }
            }
            Action::Rename => {
                let tab = self.pane_mut(role).active_tab_mut();
                if let Some(e) = tab.cursor_entry() {
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
            Action::GoUp => {
                if let Some(p) = self.pane(role).active_tab().parent() {
                    self.open_in(role, p);
                }
            }
            Action::ComputeSize => {
                let sel = self.pane(role).active_tab().effective_selection();
                self.request_sizes(sel);
            }
            Action::Trash => {
                let sel = self.pane(role).active_tab().effective_selection();
                if !sel.is_empty() {
                    self.jobs.trash(sel);
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
                let cwd = self.pane(role).active_tab().cwd.clone();
                let id = self.new_tab_id();
                self.pane_mut(role).add_tab(DirTab::new(id, cwd));
                let i = self.pane(role).active;
                self.reload(role, i);
            }
            Action::CloseTab => {
                let i = self.pane(role).active;
                self.pane_mut(role).close_tab(i);
            }
            Action::NextTab => self.pane_mut(role).cycle(1),
            Action::PrevTab => self.pane_mut(role).cycle(-1),
            Action::SwitchPane => self.focused = self.focused.other(),
            Action::Cancel => {
                self.preview.stop();
                if let Some(a) = &self.active_job {
                    self.jobs.cancel(a.id);
                }
                self.pane_mut(role).active_tab_mut().renaming = None;
            }
        }
    }

    fn open_in(&mut self, role: Role, path: PathBuf) {
        self.pane_mut(role).active_tab_mut().navigate_to(path);
        let i = self.pane(role).active;
        self.reload(role, i);
    }

    fn do_rename(&mut self, from: &Path, to: &str) {
        let to = to.trim();
        if to.is_empty() || to.contains(std::path::MAIN_SEPARATOR) {
            self.errors.push(format!("invalid name: {to:?}"));
            return;
        }
        let Some(parent) = from.parent() else { return };
        let target = parent.join(to);
        if target == from {
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
                self.reload_showing(parent);
            }
            Err(e) => self.errors.push(format!("rename failed: {e}")),
        }
    }
}

// ---- rendering ------------------------------------------------------------

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, state::KEY, &self.snapshot());
    }

    fn auto_save_interval(&self) -> std::time::Duration {
        state::SAVE_INTERVAL
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // eframe only autosaves on a frame. After a change, make sure one
        // happens once the interval has passed, even if the app goes idle.
        let snap = self.snapshot();
        if self.last_state.as_ref() != Some(&snap) {
            self.last_state = Some(snap);
            ctx.request_repaint_after(state::SAVE_INTERVAL + std::time::Duration::from_millis(100));
        }
        // Absorb worker results here rather than in `ui`, so they land even
        // while the window is hidden.
        let events = self.jobs.drain();
        if !events.is_empty() {
            self.absorb(events);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));

        egui::Panel::left("source_pane")
            .resizable(true)
            .default_size(ui.available_width() * 0.5)
            .show(ui, |ui| self.show_pane(ui, Role::Source));

        egui::CentralPanel::default().show(ui, |ui| self.show_pane(ui, Role::Dest));

        // Modal last, so it paints over both panes.
        if let Some(p) = &self.pending {
            if let Some(decision) = dialogs::conflicts(ui.ctx(), &p.plan) {
                let pending = self.pending.take().expect("checked above");
                match decision {
                    Decision::Proceed(policy) => self.launch(&pending.plan, policy),
                    Decision::Cancel => self.status = "Transfer cancelled".to_owned(),
                }
            }
        }

        self.handle_keys(ui);
    }
}

impl App {
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Sample Manager");
            ui.separator();

            let commit_key = self.keymap.describe(Action::Commit);
            if ui
                .button(format!("Commit to destination  {commit_key}"))
                .on_hover_text("Copy the source selection into the active destination tab")
                .clicked()
            {
                self.commit(Kind::Copy);
            }
            if ui.button("Move").clicked() {
                self.commit(Kind::Move);
            }

            ui.separator();
            if ui.checkbox(&mut self.show_hidden, "Hidden").changed() {
                self.reload_all();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!("keys: {}", self.keymap.name))
                        .weak()
                        .small(),
                );
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        if let Some(a) = &self.active_job {
            let frac = if a.total_bytes > 0 {
                a.done_bytes as f32 / a.total_bytes as f32
            } else if a.total_items > 0 {
                a.done_items as f32 / a.total_items as f32
            } else {
                0.0
            };
            let job_id = a.id;
            let text = format!(
                "{}/{} · {} / {} · {}",
                a.done_items + 1,
                a.total_items,
                fs_model::human_bytes(a.done_bytes),
                fs_model::human_bytes(a.total_bytes),
                a.label
            );
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(frac.clamp(0.0, 1.0))
                        .desired_width(240.0)
                        .text(text),
                );
                if ui.button("Cancel").clicked() {
                    self.jobs.cancel(job_id);
                }
            });
            return;
        }

        ui.horizontal(|ui| {
            let tab = self.pane(self.focused).active_tab();
            let n_sel = tab.selected.len();
            let sel_bytes: u64 = tab
                .entries
                .iter()
                .filter(|e| tab.selected.contains(&e.path))
                .map(|e| if e.is_dir { 0 } else { e.len })
                .sum();
            ui.label(format!(
                "{} · {} items · {n_sel} selected ({})",
                self.focused.label(),
                tab.entries.len(),
                fs_model::human_bytes(sel_bytes)
            ));

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
                        .on_hover_text("click to clear")
                        .clicked()
                    {
                        self.errors.clear();
                    }
                }
            });
        });
    }

    fn show_pane(&mut self, ui: &mut egui::Ui, role: Role) {
        let focused = self.focused == role;

        // Pane header: role, path, and the sub-tab strip.
        ui.horizontal(|ui| {
            let title = egui::RichText::new(role.label());
            let title = if focused { title.strong() } else { title.weak() };
            if ui.selectable_label(focused, title).clicked() {
                self.focused = role;
            }
            ui.separator();
            let cwd = self.pane(role).active_tab().cwd.display().to_string();
            ui.add(egui::Label::new(egui::RichText::new(cwd).weak().small()).truncate());
        });

        self.tab_strip(ui, role);
        ui.separator();

        let intents = {
            let sizes = &self.sizes;
            let preview = &self.preview;
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
                    pane_focused: focused,
                },
            )
        };

        for intent in intents {
            match intent {
                Intent::TookFocus => self.focused = role,
                Intent::Open(p) => {
                    if p.is_dir() {
                        self.open_in(role, p);
                    } else {
                        self.preview.toggle(&p);
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
                    self.pane_mut(role).active_tab_mut().renaming = None;
                    self.do_rename(&from, &to);
                }
                Intent::CancelRename => {
                    self.pane_mut(role).active_tab_mut().renaming = None;
                }
                Intent::WantSize(paths) => self.request_sizes(paths),
                Intent::Dropped(paths) => {
                    let dest = self.pane(role).active_tab().cwd.clone();
                    self.start_transfer(Kind::Copy, paths, dest);
                }
            }
        }
    }

    /// The sub-tab strip. Each tab header is also a drop target, so you can
    /// drag onto a folder you have open without first switching to it.
    fn tab_strip(&mut self, ui: &mut egui::Ui, role: Role) {
        let mut switch_to = None;
        let mut close = None;
        let mut new_tab = false;
        let mut drop_on: Option<(usize, Vec<PathBuf>)> = None;

        egui::ScrollArea::horizontal()
            .id_salt(("tabs", role.label()))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
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
                        if let Some(p) = payload {
                            drop_on = Some((i, p.paths.clone()));
                        }
                    }

                    if ui.small_button("+").on_hover_text("New tab").clicked() {
                        new_tab = true;
                    }
                    if self.pane(role).tabs.len() > 1
                        && ui.small_button("×").on_hover_text("Close tab").clicked()
                    {
                        close = Some(self.pane(role).active);
                    }
                });
            });

        if let Some(i) = switch_to {
            self.pane_mut(role).active = i;
            self.focused = role;
        }
        if let Some((i, paths)) = drop_on {
            if let Some(tab) = self.pane(role).tabs.get(i) {
                let dest = tab.cwd.clone();
                self.start_transfer(Kind::Copy, paths, dest);
            }
        }
        if new_tab {
            let cwd = self.pane(role).active_tab().cwd.clone();
            let id = self.new_tab_id();
            self.pane_mut(role).add_tab(DirTab::new(id, cwd));
            let i = self.pane(role).active;
            self.reload(role, i);
        }
        if let Some(i) = close {
            self.pane_mut(role).close_tab(i);
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

// Keep `SortKey` reachable from here for future column config.
const _: Option<SortKey> = None;
