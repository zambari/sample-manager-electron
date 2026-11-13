# Map

Where to start reading when a concept comes up. Names are files and
functions, not line numbers; grep the function name. Every file has a `//!`
header that says what it owns, so read that first.

User-facing behaviour is described in [README.md](README.md). Device format
research lives in `project_discovery.md` and `project_management_discovery.md`
(local only, gitignored).

## Architecture in five lines

- `src/app/mod.rs`: the `App` struct, `logic()` (absorbs worker results every
  frame, even when hidden) and `ui()` (layout, then every modal dialog).
  `App`'s methods are split across `src/app/*.rs` by topic.
- `src/ui/*`: drawing and mouse handling only. Views take state, return intents.
- `src/editor/*`, `src/audio/*`: the sample editor's model and DSP. No egui in `audio`.
- `src/device/*`: one worker thread owning the MIDI link.
- Two rules: `ui()` never does I/O (post a request, draw from cached state),
  and everything a worker produces comes back as a `jobs::Ev`, handled in
  `App::absorb` (`src/app/transfers.rs`).


### Top toolbar: `App::toolbar` in `src/app/bars.rs`

| Control | Where |
|---|---|
| "SAMPLE MANAGER" title, **New build: relaunch** | `App::toolbar`; watcher in `src/app/relaunch.rs` |
| Audio output picker, ⟳ rescan | `App::device_picker` |
| Volume slider, L/R meter, phase strip | `App::volume_and_meter`, `App::phase_strip` |
| Pitch fader | `App::pitch_fader` |
| **Commit to destination**, **Move**, `keys:` profile label | `App::toolbar` |

### Bottom status bar: `App::status_bar` in `src/app/bars.rs`

Progress bar for the whole queue, per-file bar, **Cancel** / **Cancel all**,
status text, latest error, ETA (`Batch::started_at`, average byte rate). Data: `ActiveJob` and `Batch` in `src/app/mod.rs`.

### File pane (Source / Destination): `App::show_pane` in `src/app/pane_view.rs`

| Control | Where |
|---|---|
| Pane title, path label / typed path bar | `show_pane`; `App::path_editor` in `src/app/nav.rs` |
| ★ favourites menu, `💾 ⏷` drives menu | `App::favourites_menu`, `App::drives_menu` in `src/app/nav.rs` |
| Device connection badge (● name / offline) | `App::device_badge` |
| Tab strip, `+` new tab, 🎹 open +Drive tab (Source only), × close, tab right-click menu, drops on tabs. The Destination strip lists local tabs only and is hidden while +DRIVE shows | `App::tab_strip` |
| Sort headers, filter box, **🌲 Tree**, **All files**, **Shorten** | `header` in `src/ui/file_table.rs` |
| Rows, check marks, speaker icon, inline rename | `row`, `draw_cells`, `draw_device_cells`, `rename_editor` in `src/ui/file_table.rs` |
| Row right-click menu (Open, Load as kit, Cut/Copy, Make mono copy, Remove duplicates…, Show on device…) | `row` in `src/ui/file_table.rs`; handled where `show_pane` matches on `Intent` |
| Paste / New folder / reveal buttons | `paste_button`, `new_folder_button`, `reveal_button` in `src/ui/file_table.rs` |
| After a single +Drive rename (inline or pool slot): offer to rename the linked local copy too | `relink_after` set in `App::do_rename` / `ElekIntent::RenameFile`; `App::offer_local_rename`, `App::rename_local_copy` in `src/app/actions.rs`; dialog `dialogs::rename_local` |
| Multi-rename panel | `src/ui/rename_view.rs`; logic `src/rename.rs` |
| Footer counts | `App::pane_footer_text` |

### Sample editor (centre): `editor_view::show` in `src/ui/editor_view/mod.rs`

Drawn top to bottom; intents handled in `App::show_editor` (`src/app/editing.rs`).

