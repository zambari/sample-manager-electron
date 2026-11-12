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
//! as prints are made; later lines win; compacted on start. Versioned and
//! logged through [`crate::store`]; anything unreadable is set aside and
//! rebuilt, since every print can be made again from the audio.

use crate::audio::decode::Sample;
use crate::audio::stats::Fold;
use crate::device::sample::{to_device, HEADER_LEN};
use std::collections::HashMap;
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
    /// The kit's slices, when asked: `(which kit, keys by slice)`.
    pub slices: Option<(SliceId, Vec<Vec<u64>>)>,
}

/// Which kit audio and slicing a set of slice prints belongs to.
#[derive(Clone, Debug, PartialEq)]
pub struct SliceId {
    /// The decoded sample's address: a new load or an edit makes a new one.
    pub sample: usize,
    pub bounds: Vec<(usize, usize)>,
}

/// What to fingerprint, built on the UI thread from cheap lookups.
#[derive(Default)]
pub struct Ask {
    /// Local files, with the stamp of the print we have (skip if unchanged).
    pub local: Vec<(PathBuf, Option<(u64, u64)>)>,
    /// Device samples, each with a downloaded copy and the stamp it had when
    /// downloaded: only an unchanged copy speaks for the device.
    pub device: Vec<((u32, u32), PathBuf, (u64, u64))>,
    /// The kit's audio and its slices, to fingerprint each slice.
    pub slices: Option<(std::sync::Arc<Sample>, SliceId)>,
}

impl Ask {
    pub fn is_empty(&self) -> bool {
        self.local.is_empty() && self.device.is_empty() && self.slices.is_none()
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
        if let Some((s, id)) = self.slices {
            let keys = id.bounds.iter().map(|&(a, b)| range_keys(&s, a, b)).collect();
            out.slices = Some((id, keys));
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
    native_of(&s.channels, 0, s.frames())
}

/// [`native`] of frames `[a, b)` of these channels.
fn native_of(channels: &[Vec<f32>], a: usize, b: usize) -> u64 {
    let mut h = Fnv::new(channels.len(), b - a);
    for f in a..b {
        for c in channels {
            let v = (c[f] * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
            h.bytes(&v.to_be_bytes());
        }
    }
    h.0
}

/// Keys of a kit slice, frames `[a, b)`, to look up the file it came
/// from. A kit downloaded from the device holds exactly what the upload
/// put there, which a source file's upload keys describe. A mono source in
/// a stereo kit sits on both sides alike: then it's keyed as mono too.
pub fn range_keys(s: &Sample, a: usize, b: usize) -> Vec<u64> {
    let b = b.min(s.frames());
    if a >= b {
        return Vec::new();
    }
    let mut keys = vec![native_of(&s.channels, a, b)];
    if s.channels.len() == 2 && s.channels[0][a..b] == s.channels[1][a..b] {
        keys.push(native_of(&s.channels[..1], a, b));
    }
    keys
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

const STORE: &str = "fingerprints";
/// Bump when the keys are computed differently: old ones would never match.
const VERSION: u32 = 1;

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
    /// Local files by each of their keys.
    by_key: HashMap<u64, Vec<PathBuf>>,
}

impl Prints {
    /// Read the store; rewrite it in the background when it holds many
    /// superseded lines.
    pub fn load() -> Self {
        let mut out = Self::default();
        let Some(p) = path() else { return out };
        // Everything here can be made again from the audio: anything this
        // version can't read is set aside and rebuilt as files are opened.
        let text = match crate::store::read(&p, STORE, VERSION) {
            crate::store::Read::Missing => return out,
            crate::store::Read::Current(t) => t,
            crate::store::Read::Unversioned(_) => {
                crate::store::set_aside(&p, STORE, "no version line");
                return out;
            }
            crate::store::Read::Other(first) => {
                crate::store::set_aside(&p, STORE, &format!("other version ({first})"));
                return out;
            }
        };
        let (mut lines, mut bad) = (0, 0);
        for l in text.lines().filter(|l| !l.is_empty()) {
            lines += 1;
            if !out.parse(l) {
                bad += 1;
            }
        }
        if bad > 0 {
            crate::log::line(format!("{STORE}: skipped {bad} of {lines} line(s) that couldn't be read"));
        }
        let kept = out.local.len() + out.device.values().map(Vec::len).sum::<usize>();
        if bad > 0 || lines > kept + kept / 4 + 100 || out.local.len() > CAP {
            let mut rows: Vec<(&PathBuf, &Print)> = out.local.iter().collect();
            rows.sort_by_key(|(_, r)| std::cmp::Reverse(r.mtime));
            rows.truncate(CAP);
            let mut body: String = rows.iter().rev().map(|(p, r)| local_line(p, r)).collect();
            for (h, v) in &out.device {
                for (s, k) in v {
                    body.push_str(&device_line((*h, *s), *k));
                }
            }
            crate::store::rewrite(p, STORE, VERSION, body);
        }
        out.reindex();
        out
    }

    /// One stored line into memory; `false` if it isn't one.
    fn parse(&mut self, l: &str) -> bool {
        let f: Vec<&str> = l.splitn(5, '\t').collect();
        match f.as_slice() {
            ["L", len, mtime, keys, path] if !path.is_empty() => {
                let keys: Option<Vec<u64>> = keys.split(',').map(|k| u64::from_str_radix(k, 16).ok()).collect();
                match (len.parse(), mtime.parse(), keys) {
                    (Ok(len), Ok(mtime), Some(keys)) if !keys.is_empty() => {
                        self.local.insert(PathBuf::from(path), Print { len, mtime, keys });
                        true
                    }
                    _ => false,
                }
            }
            ["D", hash, size, key] => match (u32::from_str_radix(hash, 16), size.parse(), u64::from_str_radix(key, 16)) {
                (Ok(h), Ok(s), Ok(k)) => {
                    self.put_device((h, s), k);
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn reindex(&mut self) {
        self.by_key.clear();
        for (p, r) in &self.local {
            for k in &r.keys {
                self.by_key.entry(*k).or_default().push(p.clone());
            }
        }
    }

    fn put_local(&mut self, p: PathBuf, r: Print) {
        if let Some(old) = self.local.get(&p) {
            for k in &old.keys {
                if let Some(v) = self.by_key.get_mut(k) {
                    v.retain(|x| x != &p);
                }
            }
        }
        for k in &r.keys {
            self.by_key.entry(*k).or_default().push(p.clone());
        }
        self.local.insert(p, r);
    }

    /// Local files holding audio with one of these keys, newest print
    /// first. They may have changed or gone since: check on use.
    pub fn files_with(&self, keys: &[u64]) -> Vec<&PathBuf> {
        let mut out: Vec<&PathBuf> = keys.iter().filter_map(|k| self.by_key.get(k)).flatten().collect();
        out.sort_by_key(|p| std::cmp::Reverse(self.local.get(*p).map_or(0, |r| r.mtime)));
        out.dedup();
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
                self.put_local(p, r);
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
        if let Some(p) = path() {
            crate::store::append(&p, STORE, VERSION, &body);
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
