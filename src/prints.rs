//! Content fingerprints of local audio files and device samples, so a local
//! file can be matched to a pool sample without a transfer link and without
//! trusting file names.
//!
//! The +Drive's own hash (the one pools refer to) can't be reproduced here:
//! no common CRC-32 / Adler / MD5 / SHA-1 / FNV variant over the sample data
//! (with or without the 64-byte header, either byte order) gives it. So both
//! sides are fingerprinted by us instead, over decoded 16-bit PCM:
//!
//! - a local file gets several keys: its PCM as it is ("native", exact for a
//!   16-bit file, e.g. one downloaded from the device), and the PCM an upload
//!   would put on the device (stereo, and each mono fold), via
//!   [`crate::device::sample::to_device`] so the rounding is the upload's;
//! - a device sample `(hash, size)` gets the native key of a downloaded copy
//!   (preview cache or the user's own download), noted while that copy is
//!   unchanged. Fetch the pool for preview to fingerprint all of it.
//!
//! A local file and a device sample hold the same audio when the device key
//! is one of the local file's keys. Header and metadata (markers, names)
//! don't count, sample rate doesn't either (the device keeps the source rate).
//!
//! One store per user (next to `device_links.tsv`), not per folder or drive:
//! kits on one volume find sources on another. Tab-separated lines appended
//! as prints are made; later lines win; compacted on start.

use crate::audio::decode::Sample;
use crate::audio::stats::Fold;
use crate::device::sample::{to_device, HEADER_LEN};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Same slack as links: a device size may or may not count the header.
const SIZE_SLACK: u32 = 128;
/// Local rows kept across restarts.
const CAP: usize = 100_000;

/// What a local file looked like when fingerprinted.
#[derive(Clone, Debug, PartialEq)]
pub struct Print {
    pub len: u64,
    pub mtime: u64,
    /// `keys[0]` is the native key; the rest are what uploads would hold.
    pub keys: Vec<u64>,
}

/// A worker's answer: new local prints and device prints.
#[derive(Debug, Default)]
pub struct Found {
    pub local: Vec<(PathBuf, Print)>,
    pub device: Vec<((u32, u32), u64)>,
    /// Asked about but unreadable (not audio, gone): don't ask again soon.
    pub failed: Vec<PathBuf>,
}

/// What to fingerprint, built on the UI thread from cheap lookups.
#[derive(Default)]
pub struct Ask {
    /// Local files, with the stamp of the print we have (skip if unchanged).
    pub local: Vec<(PathBuf, Option<(u64, u64)>)>,
    /// Device samples, each with a downloaded copy and the stamp it had when
    /// downloaded: only an unchanged copy speaks for the device.
    pub device: Vec<((u32, u32), PathBuf, (u64, u64))>,
}

impl Ask {
    pub fn is_empty(&self) -> bool {
        self.local.is_empty() && self.device.is_empty()
    }

    /// Do the work. Off the UI thread: decodes audio.
    pub fn run(self) -> Found {
        let mut out = Found::default();
        for (p, had) in self.local {
            let Some(st) = crate::links::stamp(&p) else {
                out.failed.push(p);
                continue;
            };
            if had == Some(st) {
                continue;
            }
            match crate::audio::decode::decode(&p) {
                Ok(s) => out.local.push((p, Print { len: st.0, mtime: st.1, keys: keys_of(&s) })),
                Err(_) => out.failed.push(p),
            }
        }
        for (k, p, st) in self.device {
            if crate::links::stamp(&p) != Some(st) {
                continue;
            }
            if let Ok(s) = crate::audio::decode::decode(&p) {
                out.device.push((k, native(&s)));
            }
        }
        out
    }
}

/// FNV-1a, 64 bit: stable across builds (std's hasher isn't).
struct Fnv(u64);

impl Fnv {
    fn new(channels: usize, frames: usize) -> Self {
        let mut h = Self(0xcbf2_9ce4_8422_2325);
        h.bytes(&(channels as u64).to_be_bytes());
        h.bytes(&(frames as u64).to_be_bytes());
        h
    }

    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= u64::from(x);
            self.0 = self.0.wrapping_mul(0x100_0000_01b3);
        }
    }
}

