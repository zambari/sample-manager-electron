//! What's playing, as far as the file list cares: a mirror of the audio
//! engine's voice, refreshed by the app every frame so the table can draw its
//! speaker icon without touching the engine.

use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct Preview {
    playing: Option<PathBuf>,
}

impl Preview {
    pub fn set(&mut self, path: Option<PathBuf>) {
        self.playing = path;
    }

    pub fn playing(&self) -> Option<&Path> {
        self.playing.as_deref()
    }

    pub fn is_playing(&self, path: &Path) -> bool {
        self.playing.as_deref() == Some(path)
    }
}
