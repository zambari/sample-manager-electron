//! What survives a restart: open tabs per pane, their sort, focus, editor
//! toggles and the output device. eframe stores
//! it (with window size/position) in its per-app storage file and calls
//! [`crate::app::App`]'s `save` on exit and every [`SAVE_INTERVAL`].
//!
//! Transient state (selection, listings, jobs, dialogs) is deliberately left
//! out. Add a field here with `#[serde(default)]` so older saves still load.

use crate::fs_model::SortKey;
use crate::pane::Role;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const KEY: &str = "sample_manager_state";

/// Short, because the dev loop kills the app rather than quitting it --
/// whatever wasn't saved by then is lost.
pub const SAVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct TabState {
    pub cwd: PathBuf,
    pub sort_key: SortKey,
    pub sort_asc: bool,
    #[serde(default)]
    pub tree: bool,
    #[serde(default)]
    pub show_all: bool,
    #[serde(default)]
    pub shorten: bool,
    /// A Digitakt tab; `cwd` is a device path.
    #[serde(default)]
    pub device: bool,
}

#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct PaneState {
    pub tabs: Vec<TabState>,
    pub active: usize,
}

#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct AppState {
    #[serde(default)]
    pub source: PaneState,
    #[serde(default)]
    pub dest: PaneState,
    #[serde(default = "default_focus")]
    pub focused: Role,
    #[serde(default)]
    pub editor: crate::editor::Prefs,
    /// cpal device id; `None` means the system default.
    #[serde(default)]
    pub output_device: Option<String>,
    /// Bookmarked folders, shared by both panes, in the order added.
    #[serde(default)]
    pub favourites: Vec<PathBuf>,
    /// Files that have been loaded into the editor at least once; the file
    /// table dims their names.
    #[serde(default)]
    pub opened: Vec<PathBuf>,
    /// Files committed to a destination at least once; the file table shows
    /// a check badge next to them.
    #[serde(default)]
    pub committed: Vec<PathBuf>,
    /// File loaded in the editor with its in/out, playhead, gain and crop.
    #[serde(default)]
    pub session: Option<crate::editor::Session>,
    /// The slice kit's editor slot, same as `session`.
    #[serde(default)]
    pub kit_session: Option<crate::editor::Session>,
    /// Other open kits, parked behind the one K adds to.
    #[serde(default)]
    pub parked_kits: Vec<crate::editor::Session>,
    /// Grid slots last picked for a new kit (`None` = free). New key so
    /// saves from before Auto was the default start on Auto.
    #[serde(rename = "new_kit_grid", default = "default_kit_grid")]
    pub kit_grid: Option<usize>,
    /// The full-window device pages hide the file panes.
    #[serde(default)]
    pub device_hides_files: bool,
    /// Pattern banks folded out in the Device view (A to H).
    #[serde(default = "default_open_banks")]
    pub device_open_banks: [bool; 8],
}

fn default_open_banks() -> [bool; 8] {
    [true, false, false, false, false, false, false, false]
}

fn default_kit_grid() -> Option<usize> {
    Some(crate::editor::AUTO_GRID)
}

fn default_focus() -> Role {
    Role::Source
}

pub fn load(storage: Option<&dyn eframe::Storage>) -> Option<AppState> {
    eframe::get_value(storage?, KEY)
}

/// A saved folder may have been moved or unmounted since. Walk up to the
/// nearest directory that still exists rather than opening on an error.
pub fn existing_dir(p: &Path) -> Option<PathBuf> {
    p.ancestors().find(|a| a.is_dir()).map(Path::to_path_buf)
}
