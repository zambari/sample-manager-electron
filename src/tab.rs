//! One open directory: its listing, selection, sort and rename state.
//!
//! Several of these live side by side inside a [`crate::pane::Pane`] as
//! sub-tabs, so you can keep a handful of folders open in parallel per side.
//!
//! Raw listings (`listing`, plus `children` for folders expanded in tree mode)
//! are kept as they came off disk. `entries` is the flattened, sorted and
//! filtered list of rows actually shown -- the cursor and the table index
//! into that, and [`DirTab::rebuild`] regenerates it whenever an input changes.

use crate::fs_model::{Entry, SortKey, sort_entries};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub struct DirTab {
    pub id: u64,
    pub cwd: PathBuf,
    /// Browsing the Digitakt's +Drive rather than the local disk. `cwd` and
    /// every entry path are then device paths.
    pub device: bool,
    /// The rows on screen, in order. In tree mode this includes the contents
    /// of expanded folders, each followed by its children.
    pub entries: Vec<Entry>,
    /// Indent level of each row in `entries` (0 = directly in `cwd`).
    pub depths: Vec<u16>,
    /// Parallel to `entries`: the shortened display name, when `shorten` is on
    /// and the name shares a long prefix with many siblings.
    pub short_names: Vec<Option<String>>,
    /// Raw listing of `cwd`.
    listing: Vec<Entry>,
    /// Raw listings of folders expanded in tree mode.
    children: HashMap<PathBuf, Vec<Entry>>,
    /// Folders expanded in tree mode. A folder can be expanded before its
    /// listing has arrived; it just shows no children until then.
    pub expanded: HashSet<PathBuf>,
    /// Latest request number per expanded folder, to drop stale replies.
    child_gen: HashMap<PathBuf, u64>,
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
    /// Show subfolders inline as an expandable tree.
    pub tree: bool,
    /// List every file, not just ones the editor can load.
    pub show_all: bool,
    /// Collapse prefixes shared by many files: `M(...)TOM_234.wav`.
    pub shorten: bool,
    /// `Some((path, edited_name))` while an inline rename is open.
    pub renaming: Option<(PathBuf, String)>,
    /// Find-and-replace over several items; takes over the pane while open.
    pub multi_rename: Option<crate::rename::MultiRename>,
    /// A click on the already-selected row arms a rename that fires once the
    /// double-click window passes: `(path, time it fires)`.
    pub slow_click_rename: Option<(PathBuf, f64)>,
    /// Set when the cursor moves by keyboard, so the table scrolls to follow.
    pub scroll_to_cursor: bool,
}

impl DirTab {
    pub fn new(id: u64, cwd: PathBuf) -> Self {
        Self {
            id,
            cwd,
            device: false,
            entries: Vec::new(),
            depths: Vec::new(),
            short_names: Vec::new(),
            listing: Vec::new(),
            children: HashMap::new(),
            expanded: HashSet::new(),
            child_gen: HashMap::new(),
            error: None,
            loading: true,
            generation: 0,
            selected: HashSet::new(),
            cursor: None,
            anchor: None,
            sort_key: SortKey::Name,
            sort_asc: true,
            filter: String::new(),
            tree: false,
            show_all: false,
            shorten: false,
            renaming: None,
            multi_rename: None,
            slow_click_rename: None,
            scroll_to_cursor: false,
        }
    }

    pub fn new_device(id: u64, cwd: PathBuf) -> Self {
        Self {
            device: true,
            ..Self::new(id, cwd)
        }
    }

