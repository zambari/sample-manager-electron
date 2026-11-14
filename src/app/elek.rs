//! Device-view lifecycle, connection handling and file/pool column layout.
//! Requests, sample jobs, project files and worker answers live in the elek/ modules.

use super::*;
use crate::device::{ElekCmd, ElekEv};
use crate::ui::elek_view::{self, ElekIntent};
use crate::device::file;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;


mod commands;
mod events;
mod files;
mod readback;
mod samples;

type Key = (u32, u32);

/// Pool files being copied into the preview cache, one device job at a time
/// so a clicked slot (put at the front) never waits behind the whole pool.
#[derive(Default)]
pub(super) struct PoolFetch {
    queue: VecDeque<Key>,
    /// The download job on its way, and which file.
    running: Option<(u64, Key)>,
    /// Play (`false`) or open (`true`) this file once it lands.
    then: Option<(Key, bool)>,
    /// Background run: files done, and total asked for.
    done: usize,
    total: usize,
    /// Failed in a row: a moved file is skipped, but a second failure
    /// (likely unplugged) drops the queue.
    fails: usize,
    /// Uploads of files dropped onto the pool: job → `(slot as read, file)`.
    pub(super) uploads: std::collections::HashMap<u64, Vec<(u16, PathBuf)>>,
    /// Slots being looked up from an earlier upload's link: if the device
    /// no longer has that file, upload it again.
    prior: std::collections::HashMap<u16, PathBuf>,
    /// A bundle waiting for its samples to land in the preview cache.
    bundle: Option<BundleSave>,
    /// A loaded file's samples being placed into the pool.
    import: Option<Import>,
}

/// A project or pattern being saved with its samples.
pub(super) struct BundleSave {
    file: file::SaveFile,
    /// The bundle's folder, and the JSON's name in it.
    dir: PathBuf,
    json: String,
    /// "Project", or the pattern's slot label, for messages.
    what: String,
    /// Samples still being fetched from the device.
    waiting: HashSet<Key>,
}

/// Samples from a loaded file being placed: each is looked up on the device
/// by hash, else its bundled WAV is uploaded and looked up again.
#[derive(Default)]
pub(super) struct Import {
    /// The load's own message, summed up with how placing went.
    what: String,
    /// By slot as read.
    slots: HashMap<u16, ImportSlot>,
    found: usize,
    uploaded: usize,
    missing: Vec<String>,
    failed: Vec<String>,
}

struct ImportSlot {
    name: String,
    /// The file's key in the file.
    key: Key,
    /// Its bundled WAV.
    file: Option<PathBuf>,
    /// Where on the +Drive an upload goes.
    dir: PathBuf,
    stage: Stage,
    /// The slot already points at the file (a loaded project): only check.
    verify: bool,
}

#[derive(PartialEq)]
enum Stage {
    /// Looking up the file's own hash.
    Hash,
    /// Looking up the hash an earlier upload of the bundled WAV got.
    Link,
    /// Uploaded, looking up what landed.
    Upload,
}

impl App {
    /// A device page in the centre. Samples is the pool itself, so the
    /// right column shows the files meanwhile ([`Self::pool_in_col`]);
    /// Patterns keeps the pool beside it.
    pub(super) fn enter_full_elek(&mut self) {
        self.pool_focused = false;
        self.enter_elek();
    }

    pub(super) fn enter_elek(&mut self) {
        // Leaving the editor: don't keep its loop running behind the device view.
        if !self.elek_mode {
            self.engine.stop();
            self.editor.play_when_loaded = false;
        }
        self.elek_mode = true;
        self.editor_focused = false;
        self.measure_cache();
        if !self.elek.asked_projects {
            self.elek.asked_projects = true;
            self.device.elek(ElekCmd::ListProjects);
        }
        self.greet_device();
    }

    /// First visit to the device view this run: read the project in the
    /// device's memory without waiting for a click, and once its sample
    /// names are in, show the docked layout (see [`Self::first_dock_now`]).
    fn greet_device(&mut self) {
        if self.elek_greeted {
            return;
        }
        let present = matches!(crate::device::presence(), crate::device::Presence::One(_));
        if !present {
            return;
        }
        self.elek_greeted = true;
        if self.elek.project.is_some() || self.elek.busy.is_some() || self.elek.reading.is_some() {
            return;
        }
        self.first_dock = true;
        self.elek_read(Vec::new());
    }

