//! Sample audition.
//!
//! Deliberately a stub with a real seam. Wiring an audio backend (rodio +
//! symphonia, or cpal directly) is a decision about decoding, device handling
//! and latency that shouldn't be buried inside a file-manager scaffold -- but
//! every call site the UI needs already exists and is wired up, so dropping a
//! backend in later touches only this file.

use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct Preview {
    playing: Option<PathBuf>,
}

impl Preview {
    /// Space on a sample: start it, or stop it if it's already the one playing.
    pub fn toggle(&mut self, path: &Path) {
        if self.playing.as_deref() == Some(path) {
            self.stop();
        } else {
            // TODO: decode and play. Until then this just tracks intent so the
            // UI's playing indicator is already correct.
            self.playing = Some(path.to_path_buf());
        }
    }

    pub fn stop(&mut self) {
        self.playing = None;
    }

    pub fn playing(&self) -> Option<&Path> {
        self.playing.as_deref()
    }

    pub fn is_playing(&self, path: &Path) -> bool {
        self.playing.as_deref() == Some(path)
    }
}