    pub fn title(&self) -> String {
        if self.device {
            let name = self
                .cwd
                .file_name()
                .map(|n| n.to_string_lossy().into_owned());
            return format!("🎹 {}", name.as_deref().unwrap_or("+Drive"));
        }
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

    /// Like [`Self::begin_reload`], for one expanded folder's listing.
    pub fn begin_child_load(&mut self, dir: &Path) -> u64 {
        let g = self.child_gen.entry(dir.to_path_buf()).or_insert(0);
        *g += 1;
        *g
    }

    /// Expanded folders whose listings the tree needs, for a full reload.
    pub fn expanded_dirs(&self) -> Vec<PathBuf> {
        if self.tree {
            self.expanded.iter().cloned().collect()
        } else {
            Vec::new()
        }
    }

    pub fn accept_listing(&mut self, generation: u64, result: Result<Vec<Entry>, String>) {
        if generation != self.generation {
            return; // stale
        }
        self.loading = false;
        match result {
            Ok(entries) => {
                self.listing = entries;
                self.error = None;
                // Forget folders that vanished, so a later folder of the same
                // name doesn't reappear expanded.
                let present: HashSet<&PathBuf> = self
                    .listing
                    .iter()
                    .filter(|e| e.is_dir)
                    .map(|e| &e.path)
                    .collect();
                self.expanded
                    .retain(|p| p.parent() != Some(self.cwd.as_path()) || present.contains(p));
                self.rebuild();
                let shown: HashSet<&PathBuf> = self.entries.iter().map(|e| &e.path).collect();
                self.selected.retain(|p| shown.contains(p));
                self.cursor_to_selection();
            }
            Err(e) => {
                self.listing.clear();
                self.entries.clear();
                self.depths.clear();
                self.error = Some(e);
                self.cursor = None;
            }
        }
    }

    pub fn accept_child_listing(
        &mut self,
        dir: &Path,
        generation: u64,
        result: Result<Vec<Entry>, String>,
    ) {
        if self.child_gen.get(dir) != Some(&generation) || !self.expanded.contains(dir) {
            return; // stale, or collapsed meanwhile
        }
        match result {
            Ok(entries) => {
                self.children.insert(dir.to_path_buf(), entries);
            }
            // Unreadable folder: show it collapsed rather than failing the tab.
            Err(_) => {
                self.expanded.remove(dir);
                self.children.remove(dir);
            }
        }
        self.rebuild();
    }

    pub fn is_expanded(&self, dir: &Path) -> bool {
        self.tree && self.expanded.contains(dir)
    }

    /// Expand a folder. Returns true if the caller should request its listing.
    pub fn expand(&mut self, dir: &Path) -> bool {
        if !self.tree {
            return false;
        }
        self.expanded.insert(dir.to_path_buf());
        self.rebuild();
        true
    }

    pub fn collapse(&mut self, dir: &Path) {
        // Collapse the whole subtree, so re-expanding starts tidy.
        self.expanded.retain(|p| !p.starts_with(dir));
        self.children.retain(|p, _| !p.starts_with(dir));
        // Selected rows inside the collapsed folder are no longer visible, and
        // acting on invisible rows would surprise.
        self.selected.retain(|p| !p.starts_with(dir) || p == dir);
        self.rebuild();
    }

    pub fn set_tree(&mut self, on: bool) {
        self.tree = on;
        if !on {
            self.expanded.clear();
            self.children.clear();
            self.child_gen.clear();
        }
        self.rebuild();
    }

    pub fn resort(&mut self) {
        self.rebuild();
    }

    /// Regenerate `entries` from the raw listings, keeping the cursor on the
    /// same path where possible.
    pub fn rebuild(&mut self) {
        let cursor_path = self.cursor_entry().map(|e| e.path.clone());
        let anchor_path = self
            .anchor
            .and_then(|a| self.entries.get(a))
            .map(|e| e.path.clone());

        let needle = self.filter.to_ascii_lowercase();
        let mut entries = Vec::new();
        let mut depths = Vec::new();
        self.flatten(&self.listing, 0, &needle, &mut entries, &mut depths);
        self.short_names = if self.shorten {
            short_names(&entries)
        } else {
            Vec::new()
        };
        self.entries = entries;
        self.depths = depths;

        let find = |p: &Option<PathBuf>, v: &[Entry]| {
            p.as_ref().and_then(|p| v.iter().position(|e| &e.path == p))
        };
        self.anchor = find(&anchor_path, &self.entries);
        self.cursor = match find(&cursor_path, &self.entries) {
            Some(i) => Some(i),
            None if self.entries.is_empty() => None,
            None => self.cursor.map(|c| c.min(self.entries.len() - 1)),
        };
    }

    /// The name to draw for row `i`.
    pub fn display_name(&self, i: usize) -> Option<&str> {
        self.short_names.get(i)?.as_deref()
    }

    /// Append the visible rows of one listing. Returns whether anything was
    /// appended, so a folder can be kept when only its contents match the
    /// filter.
    fn flatten(
        &self,
        list: &[Entry],
        depth: u16,
        needle: &str,
        out: &mut Vec<Entry>,
        depths: &mut Vec<u16>,
    ) -> bool {
        let mut sorted: Vec<Entry> = list
            .iter()
            // Everything on the device is a sample.
            .filter(|e| e.is_dir || e.is_audio || self.show_all || self.device)
            .cloned()
            .collect();
        sort_entries(&mut sorted, self.sort_key, self.sort_asc);

        let start = out.len();
        for e in sorted {
            let name_hit = needle.is_empty() || e.name.to_ascii_lowercase().contains(needle);
            let kids = if e.is_dir && self.is_expanded(&e.path) {
                self.children.get(&e.path)
            } else {
                None
            };
            match kids {
                Some(kids) => {
                    let at = out.len();
                    out.push(e);
                    depths.push(depth);
                    let any = self.flatten(kids, depth + 1, needle, out, depths);
                    if !any && !name_hit {
                        out.remove(at);
                        depths.remove(at);
                    }
                }
                None if name_hit => {
                    out.push(e);
                    depths.push(depth);
                }
                None => {}
            }
        }
        out.len() > start
    }

    /// The raw listing of `dir`, if this tab has one.
    pub fn listing_of(&self, dir: &Path) -> Option<&[Entry]> {
        if dir == self.cwd && !self.loading {
            Some(&self.listing)
        } else {
            self.children.get(dir).map(Vec::as_slice)
        }
    }

    pub fn cursor_entry(&self) -> Option<&Entry> {
        self.cursor.and_then(|c| self.entries.get(c))
    }

    /// Row index of the folder containing row `i`, in tree mode.
    pub fn parent_row(&self, i: usize) -> Option<usize> {
        let d = *self.depths.get(i)?;
        if d == 0 {
            return None;
        }
        (0..i).rev().find(|&j| self.depths[j] < d)
    }

    /// The paths an action should apply to: the selection if there is one,
    /// otherwise just the cursor row.
    /// Every selected row, in display order, with whether it is a folder.
    pub fn selected_rows(&self) -> Vec<(PathBuf, bool)> {
        self.entries
            .iter()
            .filter(|e| self.selected.contains(&e.path))
            .map(|e| (e.path.clone(), e.is_dir))
            .collect()
    }

    /// Names listed in `dir` (the tab's folder or an expanded one).
    pub fn listed_names(&self, dir: &Path) -> Vec<String> {
        let list = if dir == self.cwd {
            Some(&self.listing)
        } else {
            self.children.get(dir)
        };
        list.map(|l| l.iter().map(|e| e.name.clone()).collect())
            .unwrap_or_default()
    }

    /// Open the Multi-Rename Tool over the selection (or the cursor row).
    pub fn begin_multi_rename(&mut self) {
        let mut items = self.selected_rows();
        if items.is_empty() {
            if let Some(e) = self.cursor_entry() {
                items.push((e.path.clone(), e.is_dir));
            }
        }
        if !items.is_empty() {
            self.renaming = None;
            self.multi_rename = Some(crate::rename::MultiRename::new(items, self.device, |d| {
                self.listed_names(d)
            }));
        }
    }

    pub fn effective_selection(&self) -> Vec<PathBuf> {
        if !self.selected.is_empty() {
            // Return in display order rather than HashSet order, so progress
            // and error messages read predictably.
            let mut out: Vec<PathBuf> = self
                .entries
                .iter()
                .filter(|e| self.selected.contains(&e.path))
                .map(|e| e.path.clone())
                .collect();
            // In a tree, a selected folder already carries its selected
            // children; acting on both would copy them twice.
            let dirs: Vec<PathBuf> = out.clone();
            out.retain(|p| !dirs.iter().any(|d| d != p && p.starts_with(d)));
            return out;
        }
        self.cursor_entry()
            .map(|e| vec![e.path.clone()])
            .into_iter()
            .flatten()
            .collect()
    }

    pub fn set_cursor(&mut self, i: usize) {
        if i < self.entries.len() {
            self.cursor = Some(i);
            self.anchor = Some(i);
            self.selected.clear();
            self.selected.insert(self.entries[i].path.clone());
            self.scroll_to_cursor = true;
        }
    }

    pub fn move_cursor(&mut self, delta: isize, extend: bool) {
        let n = self.entries.len();
        if n == 0 {
            return;
        }
        let cur = self.cursor.unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, n as isize - 1) as usize;
        if extend {
            let anchor = self.anchor.unwrap_or(cur as usize);
            self.anchor = Some(anchor);
            self.select_range(anchor, next);
        } else {
            self.anchor = Some(next);
            self.selected.clear();
            self.selected.insert(self.entries[next].path.clone());
        }
        self.cursor = Some(next);
        self.scroll_to_cursor = true;
    }