    /// The device is back after being unplugged or switched off. Nothing
    /// read before can be trusted: it may have been edited on its own, had
    /// another project loaded, or be another machine altogether (two units
    /// of one model look the same on the port list). So forget what came
    /// from its +Drive and read its project again. Unpushed edits here are
    /// not dropped without asking; a project opened from a file is kept
    /// (it is meant to be pushed, and the push checks for staleness).
    /// `other`: a different unit by port name (Digitakt → Digitakt II):
    /// what was read from the old one, sound pool included, goes at once,
    /// edits too (they were made against the other machine's project).
    pub(super) fn device_reconnected(&mut self, other: bool) {
        let st = &mut self.elek;
        // Paths and listings belong to whichever machine answered last.
        st.names.clear();
        st.presets.device_reconnected(other);
        if st.project.as_ref().is_some_and(|p| p.from_file.is_some()) {
            st.status = "Device reconnected; the project opened from a file is kept (not read from the device)".into();
            return;
        }
        if other && st.project.is_some() {
            let edits = st.changed() + st.filling.len();
            crate::log::line(format!("elek: another unit connected; dropping the old project ({edits} unpushed edit(s))"));
            st.close_project();
            st.reading = None;
            st.busy = None;
            st.expect = None;
            self.reconnect_ask = None;
            self.elek.status = if edits > 0 {
                format!("Another device connected: {edits} unpushed edit(s) made for the previous one were dropped; reading this one's project")
            } else {
                "Another device connected: reading its project".into()
            };
            self.elek_read(Vec::new());
            return;
        }
        if st.project.is_none() && !self.elek_greeted {
            // Never read: the first visit to the device view reads it.
            return;
        }
        let edits = st.changed() + st.filling.len();
        crate::log::line(format!("elek: device reconnected, {edits} unpushed edit(s); reading the project again"));
        if edits > 0 {
            self.reconnect_ask = Some(edits);
            return;
        }
        self.reread_after_reconnect();
    }

    /// The reconnect question: `true` drops the edits and reads.
    pub(super) fn answer_reconnect(&mut self, read: bool) {
        if read {
            self.reread_after_reconnect();
        } else {
            self.elek.status = "Kept your unpushed edits; the project shown may not match the device. \
                A push checks first and holds back if the device's project changed."
                .into();
        }
    }

