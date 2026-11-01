//! One open directory: its listing, selection, sort and rename state.
//!
//! Several of these live side by side inside a [`crate::pane::Pane`] as
//! sub-tabs, so you can keep a handful of folders open in parallel per side.

use crate::fs_model::{Entry, SortKey, sort_entries};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub struct DirTab {
    pub id: u64,
    pub cwd: PathBuf,
    pub entries: Vec<Entry>,
    pub error: Option<String>,
    pub loading: bool,
    /// Incremented on every listing request. A reply with an older generation
    /// is stale (the user navigated on) and gets dropped.
    pub generation: u64,
    pub selected: HashSet<PathBuf>,
    /// Index into `entries` of the keyboard cursor.
    pub cursor: Option<usize>,
    /// Where a shift-range selection started.
    pub anchor: Option<usize>,
    pub sort_key: SortKey,
    pub sort_asc: bool,
    pub filter: String,
    /// `Some((path, edited_name))` while an inline rename is open.
    pub renaming: Option<(PathBuf, String)>,
    /// Set when the cursor moves by keyboard, so the table scrolls to follow.
    pub scroll_to_cursor: bool,
}

impl DirTab {
    pub fn new(id: u64, cwd: PathBuf) -> Self {
        Self {
            id,
            cwd,
            entries: Vec::new(),
            error: None,
            loading: true,
            generation: 0,
            selected: HashSet::new(),
            cursor: None,
            anchor: None,
            sort_key: SortKey::Name,
            sort_asc: true,
            filter: String::new(),
            renaming: None,
            scroll_to_cursor: false,
        }
    }

    pub fn title(&self) -> String {
        self.cwd
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.cwd.display().to_string())
    }

    /// Bump the generation and mark as loading. Caller then posts the request.
    pub fn begin_reload(&mut self) -> u64 {
        self.generation += 1;
        self.loading = true;
        self.generation
    }

    pub fn accept_listing(&mut self, generation: u64, result: Result<Vec<Entry>, String>) {
        if generation != self.generation {
            return; // stale
        }
        self.loading = false;
        match result {
            Ok(mut entries) => {
                sort_entries(&mut entries, self.sort_key, self.sort_asc);
                // Keep selection only for paths that still exist.
                let present: HashSet<&PathBuf> = entries.iter().map(|e| &e.path).collect();
                self.selected.retain(|p| present.contains(p));
                self.entries = entries;
                self.error = None;
                self.clamp_cursor();
            }
            Err(e) => {
                self.entries.clear();
                self.error = Some(e);
                self.cursor = None;
            }
        }
    }

    pub fn resort(&mut self) {
        sort_entries(&mut self.entries, self.sort_key, self.sort_asc);
        self.clamp_cursor();
    }

    fn clamp_cursor(&mut self) {
        let n = self.visible_indices().len();
        if n == 0 {
            self.cursor = None;
        } else if let Some(c) = self.cursor {
            self.cursor = Some(c.min(n - 1));
        }
    }

    /// Indices into `entries` passing the current filter. The table renders
    /// these, and the cursor indexes into *this* list, not `entries`.
    pub fn visible_indices(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.entries.len()).collect();
        }
        let needle = self.filter.to_ascii_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.name.to_ascii_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn entry_at_visible(&self, vis: usize) -> Option<&Entry> {
        self.visible_indices().get(vis).map(|&i| &self.entries[i])
    }

    pub fn cursor_entry(&self) -> Option<&Entry> {
        self.cursor.and_then(|c| self.entry_at_visible(c))
    }

    /// The paths an action should apply to: the selection if there is one,
    /// otherwise just the cursor row.
    pub fn effective_selection(&self) -> Vec<PathBuf> {
        if !self.selected.is_empty() {
            // Return in display order rather than HashSet order, so progress
            // and error messages read predictably.
            return self
                .entries
                .iter()
                .filter(|e| self.selected.contains(&e.path))
                .map(|e| e.path.clone())
                .collect();
        }
        self.cursor_entry().map(|e| vec![e.path.clone()]).into_iter().flatten().collect()
    }

    pub fn move_cursor(&mut self, delta: isize, extend: bool) {
        let vis = self.visible_indices();
        if vis.is_empty() {
            return;
        }
        let cur = self.cursor.unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, vis.len() as isize - 1) as usize;
        if extend {
            let anchor = self.anchor.unwrap_or(cur as usize);
            self.anchor = Some(anchor);
            self.select_range(anchor, next);
        } else {
            self.anchor = Some(next);
            self.selected.clear();
            if let Some(&i) = vis.get(next) {
                self.selected.insert(self.entries[i].path.clone());
            }
        }
        self.cursor = Some(next);
        self.scroll_to_cursor = true;
    }

    pub fn select_range(&mut self, a: usize, b: usize) {
        let vis = self.visible_indices();
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        self.selected.clear();
        for v in lo..=hi {
            if let Some(&i) = vis.get(v) {
                self.selected.insert(self.entries[i].path.clone());
            }
        }
    }

    pub fn select_all(&mut self) {
        let vis = self.visible_indices();
        self.selected = vis.iter().map(|&i| self.entries[i].path.clone()).collect();
    }

    pub fn toggle_at(&mut self, vis: usize) {
        if let Some(e) = self.entry_at_visible(vis) {
            let p = e.path.clone();
            if !self.selected.remove(&p) {
                self.selected.insert(p);
            }
        }
    }

    pub fn navigate_to(&mut self, path: PathBuf) {
        self.cwd = path;
        self.selected.clear();
        self.cursor = None;
        self.anchor = None;
        self.filter.clear();
        self.renaming = None;
    }

    pub fn parent(&self) -> Option<PathBuf> {
        self.cwd.parent().map(Path::to_path_buf)
    }
}
