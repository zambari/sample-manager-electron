//! Application construction, session restoration and persistence snapshots.

use super::*;

impl App {
    pub fn new(ctx: egui::Context, storage: Option<&dyn eframe::Storage>) -> Self {
        theme::apply(&ctx);
        theme::install_fonts(&ctx);
        let home = dirs_home();
        let saved = state::load(storage);
        let hide_files = saved.as_ref().is_some_and(|s| s.device_hides_files);
        let open_banks = saved.as_ref().map_or([true, false, false, false, false, false, false, false], |s| s.device_open_banks);
        let mut next_tab_id = 0;
        let mut mk = |p: PathBuf, device: bool| {
            next_tab_id += 1;
            if device {
                DirTab::new_device(next_tab_id, p)
            } else {
                DirTab::new(next_tab_id, p)
            }
        };
        let mut restore = |ps: Option<&state::PaneState>| -> Pane {
            let mut tabs: Vec<DirTab> = ps
                .map(|ps| ps.tabs.as_slice())
                .unwrap_or_default()
                .iter()
                .filter_map(|t| {
                    let mut tab = if t.device {
                        mk(t.cwd.clone(), true)
                    } else {
                        mk(state::existing_dir(&t.cwd)?, false)
                    };
                    tab.sort_key = t.sort_key;
                    tab.sort_asc = t.sort_asc;
                    tab.tree = t.tree;
                    tab.show_all = t.show_all;
                    tab.shorten = t.shorten;
                    Some(tab)
                })
                .collect();
            if tabs.is_empty() {
                tabs.push(mk(home.clone(), false));
            }
            let active = ps.map_or(0, |ps| ps.active).min(tabs.len() - 1);
            Pane {
                tabs,
                active,
                last_local: None,
            }
        };
        let source = restore(saved.as_ref().map(|s| &s.source));
        let dest = restore(saved.as_ref().map(|s| &s.dest));

        let output_device = saved.as_ref().and_then(|s| s.output_device.clone());
        let engine = Engine::open(output_device.as_deref());
        let mut prefs = saved.as_ref().map(|s| s.editor.clone()).unwrap_or_default();
        // Starts on the browsed sample.
        prefs.load_mono(false);
        let mut editor = Editor::new(ctx.clone(), prefs);
        let session = saved
            .as_ref()
            .and_then(|s| s.session.clone())
            .filter(|s| s.path.is_file());
        if let Some(s) = &session {
            editor.restore_session(s);
        }
        let mut other = Editor::new(ctx.clone(), editor.prefs.clone());
        // The hidden slot starts out as the kit; `close` keeps the flag.
        other.is_kit = true;
        if let Some(s) = saved
            .as_ref()
            .and_then(|s| s.kit_session.clone())
            .filter(|s| s.path.is_file())
        {
            other.restore_session(&s);
        }
        let parked = saved
            .as_ref()
            .map(|s| s.parked_kits.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.path.is_file() && other.loading.as_deref() != Some(s.path.as_path()))
            .map(|s| {
                let mut k = Editor::new(ctx.clone(), editor.prefs.clone());
                k.is_kit = true;
                k.restore_session(&s);
                k
            })
            .collect();
        let mut errors = Vec::new();
        if let Some(e) = &engine.open_error {
            errors.push(format!("audio output: {e}"));
        }

        crate::device::watch_presence(ctx.clone());
        let jobs = Jobs::new(ctx);
        let device = Device::new(jobs.emitter());
        let mut app = Self {
            os_dnd: Default::default(),
            device,
            device_status: None,
            last_presence: Default::default(),
            last_unit: None,
            device_last: None,
            staged_upload: None,
            pool_edit: None,
            slot_save: None,
            save_confirm: None,
            engine,
            editor,
            other,
            showing_kit: false,
            parked,
            browsed_kit: None,
            kit_pending: None,
            kit_seed: None,
            kit_add: None,
            kit_grid: saved
                .as_ref()
                .map_or(Some(crate::editor::AUTO_GRID), |s| s.kit_grid),
            kit_naming: None,
            editor_focused: false,
            devices: Engine::devices(),
            output_device,
            favourites: saved
                .as_ref()
                .map(|s| s.favourites.clone())
                .unwrap_or_default(),
            opened: saved
                .as_ref()
                .map(|s| s.opened.iter().cloned().collect())
                .unwrap_or_default(),
            opened_dirty: false,
            committed: saved
                .as_ref()
                .map(|s| s.committed.iter().cloned().collect())
                .unwrap_or_default(),
            committed_dirty: false,
            voice_path: None,
            voice_sample: None,
            voice_once: false,
            once_for: None,
            last_cursor: None,
            hold_cursor_load: session.is_some(),
            last_stream_error: None,
            side_widths: [0.0; 2],
            volume: 1.0,
            pitch: 0.0,
            pitch_path: None,
            meter: [0.0; 2],
            meter_hold: [0.0; 2],
            window_focused: true,
            factory_popup: false,
            factory_warned: false,
            device_load_pending: None,
            device_load: None,
            leave_pending: None,
            leave_after_save: None,
            close_kit_now: false,
            close_queue: None,
            save_scope_pending: false,
            export_naming: None,
            saving: Default::default(),
            allow_close: false,
            close_when_idle: false,
            build_watch: relaunch::BuildWatch::new(),
            links: crate::links::Links::load(),
            prints: crate::prints::Prints::load(),
            print_run: Default::default(),
            device_origin: None,
            send_back: None,
            relaunch: false,
            elek_mode: false,
            elek: Default::default(),
            seen_file: [None, None],
            seen_slot: None,
            changed_paths: Default::default(),
            changed_at: None,
            update_push: Default::default(),
            dock_pool: false,
            pool_snapped: false,
            pool_focused: false,
            pool_rect: None,
            elek_greeted: false,
            first_dock: false,
            reconnect_ask: None,
            pool_fetch: Default::default(),
            jobs,
            keymap: Keymap::platform_default(),
            sizes: SizeCache::default(),
            preview: Preview::default(),
            source,
            dest,
            focused: saved.as_ref().map_or(Role::Source, |s| s.focused),
            next_tab_id,
            pending: None,
            delete_pending: None,
            clipboard: None,
            undo: Vec::new(),
            undo_jobs: Default::default(),
            rename_after: None,
            pool_rename: None,
            relink_after: None,
            local_rename_pending: None,
            slot_download_pending: None,
            slot_prefix_pending: None,
            reveal_after: None,
            export_rename: None,
            finish_note: None,
            device_queued: Default::default(),
            path_edit: None,
            volumes: None,
            dupes_pending: None,
            active_jobs: Vec::new(),
            batch: Batch::default(),
            status: String::new(),
            errors,
            errors_logged: 0,
            last_state: None,
        };
        app.elek.hide_files = hide_files;
        app.elek.proj.banks = open_banks;
        // The active kit's file went missing: bring in a parked one.
        if !app.has_kit() && !app.parked.is_empty() {
            app.unpark_first();
        }
        app.reload_all();
        app
    }

