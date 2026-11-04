//! Planning a file operation *before* touching the disk.
//!
//! Every copy/move goes through [`plan`] first, so conflicts are known up front
//! and the user answers once instead of being interrupted mid-transfer.

use crate::fs_model::unique_sibling;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Copy,
    Move,
}

/// Paths put on the in-app clipboard by Cut or Copy, waiting for a Paste.
/// `Kind::Move` means they were cut.
#[derive(Clone, Debug)]
pub struct Clipboard {
    pub kind: Kind,
    /// The paths are on the Digitakt.
    pub device: bool,
    pub paths: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct Op {
    pub src: PathBuf,
    pub dst: PathBuf,
    /// Best-effort size, used only to weight the progress bar. For directories
    /// this is the cached recursive size if we have one, else 0.
    pub approx_bytes: u64,
    pub is_dir: bool,
    /// `dst` already exists and this op has not yet been resolved.
    pub conflict: bool,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub kind: Kind,
    pub ops: Vec<Op>,
    /// Sources we refused to plan, with the reason (e.g. copying a folder into
    /// itself). Shown to the user rather than silently dropped.
    pub rejected: Vec<(PathBuf, String)>,
}

impl Plan {
    pub fn conflicts(&self) -> usize {
        self.ops.iter().filter(|o| o.conflict).count()
    }

    pub fn total_bytes(&self) -> u64 {
        self.ops.iter().map(|o| o.approx_bytes).sum()
    }
}

/// How to handle destinations that already exist.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Policy {
    Replace,
    KeepBoth,
    Skip,
    /// Device transfers only (local merges go through [`merge`]): combine
    /// folders, skip files already there at the expected size, leave
    /// same-name files of another size alone.
    Merge,
}

impl Policy {
    pub fn label(self) -> &'static str {
        match self {
            Self::Replace => "Replace",
            Self::KeepBoth => "Keep both",
            Self::Skip => "Skip",
            Self::Merge => "Merge",
        }
    }
}

/// Build the op list for dropping `sources` into `dest_dir`.
///
/// `dir_size` lets the caller supply already-computed recursive sizes (from the
/// size cache) so the progress bar is weighted sensibly; returning `None` just
/// means that op contributes 0 to the total.
pub fn plan(
    kind: Kind,
    sources: &[PathBuf],
    dest_dir: &Path,
    dir_size: impl Fn(&Path) -> Option<u64>,
) -> Plan {
    let mut ops = Vec::new();
    let mut rejected = Vec::new();

    for src in sources {
        let Ok(md) = std::fs::symlink_metadata(src) else {
            rejected.push((src.clone(), "cannot read source".to_owned()));
            continue;
        };
        let is_dir = md.is_dir();
        let Some(name) = src.file_name() else {
            rejected.push((src.clone(), "source has no file name".to_owned()));
            continue;
        };
        let dst = dest_dir.join(name);

        if dst == *src {
            rejected.push((src.clone(), "source and destination are the same".to_owned()));
            continue;
        }
        // Copying a directory into its own subtree would recurse forever.
        if is_dir && dest_dir.starts_with(src) {
            rejected.push((src.clone(), "cannot copy a folder into itself".to_owned()));
            continue;
        }

        let approx_bytes = if is_dir {
            dir_size(src).unwrap_or(0)
        } else {
            md.len()
        };

        ops.push(Op {
            conflict: dst.exists(),
            src: src.clone(),
            dst,
            approx_bytes,
            is_dir,
        });
    }

    Plan { kind, ops, rejected }
}

/// Resolve every conflicting op with `policy`, returning the ops to execute.
///
/// Note: `Replace` on a directory replaces the whole directory; merging two
/// trees is not supported yet (see TODO in `jobs::execute`).
pub fn resolve(plan: &Plan, policy: Policy) -> Vec<Op> {
    plan.ops
        .iter()
        .filter_map(|op| {
            if !op.conflict {
                return Some(op.clone());
            }
            match policy {
                Policy::Skip | Policy::Merge => None,
                Policy::Replace => Some(Op { conflict: false, ..op.clone() }),
                Policy::KeepBoth => Some(Op {
                    dst: unique_sibling(&op.dst),
                    conflict: false,
                    ..op.clone()
                }),
            }
        })
        .collect()
}

/// A folder merge worked out up front: what to transfer, and what to leave.
#[derive(Default, Debug)]
pub struct Merge {
    /// Items with nothing in their way at the destination. A subfolder that
    /// doesn't exist there yet goes as one op.
    pub ops: Vec<Op>,
    /// Source files whose name and size already sit at the destination.
    /// Skipped on a copy; sent to the Trash on a move.
    pub dupes: Vec<PathBuf>,
    /// Same name, different size (or a file/folder mismatch): left alone.
    pub collisions: Vec<PathBuf>,
    /// Source folders merged into existing ones; on a move, emptied
    /// folders under them are removed afterwards.
    pub roots: Vec<PathBuf>,
}