| Control | Where (`src/ui/editor_view/`) |
|---|---|
| "Sample Editor" title, **Device** switch, Samples / Patterns / +Drive tabs | `toolbar.rs` `header`; switch widget is `theme::device_switch`; tabs `elek_view::device_pages` (`DevicePick`). One lit at a time: +Drive from the top goes back to the editor (`ElekIntent::ShowDrive`). Tabs fold away while the device is unplugged or switched off |
| **Device** switch click: switch the device off (close device view, docked pool, +Drive tabs; close MIDI ports; stop the presence scan, so a DAW can use it) or back on. Unplugging folds the same panels but keeps scanning. The switch is white while a device is connected; on (re)connect the last device subpage (`App::device_last`, a `DevicePick`) opens again | `App::toggle_device`, `App::fold_device` in `src/app/elek.rs` (`EdIntent::ToggleDevice`, `ElekIntent::ToggleDevice`); `Presence::Off`, `device::switched_off`, `presence::set_off`; worker side `Cmd::Release`, `Worker::release`, refusal in `Worker::with_conn`; unplug fold in `App::track_presence` |
| **Show** row of panel toggles | `toolbar.rs` `panel_toggles`; flags in `editor::Panels` |
| Sample / Kit tabs (file-pane tab style, `theme::pane_tab`), kit switcher ▾, ×, **Close all kits** | `toolbar.rs` `slot_bar`, `slot_tabs`, `kit_tab`, `kit_switcher` |
| **Kit** section (toggle in Show, `Panels::kit`): accent → (K) button, fill boxes (muted grey when full), grid picker, **Start New Grid Kit** (alone and accent-filled when no kit is open) | `toolbar.rs` `kit_section`, `add_slice_button`, `fill_boxes`, `grid_picker` |
| **Transport** (play, Loop, Follow, Snap to 0, Auto-load, Auto-play; toggles are `theme::soft_toggle`), **Gain** (slider, Normalize, Reset), **Histogram** (`level_strip.rs` `histogram_panel`: level bars, tiny dropdowns `theme::tiny_pick`, nerd stats box; cached data in `src/editor/level_hist.rs`), **Tools** (undo/redo, cut/copy/paste, +100 ms, Crop, loop halve/double) | `toolbar.rs` `toolbar` |
| Stereo / Mix / L / R | `toolbar.rs` `mono_picker` |
| **File** row: **Save over …** / **⬆ Save → slot …** (names its target), **Export → …** (names a +Drive folder or the pool slot; `Slots::export_drive`), Export and close (kits, the accent one there), **Save to ⏷** menu (kits too) (over the original, pool slot: selected / first empty, new +Drive file) | `toolbar.rs` `file_row`, `save_to_menu`; overwrites confirmed by `dialogs::confirm_overwrite` via `App::save_confirm` (`SaveConfirm`, `App::confirmed_save` in `src/app/pool_edit.rs`) |
| Which file is open (Original / Device copy / changed since upload / Same audio as on device, with the original's path), click to show in source pane; then the pool slot it is on the device as (`pool A012`, amber `not identical`, `? (same name)`), click to flash it | `toolbar.rs` `origin_line`; worked out by `App::editor_origin` (slot via `App::pool_match`), shown by `App::show_in_source` / `App::origin_in_pool` in `src/app/editing.rs` |
| **⬆ Save → slot …**: the Save button when the sample was opened from a pool slot | `file_row` (`Slots::pool_slot`); logic in `src/app/pool_edit.rs` |
| **Save to ⏷ → pool slot**: the one selected slot (empty, or replace after a question) or the first empty one; fills it and pushes | `save_to_menu` (`Slots::pool`, `PoolTargets`, `SlotPick`); `ElekState::lone_sel`, `first_free`; logic `App::save_to_slot`, `save_into_slot`, `SlotSave` in `src/app/pool_edit.rs` |
| Kit save safety: a kit never saves over (or renames) a file its slices came from | `Editor::note_source`, `path_is_source`; kit-file test `kit::is_collection` (our `slnm` chunk via `cues::is_ours`, or `[N]` name with cues) |
| Kit slice right-click: **Reveal in source pane** / **Reveal in sample pool** (faint wash on the slice while open). Slices keep their source's full path (`Editor::names`, saved in the `slnm` chunk by `cues::append_names`; older kits have bare names; a slice with no usable name, e.g. in a kit fetched from the device, is looked up by its audio among fingerprinted files); the pool slot is found by a link's +Drive hash, else by audio fingerprint, else by file name (then marked not identical / not compared) | menu `slice_menu` in `waveform.rs` (`Editor::slice_menu`, `SliceHome`); resolved by `App::slice_homes`, `App::slice_in_pool` in `src/app/editing.rs`; `App::slice_local`, `App::pool_match` in `src/app/prints.rs`; slice prints `prints::range_keys`, `Editor::slice_bounds`; `ElekState::slot_holding` |
| **Trim** section (kits) | `trim.rs` `trim_section`, `strip.rs` |
| **Wave**: lanes, ruler, in/out and slice markers, fade handles, all mouse handling | `waveform.rs` |
| Marker **padlock** (top right of the ruler, SliceKits with markers): locked = markers fixed, a grid keeps its slots (cuts padded back); unlocked = no grid (audio untouched), cuts close up, markers draggable by ruler tab or line (zero-snapped with Snap to 0); grid picker greyed while unlocked | `padlock`, `lock_rect` in `waveform.rs`; `Editor::markers_locked`, `set_markers_locked` (`grid.rs`), `move_marker`, `marker_index`, `markers_moved` (`clip.rs`), `Drag::Marker`; `Slots::kit_unlocked` |
| Grid padding overlay: each slot's silence after its sound, solid dark | `waveform.rs` (after `draw_lanes`), data `Editor::slot_sounds`; colour `PAD_COLOR` |
| **Nums** | `toolbar.rs` `positions` |
| **Scalpel** | `scope.rs` |
| **Map** | `overview.rs` |
| **Stats**: levels, loudness, phase meter, waterfall | `stats_view.rs`; analysis in `src/editor/analysis.rs`, `src/audio/loudness.rs`, `spectrum.rs`, `stats.rs` |
| **Help** text | `keymap::EDITOR_HELP` |

### Device view

Intents handled in `App::elek_intents` (`src/app/elek.rs`).

| Control | Where |
|---|---|
| Mode header: Sample Editor / Device (Samples / Patterns / +Drive), **⇥ Dock beside files** (accent-filled, Samples page only), **Hide file panes** | `mode_header`, `device_pages` in `src/ui/elek_view.rs` |
| Device bar: **⬇ Load current project**, **⬆ Push to device**, **💾 File** menu (Save / Load project…), **+Drive projects** menu, status line | `device_bar`, `file_menu`, `projects_menu`, `status_line` in `src/ui/elek_view.rs` |
| Samples page tools: **🔊 Preview on click**, **⬇ Fetch all for preview**, Cache menu, Undo / Reset, **⚡ Live** | `preview_tools`, `order_tools` |
| Live mode (push each pool edit after a pause; needs the whole project read, every pattern renumbered) | `App::live_push_tick`, `App::elek_push` in `src/app/elek.rs`; gate `ElekState::pool_pushable` |
| Read-back after a push skips empty patterns (no note trigs, kit samples or not: `PatternInfo::silent`) that the push leaves as read are handed to the read from memory, not asked for again (manual Load / Reload and the first read stay complete). **Save debug dump** leaves empty patterns out | `ElekProject::untouched_blanks`, `blank_patterns`; `ElekState::keep_blank` set in `App::elek_push`, passed by `App::elek_read` as `ElekCmd::Read::cached`; `save_dump` in `src/device/elek/dump.rs` |
| Samples page: two bank panes, slot rows, slot right-click menu (Play / Open local copy, Show original in source pane, Rename on +Drive…, Insert empty slot / Insert N above selection, Clear all unused) | `slot_pane`, `slot_row`; edits in `impl ElekState` (`insert_empty`); original lookup `Links::original`, handled under `ElekIntent::ShowOriginal`; rename dialog `rename_dialog`, run under `ElekIntent::RenameFile`, name updated in `App::absorb` (`pool_rename`) |
| Cross-pointing while the pool is shown: a local file under a pane's cursor flashes its slot; a slot under the pool cursor flashes its local file in the source pane (else the folder leading to it, else ".."). Nothing gets selected | `App::cross_ping`, `App::pool_shown` in `src/app/elek.rs`; `ElekState::ping_key`, `cursor_key`; `DirTab::ping`; drawn by `paint_ping` in `src/ui/file_table.rs`; match test `Link::is` |
| Edited since upload: pool samples whose local file changed are drawn amber (slot name and file row, `theme::CHANGED`), rechecked every 2 s; slot menu **⬆ Update on device from edited file** / **Update all N edited** uploads it beside the +Drive original (kept), fills the same slot, pushes once all land | `App::check_changed`, `App::update_changed`, `App::update_landed` in `src/app/elek.rs`; `Links::changed_copy`; `ElekState::changed_local`, `ElekIntent::UpdateChanged`; file rows via `TableCtx::changed` |
| Sample memory bar under the slot panes (fill vs 64 MB / 400 MB, change since read) | `memory_bar` in `src/ui/elek_view.rs`; capacity `Model::sample_memory` in `src/device/elek.rs` |
| Drag slots out onto a local file pane: download their +Drive files there; if some already have a local copy (not the preview cache), ask: download anyway / only missing / show the local file in that pane. With several slots, then ask whether to prefix names with slot numbers (`060_kick.wav`) | `ElekState::dragged_slots`; caught by pointer in `App::show_pane`, then `App::slots_dropped`, `App::answer_slot_download`, `App::download_slots`, `App::answer_slot_prefix`, `slot_prefixed` in `src/app/pane_view.rs`; dialogs `dialogs::slot_download`, `dialogs::slot_prefix`; the new names ride on `Task::Transfer::local_names` to `download` in `src/device/transfer.rs`; lookup `Links::user_copy` |
| Cut / copy / paste of slots: ⌘X then ⌘V fills the empty slots from the cursor down, ⇧⌘V inserts at the cursor and shifts the rest down (into the next empty slots); cut slots are left empty, drawn faded. Copied or cut slots pasted into a local file pane download like a slot drag; local files copied in a pane and pasted into the pool fill empty slots like a file drop. One clipboard: setting either drops the other | `ElekState::set_clip`, `paste`, `paste_files`, `clip_keys`, `PoolClip` in `src/ui/elek_view.rs`; keys in `ElekState::keys`; app side `ElekIntent::Clipped` / `PasteFiles` in `App::elek_intents`, `App::paste` and `App::set_clipboard` in `src/app/actions.rs`; pane button label `TableCtx::slot_clip` |
| Push / clear / close / stale-project confirmations | `confirm_push`, `confirm_clear`, `confirm_close`, `confirm_stale` in `src/ui/elek_view.rs` |
| Patterns page tools: Hide empty, Bytes, Open all / Fold all banks | `view_tools` in `src/ui/elek_project_view.rs` |
| Patterns page: Pack bank, Rename all, pattern list, step dots, row menu (Copy pattern, Save / Load pattern…). Selecting a pattern and its menu are on the header line only (`hresp`); the whole row drags it (`resp`), so folded-out tracks and SOUNDS lines keep their own copy menus | `action_bar`, `list`, `dots`, `mini` in `src/ui/elek_project_view.rs`; state in `ProjEd` |
| Patterns page **SOUNDS** foldout under a folded-out pattern's tracks: each kit track's sound decoded like the Sounds page (machine, SRC page, defaults hidden unless the Sounds page's **Show defaults** is on), the pool sound with the same name and sample (POOL), values differing from it amber. Right-click a sound line → **Copy sound** (sets the track sound clipboard, `SoundFold::copy`, for pasting into the sound pool or other patterns). Copies also put a status text on the system clipboard, since ⌘V only reaches egui when it holds text | `sound_foldout`, `SoundFold`, `ProjEd::snd_open` in `src/ui/elek_project_view.rs`; decoded at parse into `TrackInfo::sound` (`make_info` → `library::parse_sound`); shared helpers `param_at`, `is_default`, `machine_text`, `src_detail` in `src/ui/elek_library_view.rs` |
| Track copy / paste: click track names in folded-out rows to select; right-click **Copy ▸** track (⌘C) / track sound / track events and **Paste ▸** the same three (greyed until copied; ⌘V pastes whatever is copied) into other patterns (same track numbers), confirm when trigs would be lost. Two clipboards (events, sound); copying events alone drops the copied sound | `Part`, `TrackClip`, `ProjEd::click_track`, `copy_tracks`, `paste_tracks`, `apply_tracks`, `copy_items`, `paste_items`, `confirm_tracks` in `src/ui/elek_project_view.rs`; stored as `Cell::tracks` (events) and `Cell::sounds`, spliced at push by `Patterns::splice_track` (events: seq track, p-lock rows; sound: kit sound, DT2 MIDI bit) in `src/device/elek/patterns.rs` |
| Sounds page (tab after Patterns, `ElekPage::Library`): the **sound pool** only, drawn in the pixel font on the OLED background (row metrics `ROW_H`, `F_*`, column widths `*_W` at the top of the file). Editable with the whole project read (`sounds_locked`): drag a sound by its number/name onto another to swap (onto an empty one to move), click a row to highlight it (`LibraryView::selected`), click the highlighted name to rename it in its row (`LibraryView::renaming`), right-click Rename… / Delete / **Paste track sound** (the Patterns page's copied track sound, `ProjEd::copied_sounds`, into this sound or over it, more than one fill the next empty ones below; also ⌘V on a highlighted empty sound; `paste_track_sounds`, cell `SoundSrc::Track`, bytes from `ElekProject::track_sound_for_pool` via `Patterns::track_sound`, refused for MIDI tracks, empty sounds, or a sound version unlike the pool's; replaced sound's locks stay on it, `lock_map`). Right-click **Copy** / ⌘C on a highlighted sound copies a pool sound (`PoolClip` in `ProjEd::pool_clip`, `copy_pool_sound`; one clipboard with the copied tracks and patterns, `ProjEd::set_pool_clip`): pasted into another pool sound it duplicates it (locks stay with the original, `lock_map`); pasted onto kit tracks on the Patterns page (selected tracks: ⌘V or Paste here ▸; a SOUNDS line: right-click → Paste) it is stored as `Cell::pool` and written over the kit sound at push (`Patterns::put_track_sound` in `ElekProject::new_data_parts`, MIDI bit cleared; checked by `ElekProject::pool_sound_for_track`). Pool sounds are taken from the payloads before patterns are placed (`new_data_parts`' first half) (tried while locked, e.g. patterns still loading: a warning by the pointer and in the tool row, `LibraryView::blocked`, `blocked_tip`); edits in `LibraryView::sounds` (`SoundCell`, `SoundSrc`), undo / reset; pushed with **⬆ Push to device** by `ElekProject::build_push` → `sound_writes` (one `0x53` each, emptied ones as `blank_sound`), sound locks renumbered in every pattern (`sound_moves`, `move_locks`, `lock_spots`), checked after read-back (`ElekState::expect_sounds`); read by itself once per project when shown with the device plugged in, `LibraryView::auto_read`; each sound's sample, SRC page values (`PoolSound::src`, offsets `src_offsets` in `library.rs`, named per machine: Machine column + SRC cells, from the machine byte (DT1 sound+124, DT II sound+210) and the tables in `src/device/elek/machines.rs`, `PoolSound::params`) and the kit tracks with the same name and sample, `kit_uses`. SRC values at their defaults are left blank unless **Show defaults** is on (`is_default`, `LibraryView::show_defaults`). Clicking a sample pings its slot in the docked pool and plays it (`ElekIntent::Local`). | `src/ui/elek_library_view.rs` (`LibraryView`, `sound_pool`, `sound_rows`, `view_tools`); device side `src/device/elek/library.rs` (`Worker::read_sound_pool`, `Worker::read_sounds`; DT1 pool from the whole dump only, its `0x63` answers are kit track sounds: `single_sounds`); `ElekCmd::ReadSounds`, `ElekEv::Sounds` / `SoundProgress` handled in `App::elek_intents` / `App::elek_event` |
| Right column switch: FILES / SAMPLE POOL / +DRIVE (SAMPLE POOL and +DRIVE hidden while the device is unplugged or switched off; SAMPLE POOL only once the device view was entered with a device plugged in or a project is loaded, and not while the Samples page is in the centre). FILES and +DRIVE are the Destination pane on a local / device tab; the mode follows the active tab | `App::show_dest_col`, `show_files`, `show_drive`, `pool_in_col`, `pool_offered`, `drive_in_col` in `src/app/elek.rs`; widget `theme::column_switch` |
| Right column width (snaps to 420 px the first time the pool shows, max 680 while it shows) | `DOCK_W`, `DOCK_MAX_W`, `pool_snapped` in `App::ui` (`src/app/mod.rs`) |
| Device back after unplug / switch-off (or another unit on the cable): +Drive names dropped, project read again; asks first when unpushed edits are open (keep them, or drop and read); a project opened from a file is kept. A different unit by port name (`App::last_unit`, also caught on a One→One swap) drops the old project and its sound pool at once, edits included (`ElekState::close_project` → `LibraryView::forget`) | `App::device_reconnected`, `App::answer_reconnect` in `src/app/elek.rs` (called from `App::track_presence`); dialog `dialogs::device_reconnected` via `App::reconnect_ask` |
| First device-view entry per run: reads the project by itself, then docks the pool once names are in (the page stays as it is) | `App::greet_device`, `App::first_dock_now` (fired from `App::elek_event`) in `src/app/elek.rs` |

### Dialogs

Shared modals are functions in `src/ui/dialogs.rs` (conflicts, trash, duplicates,
device load, rename local copy, slot download, leave editor, save scope, name export, name kit, send back). Each
is opened by a pending field on `App` and answered at the end of `App::ui`.

## Styling

| What | Where |
|---|---|
| Global look: greys, accent, corner radius, spacing | `theme::apply` in `src/ui/theme.rs` |
| Accent colours | `ACCENT`, `ACCENT_BRIGHT`, `ON_ACCENT`; device yellow `DEVICE_ACCENT*` |
| Anything device-related: wash, frame, dimming when unplugged, icon | `device_wash`, `device_frame`, `device_dimmed`, `device_group`, `DEVICE_ICON` in `theme.rs` |
| Device-screen look (pixel font, OLED colours) | `oled_font`, `OLED*`, `install_fonts` in `theme.rs`; font is `assets/frucade.ttf` |
| Shared widgets: pane titles, tabs, section headers, × buttons, show toggles | `pane_title`, `mode_title`, `pane_tab`, `device_tab`, `section_header`, `close_button`, `vis_toggle`, `soft_toggle` (quiet on/off setting), `progress_bar` (square corners, use for every bar) in `theme.rs` |
| Editor colours (in/out, playhead, clip, limit, mono, marker) and sizes | consts at the top of `src/ui/editor_view/mod.rs` |
| File row height, device row text | `ROW_HEIGHT`, `OLED_TEXT` in `src/ui/file_table.rs` |
| Pattern list colours and metrics | consts at the top of `src/ui/elek_project_view.rs` |
| Window size, title, icon | `src/main.rs` |

Other one-off colours are local consts in the view that uses them.

## Jobs and queues

There are two mechanisms, and only the device one is a real queue.

| What | Where |
|---|---|
| Worker-to-UI events (`Ev`), job ids, cancel flags | `src/jobs.rs` (`Ev`, `Jobs::register`, `Emitter`) |
| Local work: one detached thread per request (list, dir size, copy/move, trash, duplicates, mono copy) | `Jobs::spawn` and `execute` in `src/jobs.rs` |
| **Device queue**: one thread, commands in order (`Cmd::{List, Job, Elek}`) | `Device`, `Task`, `Dir`, `Cmd` in `src/device/mod.rs` |
| Queue loop, parked jobs (`later`), counting ahead | `Worker::run`, `Worker::job`, `Worker::count_parked` in `src/device/worker.rs` |
| What may jump the queue (mkdir, non-overlapping rename; listings always) | `jumps_queue`, `serve_lists`, `involved` in `src/device/worker.rs` |
| Retries, timeouts, chunk size | consts in `src/device/mod.rs`; `proto::CHUNK` |
| Starting a device job from the UI | `App::run_device` in `src/app/transfers.rs` |
| Planning and conflicts before anything runs | `ops::plan` in `src/ops.rs`; `App::offer`, `App::launch`, `App::launch_merge`; `PlanKind` in `src/app/mod.rs` |
| Results landing in the UI (progress, finished, relist) | `App::absorb` in `src/app/transfers.rs` |
| Project reads and pushes | `ElekCmd` through the same queue; `Worker::elek` in `src/device/elek.rs` |
| Preview fetches going ahead of other work | `App::pump_fetch` in `src/app/elek.rs` |

## Device internals

| Topic | Where |
|---|---|
| SysEx wire format, command codes, 7-bit coding | `src/device/proto.rs` |
| Finding the port, handshake, product ids | `src/device/conn.rs` (`elektron_port`, `product`, `Conn::open`) |
| Plugged in or not (port scan) | `src/device/presence.rs`; reaction in `App::track_presence` |
| Device sample format and WAV conversion | `src/device/sample.rs` |
| Upload / download / copy / move / delete, `/trash` | `src/device/transfer.rs`; `device::TRASH` |
| Project dump: models, struct sizes per version, slot references, usage counts | `src/device/elek.rs` (`Model`, `Scan`, `SlotRef`, `ElekProject`) |
| Pattern parsing and editing | `src/device/elek/patterns.rs` |
| Project / pattern JSON files and bundles | `src/device/elek/file.rs`; UI side `App::save_elek_file`, `App::start_import` |
| Raw dump for format research | `src/device/elek/dump.rs` |

## Other topics

| Topic | Where |
|---|---|
| Keyboard shortcuts: browser | `src/keymap.rs` (`Action`, `Keymap::macos` / `windows`); dispatch `App::handle_keys`, `App::perform` in `src/app/actions.rs` |
| Keyboard shortcuts: editor | `keymap::editor_actions`, `EdAction`; dispatch `App::perform_editor` in `src/app/editing.rs` |
| Keyboard shortcuts: device view | `ElekState::keys` in `elek_view.rs`, `keys` in `elek_project_view.rs` |
| Playback engine, output device, live loop updates | `src/audio/engine.rs`; `App::start_play_from`, end of `App::logic` |
| Loading a file into the editor, auditioning, auto-play | `App::audition`, `App::follow_cursor`, `Editor::request` |
| Editor state, prefs, session restore | `Editor`, `Prefs`, `Session` in `src/editor/mod.rs` |
| Editor undo | `src/editor/history.rs` |
| File-operation undo (⌘Z in a pane) | `src/app/undo.rs` |
| Selection, in/out, snapping, normalize | `src/editor/selection.rs`; kit gain in `sel_gain.rs` |
| Limiter above 0 dB | `src/audio/limit.rs` |
| Slice markers in WAV files | `src/audio/cues.rs` |
| Waveform peak pyramid | `src/audio/peaks.rs` |
| Drag and drop inside the app | `DragPayload` in `file_table.rs`, `App::drop_kind` in `pane_view.rs`, `src/ui/drag_drop.rs` |
| Drag and drop with Finder / Explorer | `src/app/os_dnd.rs` |
| Clipboard, new folder, rename | `src/app/actions.rs` (pool slots: see Device view) |
| Tab listing, tree mode, filter, sort, selection | `src/tab.rs`, `src/fs_model.rs` |
| Folder sizes | `src/sizes.rs` |
| Audio fingerprints: editor's sample, kit and slice sources re-checked every 3 s on a thread (`Ev::Printed`); pool samples printed from unchanged downloads (preview cache) | `src/prints.rs` (`Prints`, `Ask::run`); `App::print_tick`, `App::prints_landed` in `src/app/prints.rs`; app data folder `links::data_dir` |
| On-disk stores (`device_links.tsv`, `fingerprints.tsv`): version line, append, background rewrite, `.bad` / `.bak` copies, logging | `src/store.rs` |
| What survives a restart | `src/state.rs` (`AppState`); written by `App::snapshot` |
| Log file (rotates to `sample_manager.old.log` past 1 MB) | `src/log.rs` |
| "Copying factory samples is not allowed" popup (once per run of queued jobs) | detected in `Worker::read_raw` (`device::FACTORY_LOCKED`, `is_factory`); collapsed and raised in `Ev::Finished` in `src/app/transfers.rs`; dialog `dialogs::factory_locked` |
| Build script options, bundling, Windows cross-build | `build.sh`, `build.rs` |


## Vocabulary

| When the user says | They mean | Start in |
|---|---|---|
| **device**, **on device**, **the Digitakt**, **Elektron** | An Elektron sampler on USB MIDI (Digitakt, Digitakt II; also Analog Rytm, Model:Samples for file browsing). Never the audio output device unless they say "output" or "audio device". | `src/device/mod.rs` |
| **+Drive**, **drive** (with a device in context) | The sampler's file storage, browsed over SysEx. Paths are plain absolute paths (`/samples/kicks`) in `PathBuf`s that never touch the local disk. A tab on it has `DirTab::device == true`. | `src/device/conn.rs`, `src/device/transfer.rs` |
| **drives** (local) | Mounted volumes, the `💾 ⏷` menu in a pane header. | `fs_model::volumes`, `App::drives_menu` in `src/app/nav.rs` |
| **Device view**, **Elek**, **device pages** | The view on the project loaded in the device's memory, in place of the editor. Three pages: **Samples** (code: `ElekPage::Order`, "Elek Sample Order"), **Patterns** (code: `ElekPage::Project`, "Elek Project Editor") and **Sounds** (code: `ElekPage::Library`). | `src/app/elek.rs`, `src/ui/elek_view.rs`, `src/ui/elek_project_view.rs` |
| **pool**, **sample pool**, **slot**, **bank** (Samples page) | The project's sample slots (`PoolEntry`), 128 per bank. | `src/device/elek.rs`, `ElekState` in `src/ui/elek_view.rs` |
| **docked pool** | The pool as a pane in the right column (SAMPLE POOL), beside the editor. | `App::dock_elek`, `App::show_dest_col` in `src/app/elek.rs`; `elek_view::show_docked` |
| **pattern**, **bank** (Patterns page) | One of 128 patterns, 16 per bank A to H. | `src/device/elek/patterns.rs` |
| **push**, **read / load project**, **read back** | Write edits into the device's memory / read the loaded project from it. Saved projects on the +Drive are never written (`proto::FORBIDDEN`). | `ElekCmd`, `ElekEv`, `Worker::elek`, `Conn::push_project` in `src/device/elek.rs`; answers in `App::elek_event` |
| **SliceKit** (code: kit, `is_kit`) | A second editor slot that collects slices (**K**). Called SliceKit in the UI so it isn't confused with the Digitakt's own "kit" (a pattern's sound set, `patterns.rs`), which keeps the word "kit" on the Patterns / Sounds pages. | `src/app/kit.rs`, `src/editor/clip.rs`. a kit may also reference something else. uncertain.|
| **grid kit** | A SliceKit laid out on 4 / 8 / 16 equal slots (only while the marker padlock is locked). | `src/editor/grid.rs` |
| **trim**, **tails**, **fades** (kits) | Cutting each slice's tail below a threshold. | `src/editor/trim.rs`, `src/editor/fade.rs`; UI in `src/ui/editor_view/trim.rs`, `strip.rs` |
| **source**, **destination**, **pane** | The left and right file columns (`Role::Source`, `Role::Dest`), each a `Pane` of `DirTab`s. | `src/pane.rs`, `src/tab.rs` |
| **commit** | Copy the source selection into the active destination tab; a single file open in the editor is written as heard. | `App::commit` in `src/app/transfers.rs` |
| **export** | Write the editor's selection to the destination. | `App::export*` in `src/app/editing.rs`, `Editor::export_with`, `src/audio/export.rs` |
| **update on device**, **pool edit** | A pool sample opened in the editor from its slot; ⌘S (**⬆ Save → slot**) uploads it next to the original, fill the same slot and push. | `src/app/pool_edit.rs` (`PoolEdit`, `update_on_device`, `pool_edit_filled`) |
| **save to slot** | Save to ⏷ → the editor's sample put into the selected slot (a filled one is replaced after a question) or the first empty one; once it lands it becomes a pool edit of that slot. | `src/app/pool_edit.rs` (`SlotSave`, `save_to_slot`, `slot_save_upload`) |
| **send back**, **Save to → New file next to …** | Upload an edited sample next to the device file it came from. | `App::begin_send_back`, `dialogs::send_back` |
| **fingerprint**, **print**, **content hash DB** | Our own hash of a file's decoded 16-bit PCM (several keys per local file: as is, and as each kind of upload would hold it), and of device samples via a downloaded copy. One global store for all drives, `fingerprints.tsv` beside `device_links.tsv`. Elektron's +Drive hash couldn't be reproduced (no CRC/Adler/MD5/SHA/FNV variant matches). | `src/prints.rs`; kept fresh by `App::print_tick`, used by `App::pool_match` in `src/app/prints.rs` |
| **link**, **local copy** | A device file known to match a local file by the +Drive's content hash. | `src/links.rs`, `Worker::note_link` |
| **preview cache** | Fetched device samples kept by content for playback. | `links::pool_cache_dir`, `PoolFetch` and `App::fetch_all` in `src/app/elek.rs` |
| **sound pool** (not the sample pool) | The project's 128 pool sounds (`0x53`, `PoolSound`), shown on the Sounds page. Presets in `/soundbanks` on the +Drive aren't read by the app. The project read takes the sound pool in after the sample names and before the patterns, unless the page in view says otherwise (`ReadFirst`, picked in `App::elek_read`: Patterns page → patterns, names, sounds; Sounds page → sounds, names, patterns; else names, sounds, patterns) (`Worker::read_sounds`, one `0x63` each, whole dump if one never comes; patterns last since they're the slow part; the Sounds page gets the sound pool as soon as it's in, `ElekEv::Sounds`), so pool pushes renumber the samples sounds play; a read stopped before then leaves `ElekProject::sounds_read` false and blocks pool pushes (`ElekState::pool_pushable`) | `src/device/elek/library.rs` |
| **mono**, **fold** | `Fold::{Mix, Left, Right}`. | `src/audio/stats.rs`, `Editor::set_mono` |
| **scalpel** / **map** / **nums** | Editor panels: code names `scope` / `overview` (`minimap`) / `positions`. | `src/ui/editor_view/` |
| **intent** | What a view returns instead of acting: `Intent` (file table), `EdIntent` (editor), `ElekIntent` (device view). The `App` method that drew the view handles them. | see Architecture |