    pub fn select_range(&mut self, a: usize, b: usize) {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        self.selected.clear();
        for e in self.entries.iter().take(hi + 1).skip(lo) {
            self.selected.insert(e.path.clone());
        }
    }

    pub fn select_all(&mut self) {
        self.selected = self.entries.iter().map(|e| e.path.clone()).collect();
    }

    pub fn toggle_at(&mut self, i: usize) {
        if let Some(e) = self.entries.get(i) {
            let p = e.path.clone();
            if !self.selected.remove(&p) {
                self.selected.insert(p);
            }
        }
    }

    pub fn navigate_to(&mut self, path: PathBuf) {
        // Going up should land the cursor on the folder we came from.
        let came_from = self.cwd.clone();
        let going_up = came_from.parent() == Some(path.as_path());
        self.cwd = path;
        self.listing.clear();
        self.entries.clear();
        self.depths.clear();
        self.children.clear();
        self.expanded.clear();
        self.child_gen.clear();
        self.selected.clear();
        self.cursor = None;
        self.anchor = None;
        self.filter.clear();
        self.renaming = None;
        if going_up {
            self.selected.insert(came_from);
        }
    }

    /// After a listing arrives, put the cursor on a pre-selected row (the
    /// folder we just came up out of).
    fn cursor_to_selection(&mut self) {
        if self.cursor.is_none() {
            if let Some(i) = self
                .entries
                .iter()
                .position(|e| self.selected.contains(&e.path))
            {
                self.cursor = Some(i);
                self.anchor = Some(i);
                self.scroll_to_cursor = true;
            }
        }
    }

