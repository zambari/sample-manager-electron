//! Plain data describing what's on disk. No UI, no threads.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Extensions we treat as previewable samples.
pub const AUDIO_EXTS: &[&str] = &[
    "wav", "aiff", "aif", "flac", "mp3", "ogg", "m4a", "aac", "opus", "wv", "caf",
];

/// Does `path` have one of the [`AUDIO_EXTS`] extensions?
pub fn is_audio_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| AUDIO_EXTS.contains(&e.to_string_lossy().to_ascii_lowercase().as_str()))
}

/// A disk the file panes can jump to: a drive letter on Windows, a mounted
/// volume elsewhere.
#[derive(Clone, Debug)]
pub struct Volume {
    pub name: String,
    pub path: PathBuf,
}

/// Mounted disks, the system disk first. Probes the filesystem, so call it
/// now and then (when a menu opens), not every frame.
pub fn volumes() -> Vec<Volume> {
    let mut out = Vec::new();
    if cfg!(target_os = "windows") {
        for c in 'A'..='Z' {
            let path = PathBuf::from(format!("{c}:\\"));
            if path.is_dir() {
                out.push(Volume {
                    name: format!("{c}:"),
                    path,
                });
            }
        }
        return out;
    }

    let root = PathBuf::from("/");
    let mut mounts: Vec<PathBuf> = Vec::new();
    let mut root_name = "/".to_owned();
    let parents: Vec<PathBuf> = if cfg!(target_os = "macos") {
        vec!["/Volumes".into()]
    } else {
        let user = std::env::var_os("USER")
            .map(PathBuf::from)
            .unwrap_or_default();
        vec![
            Path::new("/media").join(&user),
            Path::new("/run/media").join(&user),
            "/mnt".into(),
        ]
    };
    for dir in parents {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = e.path();
            // macOS lists the system disk in /Volumes as a link back to "/".
            if path.canonicalize().is_ok_and(|c| c == root) {
                root_name = name;
                continue;
            }
            if path.is_dir() {
                mounts.push(path);
            }
        }
    }
    mounts.sort();
    out.push(Volume {
        name: root_name,
        path: root,
    });
    out.extend(mounts.into_iter().map(|path| {
        Volume {
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path,
        }
    }));
    out
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    /// Size of the file itself. Always 0 for directories -- a directory's
    /// recursive size is expensive, so it lives in [`crate::sizes::SizeCache`]
    /// and is only computed on demand.
    pub len: u64,
    pub modified: Option<SystemTime>,
    pub is_audio: bool,
    /// A folder with nothing in it (dotfiles like .DS_Store don't count).
    pub empty: bool,
    /// Device files: the +Drive's content hash. 0 for local files.
    pub hash: u32,
}

impl Entry {
    fn from_dir_entry(de: &std::fs::DirEntry) -> Option<Self> {
        let path = de.path();
        let name = de.file_name().to_string_lossy().into_owned();
        // `metadata()` on the DirEntry avoids a second stat on most platforms.
        let md = de.metadata().ok()?;
        let is_dir = md.is_dir();
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let empty = is_dir && dir_is_empty(&path);
        Some(Self {
            name,
            path,
            is_dir,
            len: if is_dir { 0 } else { md.len() },
            modified: md.modified().ok(),
            is_audio: !is_dir && AUDIO_EXTS.contains(&ext.as_str()),
            empty,
            hash: 0,
        })
    }
}

/// True when the folder has no visible children. Stops at the first one, so
/// it stays cheap even for huge folders. Unreadable folders aren't "empty".
fn dir_is_empty(path: &Path) -> bool {
    match std::fs::read_dir(path) {
        Ok(rd) => !rd
            .flatten()
            .any(|de| !de.file_name().to_string_lossy().starts_with('.')),
        Err(_) => false,
    }
}

/// Read one directory level. Unreadable children are skipped rather than
/// failing the whole listing; only an unreadable *parent* is an error.
pub fn list_dir(path: &Path, show_hidden: bool) -> std::io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for de in std::fs::read_dir(path)? {
        let Ok(de) = de else { continue };
        if !show_hidden && de.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if let Some(e) = Entry::from_dir_entry(&de) {
            out.push(e);
        }
    }
    Ok(out)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum SortKey {
    Name,
    Size,
    Modified,
    Kind,
}

