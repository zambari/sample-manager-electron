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
status text, latest error. Data: `ActiveJob` and `Batch` in `src/app/mod.rs`.

### File pane (Source / Destination): `App::show_pane` in `src/app/pane_view.rs`

| Control | Where |
|---|---|
| Pane title, path label / typed path bar | `show_pane`; `App::path_editor` in `src/app/nav.rs` |
| ★ favourites menu, `💾 ⏷` drives menu | `App::favourites_menu`, `App::drives_menu` in `src/app/nav.rs` |
| Device connection badge (● name / offline) | `App::device_badge` |
| Tab strip, `+` new tab, 🎹 open +Drive tab, × close, tab right-click menu, drops on tabs | `App::tab_strip` |
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
| "Sample Editor" title, **Device** switch, Samples / Patterns tabs | `toolbar.rs` `header`; switch widget is `theme::device_switch` |
| **Show** row of panel toggles | `toolbar.rs` `panel_toggles`; flags in `editor::Panels` |
| Sample / Kit tabs, kit switcher, ×, K arrow button, grid picker, fill boxes, **Start kit**, **Close all kits** | `toolbar.rs` `slot_bar`, `kit_tab`, `kit_switcher`, `grid_picker`, `add_slice_button` |
| **Transport** (play, Loop, Follow, Snap to 0, Auto-load, Auto-play), **Gain** (slider, Normalize, Reset), **Tools** (undo/redo, cut/copy/paste, +100 ms, Crop, loop halve/double) | `toolbar.rs` `toolbar` |
| Stereo / Mix / L / R | `toolbar.rs` `mono_picker` |
| **File** row: Save in place, **Export → Destination**, Export and close, Send to device | `toolbar.rs` `file_row` |
| Which file is open (Original / Device copy / changed since upload, with the original's path), click to show in source pane | `toolbar.rs` `origin_line`; worked out by `App::editor_origin`, shown by `App::show_in_source` in `src/app/editing.rs` |
| **⬆ Update on device (slot …)**: replaces Save / Export when the sample was opened from a pool slot | `file_row` (`Slots::pool_slot`); logic in `src/app/pool_edit.rs` |
| **⬆ Save to slot (…)**: shown when exactly one empty pool slot is selected in the device view; fills it with what you hear and pushes | `file_row` (`Slots::empty_slot`, `ElekState::lone_empty_sel`); logic in `src/app/pool_edit.rs` (`SlotSave`, `save_to_slot`) |
| **Trim** section (kits) | `trim.rs` `trim_section`, `strip.rs` |
| **Wave**: lanes, ruler, in/out and slice markers, fade handles, all mouse handling | `waveform.rs` |
| **Nums** | `toolbar.rs` `positions` |
| **Scalpel** | `scope.rs` |
| **Map** | `overview.rs` |
| **Stats**: levels, loudness, phase meter, waterfall | `stats_view.rs`; analysis in `src/editor/analysis.rs`, `src/audio/loudness.rs`, `spectrum.rs`, `stats.rs` |
| **Help** text | `keymap::EDITOR_HELP` |

### Device view

Intents handled in `App::elek_intents` (`src/app/elek.rs`).

| Control | Where |
|---|---|
| Mode header: Sample Editor / Device, **⇥ Dock beside files**, **Hide file panes** | `mode_header` in `src/ui/elek_view.rs` |
| Device bar: **⬇ Load current project**, **⬆ Push to device**, **💾 File** menu (Save / Load project…), **+Drive projects** menu, status line | `device_bar`, `file_menu`, `projects_menu`, `status_line` in `src/ui/elek_view.rs` |
| Samples page tools: **🔊 Preview on click**, **⬇ Fetch all for preview**, Cache menu, Undo / Reset, **⚡ Live** | `preview_tools`, `order_tools` |
| Live mode (push each pool edit after a pause; needs the whole project read, every pattern renumbered) | `App::live_push_tick`, `App::elek_push` in `src/app/elek.rs`; gate `ElekState::pool_pushable` |
| Samples page: two bank panes, slot rows, slot right-click menu (Play / Open local copy, Show original in source pane, Rename on +Drive…, Insert empty slot / Insert N above selection, Clear all unused) | `slot_pane`, `slot_row`; edits in `impl ElekState` (`insert_empty`); original lookup `Links::original`, handled under `ElekIntent::ShowOriginal`; rename dialog `rename_dialog`, run under `ElekIntent::RenameFile`, name updated in `App::absorb` (`pool_rename`) |
| Sample memory bar under the slot panes (fill vs 64 MB / 400 MB, change since read) | `memory_bar` in `src/ui/elek_view.rs`; capacity `Model::sample_memory` in `src/device/elek.rs` |
| Drag slots out onto a local file pane: download their +Drive files there; if some already have a local copy (not the preview cache), ask: download anyway / only missing / show the local file in that pane | `ElekState::dragged_keys`; caught by pointer in `App::show_pane`, then `App::slots_dropped`, `App::answer_slot_download` in `src/app/pane_view.rs`; dialog `dialogs::slot_download`; lookup `Links::user_copy` |
| Push / clear / close / stale-project confirmations | `confirm_push`, `confirm_clear`, `confirm_close`, `confirm_stale` in `src/ui/elek_view.rs` |
| Patterns page tools: Hide empty, Bytes, Open all / Fold all banks | `view_tools` in `src/ui/elek_project_view.rs` |
| Patterns page: Pack bank, Rename all, pattern list, step dots, row menu (Save / Load pattern…) | `action_bar`, `list`, `dots`, `mini` in `src/ui/elek_project_view.rs`; state in `ProjEd` |
| Docked column: Files / Sample pool switch | `App::show_dock_col` in `src/app/elek.rs` |
| Docked column width (snaps to 420 px when docking starts, max 680) | `DOCK_W`, `DOCK_MAX_W` in `App::ui` (`src/app/mod.rs`) |
| Docked column beside the Patterns page (not the Samples page) | `App::dock_col_shown` in `src/app/elek.rs` |
| First device-view entry per run: reads the project by itself, then Patterns + docked pool once names are in | `App::greet_device`, `App::first_dock_now` (fired from `App::elek_event`) in `src/app/elek.rs` |

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
| Shared widgets: pane titles, tabs, section headers, × buttons, show toggles | `pane_title`, `mode_title`, `pane_tab`, `device_tab`, `section_header`, `close_button`, `vis_toggle`, `progress_bar` (square corners, use for every bar) in `theme.rs` |
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
| Clipboard, new folder, rename | `src/app/actions.rs` |
| Tab listing, tree mode, filter, sort, selection | `src/tab.rs`, `src/fs_model.rs` |
| Folder sizes | `src/sizes.rs` |
| What survives a restart | `src/state.rs` (`AppState`); written by `App::snapshot` |
| Log file | `src/log.rs` |
| Build script options, bundling, Windows cross-build | `build.sh`, `build.rs` |


## Vocabulary

| When the user says | They mean | Start in |
|---|---|---|
| **device**, **on device**, **the Digitakt**, **Elektron** | An Elektron sampler on USB MIDI (Digitakt, Digitakt II; also Analog Rytm, Model:Samples for file browsing). Never the audio output device unless they say "output" or "audio device". | `src/device/mod.rs` |
| **+Drive**, **drive** (with a device in context) | The sampler's file storage, browsed over SysEx. Paths are plain absolute paths (`/samples/kicks`) in `PathBuf`s that never touch the local disk. A tab on it has `DirTab::device == true`. | `src/device/conn.rs`, `src/device/transfer.rs` |
| **drives** (local) | Mounted volumes, the `💾 ⏷` menu in a pane header. | `fs_model::volumes`, `App::drives_menu` in `src/app/nav.rs` |
| **Device view**, **Elek**, **device pages** | The view on the project loaded in the device's memory, in place of the editor. Two pages: **Samples** (code: `ElekPage::Order`, "Elek Sample Order") and **Patterns** (code: `ElekPage::Project`, "Elek Project Editor"). | `src/app/elek.rs`, `src/ui/elek_view.rs`, `src/ui/elek_project_view.rs` |
| **pool**, **sample pool**, **slot**, **bank** (Samples page) | The project's sample slots (`PoolEntry`), 128 per bank. | `src/device/elek.rs`, `ElekState` in `src/ui/elek_view.rs` |
| **docked pool** | The pool as a pane in the right column, beside the editor. | `App::dock_elek`, `App::show_dock_col` in `src/app/elek.rs`; `elek_view::show_docked` |
| **pattern**, **bank** (Patterns page) | One of 128 patterns, 16 per bank A to H. | `src/device/elek/patterns.rs` |
| **push**, **read / load project**, **read back** | Write edits into the device's memory / read the loaded project from it. Saved projects on the +Drive are never written (`proto::FORBIDDEN`). | `ElekCmd`, `ElekEv`, `Worker::elek`, `Conn::push_project` in `src/device/elek.rs`; answers in `App::elek_event` |
| **kit**, **slice kit** | A second editor slot that collects slices (**K**). Not the Digitakt's own "kit" (a pattern's sound set), which lives in `patterns.rs`. | `src/app/kit.rs`, `src/editor/clip.rs`. a kit may also reference something else. uncertain.|
| **grid kit** | A kit laid out on 4 / 8 / 16 equal slots. | `src/editor/grid.rs` |
| **trim**, **tails**, **fades** (kits) | Cutting each slice's tail below a threshold. | `src/editor/trim.rs`, `src/editor/fade.rs`; UI in `src/ui/editor_view/trim.rs`, `strip.rs` |
| **source**, **destination**, **pane** | The left and right file columns (`Role::Source`, `Role::Dest`), each a `Pane` of `DirTab`s. | `src/pane.rs`, `src/tab.rs` |
| **commit** | Copy the source selection into the active destination tab; a single file open in the editor is written as heard. | `App::commit` in `src/app/transfers.rs` |
| **export** | Write the editor's selection to the destination. | `App::export*` in `src/app/editing.rs`, `Editor::export_with`, `src/audio/export.rs` |
| **update on device**, **pool edit** | A pool sample opened in the editor from its slot; ⌘S / ↩ upload it next to the original, fill the same slot and push. | `src/app/pool_edit.rs` (`PoolEdit`, `update_on_device`, `pool_edit_filled`) |
| **save to slot** | The editor's sample put into the one empty slot selected on the Samples page; once it lands it becomes a pool edit of that slot. | `src/app/pool_edit.rs` (`SlotSave`, `save_to_slot`, `slot_save_upload`) |
| **send back**, **Send to device as…** | Upload an edited sample next to the device file it came from. | `App::begin_send_back`, `dialogs::send_back` |
| **link**, **local copy** | A device file known to match a local file by the +Drive's content hash. | `src/links.rs`, `Worker::note_link` |
| **preview cache** | Fetched device samples kept by content for playback. | `links::pool_cache_dir`, `PoolFetch` and `App::fetch_all` in `src/app/elek.rs` |
| **mono**, **fold** | `Fold::{Mix, Left, Right}`. | `src/audio/stats.rs`, `Editor::set_mono` |
| **scalpel** / **map** / **nums** | Editor panels: code names `scope` / `overview` (`minimap`) / `positions`. | `src/ui/editor_view/` |
| **intent** | What a view returns instead of acting: `Intent` (file table), `EdIntent` (editor), `ElekIntent` (device view). The `App` method that drew the view handles them. | see Architecture |