    fn reread_after_reconnect(&mut self) {
        let st = &mut self.elek;
        if let Some(ctl) = st.reading.take() {
            ctl.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        st.busy = None;
        st.expect = None;
        st.expect_patterns = None;
        st.expect_sounds = None;
        st.status = "Device reconnected: reading its project again".into();
        self.elek_read(Vec::new());
    }

    /// The first read has its sample names: Patterns in the centre (if
    /// the user is still on a device page), the pool docked on the right.
    fn first_dock_now(&mut self) {
        self.first_dock = false;
        self.dock_pool = true;
        self.pool_focused = false;
        // The docked pool lives in the right column, which this hides.
        self.elek.hide_files = false;
        self.status = "Project loaded: the sample pool is docked on the right. \
            Switch the right column with Files / Sample pool / +Drive."
            .into();
    }

    /// The right column shows the pool: SAMPLE POOL is picked, and the
    /// centre is not the Samples page (which already is the pool).
    pub(super) fn pool_in_col(&self) -> bool {
        self.dock_pool && self.pool_offered()
    }

    /// SAMPLE POOL is in the right column's switch: the device was talked
    /// to this run (or a project is loaded), and the centre is not the
    /// Samples page, which already is the pool.
    fn pool_offered(&self) -> bool {
        (self.elek_greeted || self.elek.project.is_some())
            && !(self.elek_mode && self.elek.page == elek_view::ElekPage::Order)
    }

    /// The right column shows the +Drive (the Destination pane on its
    /// device tab).
    pub(super) fn drive_in_col(&self) -> bool {
        !self.pool_in_col() && !(self.elek_mode && self.elek.hide_files) && self.pane(Role::Dest).active_tab().device
    }

    /// The DEVICE switch: off closes every device panel and the MIDI ports
    /// (so a DAW can have the device) and stops watching for it; on
    /// connects again.
    pub(super) fn toggle_device(&mut self) {
        let off = !crate::device::switched_off();
        if off {
            self.fold_device();
        }
        self.device.switch_off(off);
        self.status = if off {
            "Device switched off: MIDI ports closed. Click DEVICE to connect again.".into()
        } else {
            "Device switched on".into()
        };
    }

    /// Close every device panel: the device view, the docked pool, and
    /// +Drive tabs in both panes. The project stays in memory.
    pub(super) fn fold_device(&mut self) {
        self.elek_mode = false;
        self.dock_pool = false;
        self.pool_focused = false;
        self.leave_device_tabs();
    }

    /// Move both panes off their +Drive tabs (opening a home tab where a
    /// pane has no local one).
    pub(super) fn leave_device_tabs(&mut self) {
        for role in [Role::Source, Role::Dest] {
            if !self.pane_mut(role).leave_device() {
                let id = self.new_tab_id();
                self.pane_mut(role).add_tab(crate::tab::DirTab::new(id, crate::app::dirs_home()));
                let i = self.pane(role).active;
                self.reload(role, i);
            }
        }
    }

    /// Back to the editor with the pool in the right column.
    /// (The Device button opens the center view.)
    pub(super) fn dock_elek(&mut self) {
        self.elek_mode = false;
        self.dock_pool = true;
        self.pool_focused = true;
        self.editor_focused = false;
        self.measure_cache();
        if !self.elek.asked_projects {
            self.elek.asked_projects = true;
            self.device.elek(ElekCmd::ListProjects);
        }
    }

    pub(super) fn show_elek(&mut self, ui: &mut egui::Ui) {
        self.elek.docked = false;
        // The pool docked beside the Patterns page takes the keys once clicked.
        self.elek.keys_on = !(self.pool_in_col() && self.pool_focused);
        let intents = match self.elek.page {
            elek_view::ElekPage::Order => elek_view::show(ui, &mut self.elek),
            elek_view::ElekPage::Project => crate::ui::elek_project_view::show(ui, &mut self.elek),
            elek_view::ElekPage::Presets => crate::ui::elek_presets_view::show(ui, &mut self.elek),
            elek_view::ElekPage::Library => crate::ui::elek_library_view::show(ui, &mut self.elek),
        };
        self.elek_intents(intents);
    }

    /// The sample pool is on screen: the Samples page, or docked.
    fn pool_shown(&self) -> bool {
        self.elek.project.is_some()
            && ((self.elek_mode && self.elek.page == elek_view::ElekPage::Order)
                || self.pool_in_col())
    }

    /// While the pool is on screen, point from one side to the other
    /// without selecting anything: a local file under a pane's cursor pings
    /// the slot holding its device copy, and a slot under the pool's cursor
    /// pings its local file in the source pane (or, when that file is
    /// elsewhere, the folder row leading to it, else the ".." row).
    /// An edited original still counts: it pings the older version.
    pub(super) fn cross_ping(&mut self) {
        let shown = self.pool_shown();
        for (i, role) in [Role::Source, Role::Dest].into_iter().enumerate() {
            let tab = self.pane(role).active_tab();
            let cur = (!tab.device)
                .then(|| tab.cursor.and_then(|c| tab.entries.get(c)))
                .flatten()
                .filter(|e| !e.is_dir)
                .map(|e| e.path.clone());
            if cur == self.seen_file[i] {
                continue;
            }
            self.seen_file[i] = cur.clone();
            if let (true, Some(p)) = (shown, cur) {
                if let Some(l) = self.links.for_local(&p).cloned() {
                    self.elek.ping_key(|k| l.is(k));
                }
            }
        }

        let key = shown.then(|| self.elek.cursor_key()).flatten();
        if key == self.seen_slot {
            return;
        }
        self.seen_slot = key;
        let Some(k) = key else { return };
        let links = &self.links;
        let tab = self.pane(Role::Source).active_tab();
        if tab.device {
            return;
        }
        let here = tab
            .entries
            .iter()
            .find(|e| !e.is_dir && links.for_local(&e.path).is_some_and(|l| l.is(k)))
            .map(|e| e.path.clone());
        let target = match here {
            Some(p) => Some(Some(p)),
            None => {
                let elsewhere = links
                    .user_copy(k.0, k.1)
                    .or_else(|| links.original(k.0, k.1).filter(|l| l.local.exists()))
                    .map(|l| l.local.clone());
                elsewhere.and_then(|local| {
                    // Inside this folder: the deepest listed folder holding it.
                    let dir = tab
                        .entries
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| e.is_dir && local.starts_with(&e.path))
                        .max_by_key(|(j, _)| tab.depths.get(*j).copied().unwrap_or(0))
                        .map(|(_, e)| e.path.clone());
                    match dir {
                        Some(d) => Some(Some(d)),
                        None if !local.starts_with(&tab.cwd) && tab.parent().is_some() => Some(None),
                        None => None,
                    }
                })
            }
        };
        if let Some(t) = target {
            self.pane_mut(Role::Source).active_tab_mut().ping(t);
        }
    }