    pub fn parent(&self) -> Option<PathBuf> {
        self.cwd.parent().map(Path::to_path_buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(path: &str, is_dir: bool) -> Entry {
        let p = PathBuf::from(path);
        Entry {
            name: p.file_name().unwrap().to_string_lossy().into_owned(),
            is_audio: !is_dir && path.ends_with(".wav"),
            empty: false,
            path: p,
            is_dir,
            len: 0,
            modified: None,
        }
    }

    fn names(t: &DirTab) -> Vec<(u16, &str)> {
        t.depths
            .iter()
            .copied()
            .zip(t.entries.iter().map(|e| e.name.as_str()))
            .collect()
    }

    fn tab() -> DirTab {
        let mut t = DirTab::new(1, "/r".into());
        let g = t.begin_reload();
        t.accept_listing(
            g,
            Ok(vec![
                e("/r/kicks", true),
                e("/r/a.wav", false),
                e("/r/notes.txt", false),
            ]),
        );
        t
    }

    #[test]
    fn hides_non_audio_unless_show_all() {
        let mut t = tab();
        assert_eq!(names(&t), [(0, "kicks"), (0, "a.wav")]);
        t.show_all = true;
        t.rebuild();
        assert_eq!(names(&t), [(0, "kicks"), (0, "a.wav"), (0, "notes.txt")]);
    }

    #[test]
    fn tree_expands_and_filters_through_folders() {
        let mut t = tab();
        t.set_tree(true);
        assert!(t.expand(Path::new("/r/kicks")));
        let g = t.begin_child_load(Path::new("/r/kicks"));
        t.accept_child_listing(
            Path::new("/r/kicks"),
            g,
            Ok(vec![
                e("/r/kicks/k1.wav", false),
                e("/r/kicks/x.txt", false),
            ]),
        );
        assert_eq!(names(&t), [(0, "kicks"), (1, "k1.wav"), (0, "a.wav")]);
        assert_eq!(t.parent_row(1), Some(0));

        // Folder survives the filter because a child matches.
        t.filter = "k1".into();
        t.rebuild();
        assert_eq!(names(&t), [(0, "kicks"), (1, "k1.wav")]);

        t.filter.clear();
        t.collapse(Path::new("/r/kicks"));
        assert_eq!(names(&t), [(0, "kicks"), (0, "a.wav")]);
    }

    #[test]
    fn selected_folder_absorbs_its_children() {
        let mut t = tab();
        t.set_tree(true);
        t.expand(Path::new("/r/kicks"));
        let g = t.begin_child_load(Path::new("/r/kicks"));
        t.accept_child_listing(
            Path::new("/r/kicks"),
            g,
            Ok(vec![e("/r/kicks/k1.wav", false)]),
        );
        t.select_all();
        assert_eq!(
            t.effective_selection(),
            [PathBuf::from("/r/kicks"), PathBuf::from("/r/a.wav")]
        );
    }
}

/// How many files must share a prefix before it gets collapsed.
const SHORTEN_MIN_COUNT: usize = 10;
/// Shorter prefixes aren't worth replacing with `X(...)`.
const SHORTEN_MIN_PREFIX: usize = 8;

/// For each file, the longest prefix ending at a separator (`_ - . space`)
/// that at least `SHORTEN_MIN_COUNT` files share is collapsed to its first
/// character plus `(...)`. Folders are left alone.
fn short_names(entries: &[Entry]) -> Vec<Option<String>> {
    fn prefixes(name: &str) -> impl Iterator<Item = &str> {
        // Never eat into the extension: stop before the last '.'.
        let stem_end = name.rfind('.').filter(|&i| i > 0).unwrap_or(name.len());
        name[..stem_end]
            .char_indices()
            .filter(|&(_, c)| matches!(c, '_' | '-' | ' ' | '.'))
            .map(move |(i, c)| &name[..i + c.len_utf8()])
            .filter(|p| p.len() >= SHORTEN_MIN_PREFIX)
    }

    let mut counts: HashMap<&str, usize> = HashMap::new();
    for e in entries.iter().filter(|e| !e.is_dir) {
        for p in prefixes(&e.name) {
            *counts.entry(p).or_default() += 1;
        }
    }
    entries
        .iter()
        .map(|e| {
            if e.is_dir {
                return None;
            }
            let p = prefixes(&e.name)
                .filter(|p| counts.get(p).copied().unwrap_or(0) >= SHORTEN_MIN_COUNT)
                .last()?;
            let first = e.name.chars().next()?;
            Some(format!("{first}(...){}", &e.name[p.len()..]))
        })
        .collect()
}
