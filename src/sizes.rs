//! Recursive directory sizes, computed once and kept across frames.
//!
//! This is the answer to "can we keep that state between frames?" -- yes, and
//! it's the only sane way: the UI body re-runs on every repaint, so a walk
//! started there would run again and again. Instead the UI only ever *reads*
//! this map, and asks the worker to fill a hole when the user requests it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug)]
pub enum Size {
    Pending,
    Known { bytes: u64, files: u64 },
}

#[derive(Default)]
pub struct SizeCache {
    map: HashMap<PathBuf, Size>,
}

impl SizeCache {
    pub fn get(&self, path: &Path) -> Option<Size> {
        self.map.get(path).copied()
    }

    /// Returns true if a walk should be started (i.e. we don't already have it
    /// and aren't already computing it).
    pub fn request(&mut self, path: &Path) -> bool {
        if self.map.contains_key(path) {
            return false;
        }
        self.map.insert(path.to_path_buf(), Size::Pending);
        true
    }

    pub fn set(&mut self, path: PathBuf, bytes: u64, files: u64) {
        self.map.insert(path, Size::Known { bytes, files });
    }

    /// Drop anything at or below `root` -- call after an operation changes it,
    /// otherwise we'd report a stale size forever.
    pub fn invalidate_subtree(&mut self, root: &Path) {
        self.map.retain(|k, _| !k.starts_with(root));
    }

    pub fn bytes_of(&self, path: &Path) -> Option<u64> {
        match self.get(path) {
            Some(Size::Known { bytes, .. }) => Some(bytes),
            _ => None,
        }
    }
}
