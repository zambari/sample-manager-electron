//! Device files we have a local copy of: every upload and download notes the
//! +Drive's content hash for the file next to the local path it came from (or
//! went to). The hash is the device's own, read back from a folder listing
//! after the transfer, so it follows the file through renames, moves and
//! copies on the device, and project sample pools refer to it too.
//!
//! A link only counts while the local file is unchanged (same size and
//! modification time as when it was noted). Edit or move the local file and
//! the link quietly stops applying; the device file is fetched as before.
//!
//! Stored as one tab-separated line per link, appended by the device worker
//! as transfers happen. Later lines win; the file is compacted on start.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Links kept across restarts, newest first.
const CAP: usize = 50_000;
/// Hash and size match: the device's size may or may not count the 64-byte
/// sample header (and pools add 16 bytes), so allow a little slack.
const SIZE_SLACK: u32 = 128;

#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub hash: u32,
    /// Size the device listed.
    pub size: u32,
    /// Where it was on the device when linked (it may have moved since).
    pub device: String,
    pub local: PathBuf,
    /// The local file's size and modification time (ms) when linked.
    pub len: u64,
    pub mtime: u64,
    /// Uploaded from `local` (the original), rather than downloaded to it.
    pub uploaded: bool,
    /// Folded to mono on the way up: the local file has more channels.
    pub folded: bool,
}

impl Link {
    /// A link to `local` as it is on disk now.
    pub fn new(hash: u32, size: u32, device: String, local: &Path, uploaded: bool, folded: bool) -> Option<Self> {
        let (len, mtime) = stamp(local)?;
        let ok = |s: &str| !s.contains(['\t', '\n', '\r']);
        (hash != 0 && ok(&device) && ok(&local.to_string_lossy())).then(|| Self {
            hash,
            size,
            device,
            local: local.to_path_buf(),
            len,
            mtime,
            uploaded,
            folded,
        })
    }

    /// The local file is still there and unchanged. Touches the disk.
    pub fn fresh(&self) -> bool {
        stamp(&self.local) == Some((self.len, self.mtime))
    }

    /// The device file's name when it was linked.
    pub fn device_name(&self) -> &str {
        self.device.rsplit('/').next().unwrap_or(&self.device)
    }

    fn line(&self) -> String {
        let flags = format!(
            "{}{}",
            if self.uploaded { "u" } else { "d" },
            if self.folded { "m" } else { "" }
        );
        format!(
            "{:08x}\t{}\t{}\t{}\t{flags}\t{}\t{}\n",
            self.hash,
            self.size,
            self.len,
            self.mtime,
            self.device,
            self.local.to_string_lossy()
        )
    }

    fn parse(l: &str) -> Option<Self> {
        let mut f = l.split('\t');
        let hash = u32::from_str_radix(f.next()?, 16).ok()?;
        let size = f.next()?.parse().ok()?;
        let len = f.next()?.parse().ok()?;
        let mtime = f.next()?.parse().ok()?;
        let flags = f.next()?;
        let device = f.next()?.to_owned();
        let local = PathBuf::from(f.next()?);
        Some(Self {
            hash,
            size,
            device,
            local,
            len,
            mtime,
            uploaded: flags.starts_with('u'),
            folded: flags.contains('m'),
        })
    }
}

fn stamp(p: &Path) -> Option<(u64, u64)> {
    let md = std::fs::metadata(p).ok()?;
    let ms = md
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    md.is_file().then_some((md.len(), ms))
}

/// Where pool samples fetched only for previewing are kept, one folder per
/// `(hash, length)`. Kept across restarts; the Elek view can empty it.
pub fn pool_cache_dir() -> Option<PathBuf> {
    let dir = if cfg!(target_os = "windows") {
        PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("sample_manager").join("cache")
    } else {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        if cfg!(target_os = "macos") {
            home.join("Library/Caches/sample_manager")
        } else {
            home.join(".cache/sample_manager")
        }
    };
    Some(dir.join("pool"))
}

fn path() -> Option<PathBuf> {
    let dir = if cfg!(target_os = "windows") {
        PathBuf::from(std::env::var_os("APPDATA")?).join("sample_manager")
    } else {
        let home = PathBuf::from(std::env::var_os("HOME")?);
        if cfg!(target_os = "macos") {
            home.join("Library/Application Support/sample_manager")
        } else {
            home.join(".local/share/sample_manager")
        }
    };
    Some(dir.join("device_links.tsv"))
}

/// Note a link on disk. Called from the device worker, off the UI thread.
pub fn append(l: &Link) {
    let Some(p) = path() else { return };
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let res = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
        .and_then(|mut f| f.write_all(l.line().as_bytes()));
    if let Err(e) = res {
        crate::log::line(format!("links: could not write {}: {e}", p.display()));
    }
}