    /// `with_opened` is false for the per-frame change check, which tracks
    /// `opened` through `opened_dirty` instead.
    pub(super) fn snapshot(&self, with_opened: bool) -> state::AppState {
        let pane = |p: &Pane| state::PaneState {
            tabs: p
                .tabs
                .iter()
                .map(|t| state::TabState {
                    cwd: t.cwd.clone(),
                    sort_key: t.sort_key,
                    sort_asc: t.sort_asc,
                    tree: t.tree,
                    show_all: t.show_all,
                    shorten: t.shorten,
                    device: t.device,
                })
                .collect(),
            active: p.active,
        };
        state::AppState {
            source: pane(&self.source),
            dest: pane(&self.dest),
            focused: self.focused,
            editor: {
                let mut p = self.editor.prefs.clone();
                p.stash_mono(self.showing_kit);
                p
            },
            output_device: self.output_device.clone(),
            favourites: self.favourites.clone(),
            opened: if with_opened {
                let mut v: Vec<PathBuf> = self.opened.iter().cloned().collect();
                v.sort();
                v
            } else {
                Vec::new()
            },
            committed: if with_opened {
                let mut v: Vec<PathBuf> = self.committed.iter().cloned().collect();
                v.sort();
                v
            } else {
                Vec::new()
            },
            session: self.browser().session(),
            kit_session: self.kit().session(),
            parked_kits: self.parked.iter().filter_map(Editor::session).collect(),
            kit_grid: self.kit_grid,
            device_hides_files: self.elek.hide_files,
            device_open_banks: self.elek.proj.banks,
        }
    }
}