    /// The pool as a pane in the right column.
    pub(super) fn show_elek_docked(&mut self, ui: &mut egui::Ui) {
        self.elek.docked = true;
        self.elek.keys_on = self.pool_focused;
        let intents = elek_view::show_docked(ui, &mut self.elek);
        self.elek_intents(intents);
    }

    /// The right column: a FILES / SAMPLE POOL / +DRIVE switch above the
    /// Destination pane or the pool. FILES and +DRIVE are the same pane:
    /// +DRIVE is its tab on the device, FILES its local tabs.
    pub(super) fn show_dest_col(&mut self, ui: &mut egui::Ui) {
        let pool = self.pool_in_col();
        let drive = self.drive_in_col();
        let pane_lit = self.focused == Role::Dest && !self.editor_focused && !self.pool_focused;
        let present = crate::device::presence().available();
        let pool_offered = present && self.pool_offered();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            if ui
                .add(theme::column_switch("Files", !pool && !drive, !pool && !drive && pane_lit, false))
                .on_hover_text("Folders on this computer")
                .clicked()
            {
                self.show_files();
            }
            if pool_offered
                && ui
                    .add(theme::column_switch("Sample pool", pool, pool && self.pool_focused, true))
                    .on_hover_text("The sample slots of the project loaded on the device")
                    .clicked()
            {
                self.dock_pool = true;
                self.pool_focused = true;
                self.editor_focused = false;
            }
            if present
                && ui
                    .add(theme::column_switch("+Drive", drive, drive && pane_lit, true))
                    .on_hover_text("The device's +Drive storage. Commit and drops land here while it shows.")
                    .clicked()
            {
                self.show_drive();
            }
        });
        if pool {
            let r = ui.max_rect();
            self.show_elek_docked(ui);
            self.pool_rect = Some(r);
        } else {
            self.pool_rect = None;
            self.show_pane(ui, Role::Dest);
        }
    }

    /// The right column on the Destination pane's local tabs (opening one
    /// if it has none).
    pub(super) fn show_files(&mut self) {
        self.dock_pool = false;
        self.pool_focused = false;
        self.focused = Role::Dest;
        self.editor_focused = false;
        if self.pane_mut(Role::Dest).leave_device() {
            return;
        }
        let src = self.pane(Role::Source);
        let cwd = src
            .tabs
            .iter()
            .find(|t| !t.device)
            .map_or_else(crate::app::dirs_home, |t| t.cwd.clone());
        let id = self.new_tab_id();
        self.pane_mut(Role::Dest).add_tab(crate::tab::DirTab::new(id, cwd));
        let i = self.pane(Role::Dest).active;
        self.reload(Role::Dest, i);
    }

    /// The right column on the +Drive: the Destination pane's device tab,
    /// opened at the root the first time. Also brings the file panes back
    /// if the device view had hidden them.
    pub(super) fn show_drive(&mut self) {
        self.engine.stop();
        self.editor.play_when_loaded = false;
        self.dock_pool = false;
        self.pool_focused = false;
        self.focused = Role::Dest;
        self.editor_focused = false;
        if self.elek_mode {
            self.elek.hide_files = false;
        }
        let pane = self.pane_mut(Role::Dest);
        if pane.active_tab().device {
            return;
        }
        if let Some(i) = pane.tabs.iter().position(|t| t.device) {
            pane.active = i;
            return;
        }
        let id = self.new_tab_id();
        self.pane_mut(Role::Dest).add_tab(crate::tab::DirTab::new_device(id, PathBuf::from("/")));
        let i = self.pane(Role::Dest).active;
        self.reload(Role::Dest, i);
    }
}