impl SortKey {
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Size => "Size",
            Self::Modified => "Modified",
            Self::Kind => "Kind",
        }
    }
}

/// Sort in place. Directories always group before files, as in Finder.
pub fn sort_entries(v: &mut [Entry], key: SortKey, ascending: bool) {
    v.sort_by(|a, b| {
        let dirs_first = b.is_dir.cmp(&a.is_dir);
        if dirs_first != std::cmp::Ordering::Equal {
            return dirs_first;
        }
        let ord = match key {
            SortKey::Name => natural_cmp(&a.name, &b.name),
            SortKey::Size => a.len.cmp(&b.len),
            SortKey::Modified => a.modified.cmp(&b.modified),
            SortKey::Kind => ext_of(&a.path).cmp(&ext_of(&b.path)),
        };
        // Equal keys fall back to name so the order is stable and predictable.
        let ord = ord.then_with(|| natural_cmp(&a.name, &b.name));
        if ascending { ord } else { ord.reverse() }
    });
}

fn ext_of(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Case-insensitive compare that orders embedded digit runs numerically, so
/// `kick_2` sorts before `kick_10`. Sample libraries are full of these.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ac), Some(bc)) => {
                if ac.is_ascii_digit() && bc.is_ascii_digit() {
                    let an = take_number(&mut ai);
                    let bn = take_number(&mut bi);
                    match an.cmp(&bn) {
                        Ordering::Equal => continue,
                        other => return other,
                    }
                } else {
                    ai.next();
                    bi.next();
                    let (al, bl) = (ac.to_ascii_lowercase(), bc.to_ascii_lowercase());
                    match al.cmp(&bl) {
                        Ordering::Equal => continue,
                        other => return other,
                    }
                }
            }
        }
    }
}

fn take_number(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> u128 {
    let mut n: u128 = 0;
    while let Some(c) = it.peek().copied() {
        if !c.is_ascii_digit() {
            break;
        }
        it.next();
        // Saturate rather than wrap on absurdly long digit runs.
        n = n
            .saturating_mul(10)
            .saturating_add((c as u8 - b'0') as u128);
    }
    n
}

pub fn human_bytes(b: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    if b < 1024 {
        return format!("{b} B");
    }
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if v < 10.0 {
        format!("{v:.1} {}", UNITS[u])
    } else {
        format!("{v:.0} {}", UNITS[u])
    }
}

/// Pick a non-colliding name next to `dst` by appending ` 2`, ` 3`, ... before
/// the extension. Used by the "Keep both" conflict policy.
pub fn unique_sibling(dst: &Path) -> PathBuf {
    if !dst.exists() {
        return dst.to_path_buf();
    }
    let parent = dst.parent().unwrap_or(Path::new("."));
    let stem = dst
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = dst.extension().map(|e| e.to_string_lossy().into_owned());
    for n in 2..10_000u32 {
        let name = match &ext {
            Some(e) => format!("{stem} {n}.{e}"),
            None => format!("{stem} {n}"),
        };
        let cand = parent.join(name);
        if !cand.exists() {
            return cand;
        }
    }
    dst.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn digit_runs_sort_numerically() {
        assert_eq!(natural_cmp("kick_2", "kick_10"), Ordering::Less);
        assert_eq!(natural_cmp("kick_10", "kick_2"), Ordering::Greater);
        assert_eq!(natural_cmp("Kick_2", "kick_2"), Ordering::Equal);
        // Plain lexicographic would get this backwards.
        assert_ne!(natural_cmp("a9", "a10"), Ordering::Greater);
    }

    #[test]
    fn directories_group_before_files() {
        let mk = |name: &str, is_dir: bool| Entry {
            name: name.to_owned(),
            path: PathBuf::from(name),
            is_dir,
            len: 0,
            modified: None,
            is_audio: false,
            empty: false,
            hash: 0,
        };
        let mut v = vec![
            mk("zed.wav", false),
            mk("alpha", true),
            mk("abc.wav", false),
        ];
        sort_entries(&mut v, SortKey::Name, true);
        assert_eq!(
            v.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["alpha", "abc.wav", "zed.wav"]
        );
    }

    #[test]
    fn human_bytes_is_readable() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(50 * 1024 * 1024), "50 MB");
    }
}
