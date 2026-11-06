//! Find-and-replace renaming of several items at once.
//!
//! The pane shows a live preview while the fields are edited; nothing touches
//! the disk until Apply. All the checks happen here, against names gathered
//! once when the rename opened, so drawing the preview costs no file I/O.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

pub struct MultiRename {
    /// The items being renamed, in display order.
    pub items: Vec<(PathBuf, bool)>,
    pub find: String,
    pub replace: String,
    pub match_case: bool,
    /// Only replace inside the name before the last dot (files only).
    pub keep_ext: bool,
    /// On the device: chains (a → b while b → c) aren't supported there.
    pub device: bool,
    /// Lowercased names already present in each parent folder.
    taken: HashMap<PathBuf, HashSet<String>>,
    /// The Find field still has to grab focus.
    pub focus_find: bool,
}

#[derive(Clone, PartialEq)]
pub enum Status {
    Unchanged,
    Ok,
    Problem(String),
}

pub struct Row {
    pub from: PathBuf,
    pub old: String,
    pub new: String,
    /// Byte ranges of `old` that matched Find.
    pub old_hits: Vec<Range<usize>>,
    /// Byte ranges of `new` that were put in by Replace.
    pub new_hits: Vec<Range<usize>>,
    pub status: Status,
}

impl MultiRename {
    /// `listed`: names in each parent folder, for the device (the local disk
    /// is read here instead, so hidden files count too).
    pub fn new(
        items: Vec<(PathBuf, bool)>,
        device: bool,
        listed: impl Fn(&Path) -> Vec<String>,
    ) -> Self {
        let mut taken: HashMap<PathBuf, HashSet<String>> = HashMap::new();
        for (p, _) in &items {
            let Some(parent) = p.parent() else { continue };
            if taken.contains_key(parent) {
                continue;
            }
            let names: Vec<String> = if device {
                listed(parent)
            } else {
                std::fs::read_dir(parent)
                    .map(|rd| {
                        rd.flatten()
                            .map(|e| e.file_name().to_string_lossy().into_owned())
                            .collect()
                    })
                    .unwrap_or_default()
            };
            taken.insert(
                parent.to_path_buf(),
                names.into_iter().map(|n| n.to_lowercase()).collect(),
            );
        }
        Self {
            items,
            find: String::new(),
            replace: String::new(),
            match_case: true,
            keep_ext: true,
            device,
            taken,
            focus_find: true,
        }
    }