/// Resolve `plan` by merging: conflicting folders are walked and combined,
/// conflicting files follow the same rule as files inside them (same size =
/// duplicate, otherwise left in place). `keep` filters which ops take part
/// at all (the rest are dropped).
pub fn merge(plan: &Plan, keep: impl Fn(&Op) -> bool) -> Merge {
    let mut m = Merge::default();
    for op in plan.ops.iter().filter(|o| keep(o)) {
        if op.conflict {
            if op.is_dir {
                m.roots.push(op.src.clone());
            }
            merge_item(&op.src, &op.dst, op.is_dir, &mut m);
        } else {
            m.ops.push(op.clone());
        }
    }
    m
}

fn merge_item(src: &Path, dst: &Path, is_dir: bool, m: &mut Merge) {
    let src_len = || std::fs::symlink_metadata(src).map_or(0, |md| md.len());
    let Ok(there) = std::fs::symlink_metadata(dst) else {
        let approx_bytes = if is_dir { 0 } else { src_len() };
        m.ops.push(Op { src: src.to_path_buf(), dst: dst.to_path_buf(), approx_bytes, is_dir, conflict: false });
        return;
    };
    if is_dir && there.is_dir() {
        let Ok(rd) = std::fs::read_dir(src) else {
            m.collisions.push(src.to_path_buf());
            return;
        };
        for de in rd.flatten() {
            let Ok(ft) = de.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            merge_item(&de.path(), &dst.join(de.file_name()), ft.is_dir(), m);
        }
        return;
    }
    // Finder's folder metadata differs between any two folders and means
    // nothing; treating it as a duplicate lets the source folder empty out.
    let finder_junk = src.file_name().is_some_and(|n| n == ".DS_Store");
    if !is_dir && there.is_file() && (finder_junk || there.len() == src_len()) {
        m.dupes.push(src.to_path_buf());
    } else {
        m.collisions.push(src.to_path_buf());
    }
}

/// `dst` already holds exactly what copying `src` would put there: the same
/// bytes for a file, or for a folder every file inside it present and
/// identical (extra files at the destination don't matter). Read errors count
/// as "no", so the user still gets asked.
pub fn already_there(src: &Path, dst: &Path) -> bool {
    let same_file = |a: &Path, b: &Path| -> bool {
        match (std::fs::metadata(a), std::fs::metadata(b)) {
            (Ok(ma), Ok(mb)) if ma.is_file() && mb.is_file() && ma.len() == mb.len() => {
                crate::jobs::same_contents(a, b).unwrap_or(false)
            }
            _ => false,
        }
    };
    if !src.is_dir() {
        return same_file(src, dst);
    }
    if !dst.is_dir() {
        return false;
    }
    for e in walkdir::WalkDir::new(src).min_depth(1) {
        let Ok(e) = e else { return false };
        let Ok(rel) = e.path().strip_prefix(src) else { return false };
        let there = dst.join(rel);
        let ok = if e.file_type().is_dir() { there.is_dir() } else { same_file(e.path(), &there) };
        if !ok {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    // Per-test directory: cargo runs tests in parallel, so a shared path
    // means one test's cleanup deletes another's fixtures.
    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sm_test_{}_{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn detects_conflicts_and_keeps_both() {
        let root = tmp("conflicts");
        let (src, dst) = (root.join("src"), root.join("dst"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("kick.wav"), b"aaa").unwrap();
        std::fs::write(src.join("snare.wav"), b"bb").unwrap();
        std::fs::write(dst.join("kick.wav"), b"existing").unwrap();

        let sources = vec![src.join("kick.wav"), src.join("snare.wav")];
        let plan = plan(Kind::Copy, &sources, &dst, |_| None);
        assert_eq!(plan.ops.len(), 2);
        assert_eq!(plan.conflicts(), 1);
        assert_eq!(plan.total_bytes(), 5);

        let keep = resolve(&plan, Policy::KeepBoth);
        assert_eq!(keep.len(), 2);
        assert!(keep.iter().any(|o| o.dst.ends_with("kick 2.wav")));

        assert_eq!(resolve(&plan, Policy::Skip).len(), 1);
        assert!(resolve(&plan, Policy::Replace).iter().all(|o| !o.conflict));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn refuses_to_copy_a_folder_into_itself() {
        let root = tmp("selfcopy");
        let outer = root.join("lib");
        let inner = outer.join("drums");
        std::fs::create_dir_all(&inner).unwrap();

        let plan = plan(Kind::Copy, &[outer.clone()], &inner, |_| None);
        assert!(plan.ops.is_empty());
        assert_eq!(plan.rejected.len(), 1);

        std::fs::remove_dir_all(&root).unwrap();
    }
}