/// All links in memory, by hash.
#[derive(Default)]
pub struct Links {
    by_hash: HashMap<u32, Vec<Link>>,
    /// Newest link of each local file: `(hash, size)`.
    by_local: HashMap<PathBuf, (u32, u32)>,
}

impl Links {
    /// Read the saved links. Rewrites the file in the background when it
    /// holds many superseded lines.
    pub fn load() -> Self {
        let mut out = Self::default();
        let Some(p) = path() else { return out };
        let Ok(text) = std::fs::read_to_string(&p) else {
            return out;
        };
        let mut lines = 0;
        let cache = pool_cache_dir();
        for l in text.lines() {
            lines += 1;
            if let Some(link) = Link::parse(l) {
                // Preview cache emptied since: those links are gone for good.
                let purged = cache.as_ref().is_some_and(|c| link.local.starts_with(c)) && !link.local.exists();
                if !purged {
                    out.add(link);
                }
            }
        }
        let kept = out.len();
        if lines > kept + kept / 4 + 100 || kept > CAP {
            let mut all: Vec<&Link> = out.by_hash.values().flatten().collect();
            all.sort_by_key(|l| std::cmp::Reverse(l.mtime));
            all.truncate(CAP);
            let body: String = all.iter().rev().map(|l| l.line()).collect();
            std::thread::spawn(move || {
                let tmp = p.with_extension("tsv.tmp");
                if std::fs::write(&tmp, body).is_ok() {
                    let _ = std::fs::rename(&tmp, &p);
                }
            });
        }
        out
    }

    /// Drop every link to a local file under `dir` (it was emptied).
    pub fn forget_under(&mut self, dir: &Path) {
        for v in self.by_hash.values_mut() {
            v.retain(|l| !l.local.starts_with(dir));
        }
        self.by_hash.retain(|_, v| !v.is_empty());
        self.by_local.retain(|p, _| !p.starts_with(dir));
    }

    fn len(&self) -> usize {
        self.by_hash.values().map(Vec::len).sum()
    }

    /// Add `l`, replacing an older link of the same device file to the same
    /// local file. Newest first.
    pub fn add(&mut self, l: Link) {
        self.by_local.insert(l.local.clone(), (l.hash, l.size));
        let v = self.by_hash.entry(l.hash).or_default();
        v.retain(|o| !(o.local == l.local && o.size == l.size));
        v.insert(0, l);
    }

    /// The newest link of local file `p`, whether or not it changed since.
    pub fn for_local(&self, p: &Path) -> Option<&Link> {
        let (h, size) = *self.by_local.get(p)?;
        self.by_hash.get(&h)?.iter().find(|l| l.local == p && l.size == size)
    }

    /// The newest link of local file `p`, if the file is unchanged since.
    /// Touches the disk.
    pub fn local_link(&self, p: &Path) -> Option<&Link> {
        self.for_local(p).filter(|l| l.fresh())
    }

    fn candidates(&self, hash: u32, size: u32) -> impl Iterator<Item = &Link> {
        let v = self.by_hash.get(&hash).map(Vec::as_slice).unwrap_or_default();
        // Originals (uploads) first: they keep any slicing the editor added.
        let up = v.iter().filter(|l| l.uploaded);
        let down = v.iter().filter(|l| !l.uploaded);
        up.chain(down)
            .filter(move |l| hash != 0 && l.size.abs_diff(size) <= SIZE_SLACK)
    }

    /// Some link for this device file, without checking the local file is
    /// still there. Cheap enough for every frame.
    pub fn known(&self, hash: u32, size: u32) -> Option<&Link> {
        self.candidates(hash, size).next()
    }

    /// The local file this device file was uploaded from, even if it changed
    /// since: one still on disk first, unchanged ones before changed ones.
    /// Touches the disk.
    pub fn original(&self, hash: u32, size: u32) -> Option<&Link> {
        let ups = || self.candidates(hash, size).filter(|l| l.uploaded);
        ups().find(|l| l.fresh()).or_else(|| ups().find(|l| l.local.exists())).or_else(|| ups().next())
    }

    /// An unchanged local copy that isn't in the preview cache: one the user
    /// put somewhere themselves. Originals first. Touches the disk.
    pub fn user_copy(&self, hash: u32, size: u32) -> Option<&Link> {
        let cache = pool_cache_dir();
        self.candidates(hash, size)
            .filter(|l| !cache.as_ref().is_some_and(|c| l.local.starts_with(c)))
            .find(|l| l.fresh())
    }

    /// A link whose local file is still there and unchanged. Touches the
    /// disk, so call it on a click, not every frame.
    pub fn local(&self, hash: u32, size: u32) -> Option<&Link> {
        self.candidates(hash, size).find(|l| l.fresh())
    }
}