    pub fn preview(&self) -> Vec<Row> {
        let mut rows: Vec<Row> = self
            .items
            .iter()
            .map(|(p, is_dir)| {
                let old = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                // The part Find may touch: everything, or the stem.
                let end = if self.keep_ext && !is_dir {
                    stem_end(&old)
                } else {
                    old.len()
                };
                let old_hits = find_all(&old[..end], &self.find, self.match_case);
                let mut new = String::new();
                let mut new_hits = Vec::new();
                let mut at = 0;
                for h in &old_hits {
                    new.push_str(&old[at..h.start]);
                    let s = new.len();
                    new.push_str(&self.replace);
                    if !self.replace.is_empty() {
                        new_hits.push(s..new.len());
                    }
                    at = h.end;
                }
                new.push_str(&old[at..]);
                let status = if new == old {
                    Status::Unchanged
                } else {
                    Status::Ok
                };
                Row {
                    from: p.clone(),
                    old,
                    new,
                    old_hits,
                    new_hits,
                    status,
                }
            })
            .collect();

        // Two items can't land on the same name.
        let mut targets: HashMap<(PathBuf, String), usize> = HashMap::new();
        for r in rows.iter().filter(|r| r.status == Status::Ok) {
            let parent = r.from.parent().unwrap_or(Path::new("")).to_path_buf();
            *targets.entry((parent, r.new.to_lowercase())).or_default() += 1;
        }
        for r in rows.iter_mut().filter(|r| r.status == Status::Ok) {
            let parent = r.from.parent().unwrap_or(Path::new("")).to_path_buf();
            if let Some(why) = invalid(&r.new) {
                r.status = Status::Problem(why.to_owned());
            } else if targets
                .get(&(parent, r.new.to_lowercase()))
                .copied()
                .unwrap_or(0)
                > 1
            {
                r.status = Status::Problem("another item gets this name too".to_owned());
            }
        }

        // An item may take a name another item is moving away from -- but
        // only if that one really moves, so settle until nothing changes.
        loop {
            let freed: HashSet<(PathBuf, String)> = rows
                .iter()
                .filter(|r| r.status == Status::Ok)
                .filter_map(|r| Some((r.from.parent()?.to_path_buf(), r.old.to_lowercase())))
                .collect();
            let mut changed = false;
            for r in rows.iter_mut().filter(|r| r.status == Status::Ok) {
                let parent = r.from.parent().unwrap_or(Path::new("")).to_path_buf();
                let low = r.new.to_lowercase();
                if low == r.old.to_lowercase() {
                    continue; // only the case changes
                }
                let problem = if freed.contains(&(parent.clone(), low.clone())) {
                    self.device
                        .then(|| "taken by another item being renamed (not possible on the device)")
                } else if self.taken.get(&parent).is_some_and(|t| t.contains(&low)) {
                    Some("a file with this name already exists")
                } else {
                    None
                };
                if let Some(why) = problem {
                    r.status = Status::Problem(why.to_owned());
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        rows
    }
}

/// Renames to make, ordered so they can run one after another: deepest
/// first (a folder's contents before the folder), and anything whose target
/// is still held by another item goes through a temporary name. Returns
/// `(steps, finals)`: run `steps` in order; each final `(from, to)` pair is
/// what the user asked for.
pub fn plan(rows: &[Row]) -> (Vec<(PathBuf, PathBuf)>, Vec<(PathBuf, PathBuf)>) {
    let mut finals: Vec<(PathBuf, PathBuf)> = rows
        .iter()
        .filter(|r| r.status == Status::Ok)
        .map(|r| (r.from.clone(), r.from.with_file_name(&r.new)))
        .collect();
    finals.sort_by_key(|(f, _)| std::cmp::Reverse(f.components().count()));

    let sources: HashSet<String> = finals
        .iter()
        .map(|(f, _)| f.to_string_lossy().to_lowercase())
        .collect();
    let mut steps = Vec::new();
    let mut later = Vec::new();
    for (i, (from, to)) in finals.iter().enumerate() {
        let case_only =
            from.to_string_lossy().to_lowercase() == to.to_string_lossy().to_lowercase();
        if !case_only && sources.contains(&to.to_string_lossy().to_lowercase()) {
            let tmp = from.with_file_name(format!(".sm-rename-{}-{i}", std::process::id()));
            steps.push((from.clone(), tmp.clone()));
            later.push((tmp, to.clone()));
        } else {
            steps.push((from.clone(), to.clone()));
        }
    }
    steps.extend(later);
    (steps, finals)
}

fn invalid(name: &str) -> Option<&'static str> {
    if name.trim().is_empty() {
        Some("name would be empty")
    } else if name.contains('/') || name.contains('\\') || name.contains(':') {
        Some("names can't contain / \\ or :")
    } else if name == "." || name == ".." {
        Some("not a valid name")
    } else if name != name.trim() {
        Some("name would start or end with a space")
    } else {
        None
    }
}

/// Where the stem ends: at the last dot, unless the name starts with it.
fn stem_end(name: &str) -> usize {
    match name.rfind('.') {
        Some(i) if i > 0 => i,
        _ => name.len(),
    }
}

/// Non-overlapping matches of `needle` in `hay`, left to right.
fn find_all(hay: &str, needle: &str, match_case: bool) -> Vec<Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }
    if match_case {
        return hay
            .match_indices(needle)
            .map(|(i, m)| i..i + m.len())
            .collect();
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < hay.len() {
        if let Some(len) = starts_with_ci(&hay[i..], needle) {
            out.push(i..i + len);
            i += len.max(1);
        } else {
            i += hay[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out
}

/// Case-insensitive prefix test; the matched byte length of `hay` if so.
fn starts_with_ci(hay: &str, needle: &str) -> Option<usize> {
    let mut h = hay.char_indices();
    for n in needle.chars() {
        let (_, c) = h.next()?;
        if !c.to_lowercase().eq(n.to_lowercase()) {
            return None;
        }
    }
    Some(h.next().map_or(hay.len(), |(i, _)| i))
}
