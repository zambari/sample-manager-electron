//! Append-only diagnostic log: `~/Library/Logs/sample_manager.log` on macOS,
//! `%LOCALAPPDATA%\sample_manager\sample_manager.log` on Windows,
//! `~/.local/state/sample_manager.log` elsewhere.
//! The UI only shows the latest error; this keeps all of them.
//! Past [`MAX_BYTES`] the file moves aside to `sample_manager.old.log`
//! (replacing the previous one), so the two together stay bounded.

use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 1 << 20;

fn path() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        let dir = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("sample_manager");
        let _ = std::fs::create_dir_all(&dir);
        return Some(dir.join("sample_manager.log"));
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    if cfg!(target_os = "macos") {
        Some(home.join("Library/Logs/sample_manager.log"))
    } else {
        let dir = home.join(".local/state");
        let _ = std::fs::create_dir_all(&dir);
        Some(dir.join("sample_manager.log"))
    }
}

pub fn line(msg: impl AsRef<str>) {
    let Some(p) = path() else { return };
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let t = secs % 86_400;
    let stamp = format!("{:02}:{:02}:{:02} UTC", t / 3600, t / 60 % 60, t % 60);
    if std::fs::metadata(&p).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = std::fs::rename(&p, p.with_extension("old.log"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
    {
        let _ = writeln!(f, "{stamp}  {}", msg.as_ref());
    }
}
