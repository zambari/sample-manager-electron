//! What survives a restart: open tabs per pane, their sort, focus, the
//! hidden-files toggle, editor toggles and the output device. eframe stores
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
    pub show_hidden: bool,
    #[serde(default)]
    pub editor: crate::editor::Prefs,
    /// cpal device id; `None` means the system default.
    #[serde(default)]
    pub output_device: Option<String>,
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
