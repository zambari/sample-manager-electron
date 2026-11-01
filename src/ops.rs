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
}

impl Policy {
    pub fn label(self) -> &'static str {
        match self {
            Self::Replace => "Replace",
            Self::KeepBoth => "Keep both",
            Self::Skip => "Skip",
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
                Policy::Skip => None,
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
