//! The app's small on-disk stores (`device_links.tsv`, `fingerprints.tsv`):
//! tab-separated lines, appended as things happen, later lines winning,
//! compacted on start. The first line names the store and its version
//! (`#sample_manager fingerprints v1`), so a file from a different version
//! is noticed instead of misread. Problems go to the log.

use std::io::Write;
use std::path::{Path, PathBuf};

/// What reading a store found.
pub enum Read {
    /// No file yet.
    Missing,
    /// The lines after a matching version line.
    Current(String),
    /// A file from before version lines (all lines are data).
    Unversioned(String),
    /// Another version, or not ours: its first line.
    Other(String),
}

fn header(name: &str, version: u32) -> String {
    format!("#sample_manager {name} v{version}\n")
}

/// Read store `p`. Read errors are logged and count as missing.
pub fn read(p: &Path, name: &str, version: u32) -> Read {
    let text = match std::fs::read(p) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Read::Missing,
        Err(e) => {
            crate::log::line(format!("{name}: could not read {}: {e}", p.display()));
            return Read::Missing;
        }
    };
    let want = header(name, version);
    match text.split_once('\n') {
        Some((first, rest)) if first == want.trim_end() => Read::Current(rest.to_owned()),
        _ if text.is_empty() => Read::Current(String::new()),
        Some((first, _)) if first.starts_with('#') => Read::Other(first.to_owned()),
        None if text.starts_with('#') => Read::Other(text),
        _ => Read::Unversioned(text),
    }
}

/// Move a store out of the way (`<file>.bad`, replacing an older one),
/// saying why in the log.
pub fn set_aside(p: &Path, name: &str, why: &str) {
    let bad = with_suffix(p, ".bad");
    match std::fs::rename(p, &bad) {
        Ok(()) => crate::log::line(format!("{name}: {why}; moved it to {} and started afresh", bad.display())),
        Err(e) => crate::log::line(format!("{name}: {why}; could not move it aside: {e}")),
    }
}

/// Keep a copy of the store as it is (`<file>.bak`) before a rewrite drops
/// lines that couldn't be read.
pub fn back_up(p: &Path, name: &str) {
    let bak = with_suffix(p, ".bak");
    match std::fs::copy(p, &bak) {
        Ok(_) => crate::log::line(format!("{name}: kept a copy as {}", bak.display())),
        Err(e) => crate::log::line(format!("{name}: could not back up {}: {e}", p.display())),
    }
}

fn with_suffix(p: &Path, s: &str) -> PathBuf {
    let mut n = p.as_os_str().to_owned();
    n.push(s);
    PathBuf::from(n)
}

/// Append `body` (whole lines), starting the file with its version line
/// when it's new or empty. Errors are logged.
pub fn append(p: &Path, name: &str, version: u32, body: &str) {
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let res = std::fs::OpenOptions::new().create(true).append(true).open(p).and_then(|mut f| {
        if f.metadata()?.len() == 0 {
            f.write_all(header(name, version).as_bytes())?;
        }
        f.write_all(body.as_bytes())
    });
    if let Err(e) = res {
        crate::log::line(format!("{name}: could not write {}: {e}", p.display()));
    }
}

/// Replace the store with `body` in the background: written to a temporary
/// file, then renamed over, so a crash leaves the old one whole.
pub fn rewrite(p: PathBuf, name: &'static str, version: u32, body: String) {
    std::thread::spawn(move || {
        let tmp = with_suffix(&p, ".tmp");
        let res = std::fs::write(&tmp, header(name, version) + &body).and_then(|()| std::fs::rename(&tmp, &p));
        if let Err(e) = res {
            crate::log::line(format!("{name}: could not rewrite {}: {e}", p.display()));
        }
    });
}