/// The PCM as it is, rounded to 16 bits: what a 16-bit file holds exactly.
fn native(s: &Sample) -> u64 {
    let (ch, frames) = (s.channels.len(), s.frames());
    let mut h = Fnv::new(ch, frames);
    for f in 0..frames {
        for c in &s.channels {
            let v = (c[f] * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
            h.bytes(&v.to_be_bytes());
        }
    }
    h.0
}

/// What a device sample made by upload holds (header skipped).
fn uploaded(s: &Sample, stereo: bool) -> u64 {
    let raw = to_device(s, stereo);
    let ch = if raw[1] == 1 { 2 } else { 1 };
    let pcm = &raw[HEADER_LEN..];
    let mut h = Fnv::new(ch, pcm.len() / (2 * ch));
    h.bytes(pcm);
    h.0
}

/// Every key a local file can match by.
fn keys_of(s: &Sample) -> Vec<u64> {
    let mut keys = vec![native(s), uploaded(s, true)];
    if s.channels.len() > 1 {
        for fold in [Fold::Mix, Fold::Left, Fold::Right] {
            let one = Sample {
                path: s.path.clone(),
                rate: s.rate,
                channels: vec![(0..s.frames()).map(|i| fold.at(&s.channels, i)).collect()],
                bits: s.bits,
                format: s.format.clone(),
                file_len: s.file_len,
            };
            keys.push(uploaded(&one, false));
        }
    }
    let mut seen = Vec::with_capacity(keys.len());
    keys.retain(|k| {
        let new = !seen.contains(k);
        seen.push(*k);
        new
    });
    keys
}

fn path() -> Option<PathBuf> {
    Some(crate::links::data_dir()?.join("fingerprints.tsv"))
}

fn ok(p: &Path) -> bool {
    !p.to_string_lossy().contains(['\t', '\n', '\r'])
}

fn local_line(p: &Path, r: &Print) -> String {
    let keys: Vec<String> = r.keys.iter().map(|k| format!("{k:016x}")).collect();
    format!("L\t{}\t{}\t{}\t{}\n", r.len, r.mtime, keys.join(","), p.to_string_lossy())
}

fn device_line(k: (u32, u32), key: u64) -> String {
    format!("D\t{:08x}\t{}\t{key:016x}\n", k.0, k.1)
}

/// Every print in memory.
#[derive(Default)]
pub struct Prints {
    local: HashMap<PathBuf, Print>,
    /// Device prints by +Drive hash: `(size, key)`.
    device: HashMap<u32, Vec<(u32, u64)>>,
}

impl Prints {
    /// Read the store; rewrite it in the background when it holds many
    /// superseded lines.
    pub fn load() -> Self {
        let mut out = Self::default();
        let Some(p) = path() else { return out };
        let Ok(text) = std::fs::read_to_string(&p) else { return out };
        let mut lines = 0;
        for l in text.lines() {
            lines += 1;
            let f: Vec<&str> = l.splitn(5, '\t').collect();
            match f.as_slice() {
                ["L", len, mtime, keys, path] => {
                    let keys: Vec<u64> = keys.split(',').filter_map(|k| u64::from_str_radix(k, 16).ok()).collect();
                    if let (Ok(len), Ok(mtime), false) = (len.parse(), mtime.parse(), keys.is_empty()) {
                        out.local.insert(PathBuf::from(path), Print { len, mtime, keys });
                    }
                }
                ["D", hash, size, key] => {
                    if let (Ok(h), Ok(s), Ok(k)) = (u32::from_str_radix(hash, 16), size.parse(), u64::from_str_radix(key, 16)) {
                        out.put_device((h, s), k);
                    }
                }
                _ => {}
            }
        }
        let kept = out.local.len() + out.device.values().map(Vec::len).sum::<usize>();
        if lines > kept + kept / 4 + 100 || out.local.len() > CAP {
            let mut rows: Vec<(&PathBuf, &Print)> = out.local.iter().collect();
            rows.sort_by_key(|(_, r)| std::cmp::Reverse(r.mtime));
            rows.truncate(CAP);
            let mut body: String = rows.iter().rev().map(|(p, r)| local_line(p, r)).collect();
            for (h, v) in &out.device {
                for (s, k) in v {
                    body.push_str(&device_line((*h, *s), *k));
                }
            }
            std::thread::spawn(move || {
                let tmp = p.with_extension("tsv.tmp");
                if std::fs::write(&tmp, body).is_ok() {
                    let _ = std::fs::rename(&tmp, &p);
                }
            });
        }
        out
    }

    fn put_device(&mut self, k: (u32, u32), key: u64) -> bool {
        let v = self.device.entry(k.0).or_default();
        if v.iter().any(|&(s, old)| s == k.1 && old == key) {
            return false;
        }
        v.retain(|&(s, _)| s != k.1);
        v.push((k.1, key));
        true
    }

    /// Take a worker's answer and append what's new to the store.
    pub fn absorb(&mut self, found: Found) {
        let mut body = String::new();
        for (p, r) in found.local {
            if ok(&p) && self.local.get(&p) != Some(&r) {
                body.push_str(&local_line(&p, &r));
                self.local.insert(p, r);
            }
        }
        for (k, key) in found.device {
            if self.put_device(k, key) {
                body.push_str(&device_line(k, key));
            }
        }
        if body.is_empty() {
            return;
        }
        let Some(p) = path() else { return };
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let res = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p)
            .and_then(|mut f| f.write_all(body.as_bytes()));
        if let Err(e) = res {
            crate::log::line(format!("prints: could not write {}: {e}", p.display()));
        }
    }

    /// The print of local file `p` as last made (it may have changed since;
    /// the editor's files are re-checked every few seconds).
    pub fn local(&self, p: &Path) -> Option<&Print> {
        self.local.get(p)
    }

    /// The fingerprint of device sample `k`, if a downloaded copy was seen.
    pub fn device(&self, k: (u32, u32)) -> Option<u64> {
        let v = self.device.get(&k.0)?;
        v.iter()
            .filter(|(s, _)| s.abs_diff(k.1) <= SIZE_SLACK)
            .min_by_key(|(s, _)| s.abs_diff(k.1))
            .map(|&(_, key)| key)
    }

    /// Device sample `k` holds the same audio as local file `p`: `None`
    /// when either side hasn't been fingerprinted.
    pub fn same(&self, p: &Path, k: (u32, u32)) -> Option<bool> {
        let d = self.device(k)?;
        Some(self.local(p)?.keys.contains(&d))
    }
}
