//! Project and pattern files: JSON around the device's own structs.
//!
//! The struct payloads (7-bit decoded, so offsets match elk-herd's
//! CppStructs) are kept byte for byte as base64: we don't know every field
//! yet, so nothing is re-encoded from a model of it. The JSON around them
//! says what's inside: format and version, the device model, struct
//! versions, names, and which +Drive file each sample slot points at
//! (`hash` + `len`, the key the device knows files by). That last part is
//! what lets a pattern go into another project with its samples found
//! again.
//!
//! A bundle is the same JSON in a folder with the WAVs beside it: each
//! sample's `file` names its copy, relative to the JSON (`samples/kick.wav`).
//! Loading uses the device's own file when it has that hash, and uploads
//! the copy only when it doesn't.
//!
//! Readers should ignore fields they don't know; bump `VERSION` only for
//! changes an older reader would get wrong.

use super::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const PROJECT_FORMAT: &str = "sample_manager/elk-project";
pub const PATTERN_FORMAT: &str = "sample_manager/elk-pattern";
const VERSION: u32 = 1;

/// One pool slot's file.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SampleRef {
    /// Pool slot, in the file's own numbering.
    pub slot: usize,
    /// The +Drive's content hash, 8 hex digits.
    pub hash: String,
    /// Sample length the +Drive knows the file by.
    pub len: u32,
    /// +Drive path when saved, if known (the file may have moved since).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Bundles: the WAV, relative to the JSON file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

impl SampleRef {
    pub fn key(&self) -> Option<(u32, u32)> {
        Some((u32::from_str_radix(&self.hash, 16).ok()?, self.len))
    }

    /// File name for messages: the path's last part, else the hash.
    pub fn short(&self) -> String {
        self.path
            .as_deref()
            .and_then(|p| p.rsplit('/').next())
            .map_or_else(|| format!("#{}", self.hash), str::to_owned)
    }

    /// The bundled WAV next to `json`, if there is one. Only plain relative
    /// paths count, so a file can't point outside its folder.
    pub fn bundled(&self, json: &Path) -> Option<PathBuf> {
        let f = Path::new(self.file.as_deref()?);
        let plain = f.components().all(|c| matches!(c, std::path::Component::Normal(_)));
        let p = json.parent()?.join(f);
        (plain && p.is_file()).then_some(p)
    }
}

/// A readable, unique file name for a bundled sample: the +Drive name (or
/// `fallback`), with `ext`. `taken` holds names used so far, lowercased.
pub fn bundle_name(s: &SampleRef, fallback: &str, ext: &str, taken: &mut HashSet<String>) -> String {
    let base = s.path.as_deref().and_then(|p| p.rsplit('/').next()).filter(|n| !n.is_empty()).unwrap_or(fallback);
    let stem = match base.rsplit_once('.') {
        Some((st, e)) if !st.is_empty() && e.eq_ignore_ascii_case(ext) => st,
        _ => base,
    };
    let clean: String = stem.chars().map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '_' } else { c }).collect();
    let clean = clean.trim().trim_start_matches('.');
    let clean = if clean.is_empty() { format!("{}_{}", s.hash, s.len) } else { clean.to_owned() };
    let mut name = format!("{clean}.{ext}");
    let mut n = 2;
    while !taken.insert(name.to_lowercase()) {
        name = format!("{clean} {n}.{ext}");
        n += 1;
    }
    name
}

/// A project or pattern file, built and not yet written: a bundle fills in
/// each sample's `file` once the WAVs are gathered.
pub enum SaveFile {
    Project(ProjectFile),
    Pattern(PatternFile),
}

impl SaveFile {
    pub fn samples_mut(&mut self) -> &mut Vec<SampleRef> {
        match self {
            Self::Project(f) => &mut f.samples,
            Self::Pattern(f) => &mut f.samples,
        }
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        match self {
            Self::Project(f) => write_json(path, f),
            Self::Pattern(f) => write_json(path, f),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Msg {
    /// Message type, hex: 50 pattern+kit, 53 pool sound, 54 settings.
    kind: String,
    index: u8,
    /// What it is, for people reading the file.
    #[serde(default)]
    label: String,
    /// The decoded struct, base64.
    data: String,
}

#[derive(Serialize, Deserialize)]
struct Head {
    format: String,
    version: u32,
    #[serde(default)]
    app: String,
    model: String,
    #[serde(default)]
    saved: String,
    /// Struct versions, as shown in the app.
    #[serde(default)]
    structs: String,
}

#[derive(Serialize, Deserialize)]
pub struct ProjectFile {
    #[serde(flatten)]
    head: Head,
    /// Every filled pool slot.
    samples: Vec<SampleRef>,
    messages: Vec<Msg>,
}

#[derive(Serialize, Deserialize)]
pub struct PatternFile {
    #[serde(flatten)]
    head: Head,
    name: String,
    /// Slot it was saved from, e.g. "A01".
    #[serde(default)]
    from_slot: String,
    /// The pool slots the pattern points at.
    samples: Vec<SampleRef>,
    /// The pattern+kit struct, base64.
    data: String,
}

/// A pattern read from a file.
pub struct LoadedPattern {
    pub name: String,
    pub data: Vec<u8>,
    pub samples: Vec<SampleRef>,
}

fn head(format: &str, model: Model, structs: String) -> Head {
    Head {
        format: format.into(),
        version: VERSION,
        app: format!("sample_manager {}", env!("CARGO_PKG_VERSION")),
        model: model.name().into(),
        saved: dump::stamp(),
        structs,
    }
}

/// Checks a file's header against what we expect, returning its model.
fn check(h: &Head, format: &str) -> Result<Model, String> {
    if h.format != format {
        let what = match h.format.as_str() {
            PROJECT_FORMAT => "a project file",
            PATTERN_FORMAT => "a pattern file",
            _ => "not a file this app wrote",
        };
        return Err(format!("that's {what}"));
    }
    if h.version > VERSION {
        return Err(format!("file format v{} is newer than this app reads (v{VERSION}); update the app", h.version));
    }
    [Model::Digitakt, Model::Digitakt2]
        .into_iter()
        .find(|m| m.name() == h.model)
        .ok_or_else(|| format!("unknown device model \"{}\"", h.model))
}

fn samples(pool: &[PoolEntry], slots: impl Iterator<Item = usize>, names: &HashMap<(u32, u32), String>) -> Vec<SampleRef> {
    slots
        .filter_map(|s| {
            let e = pool.get(s).filter(|e| !e.is_empty())?;
            let k = e.key();
            Some(SampleRef { slot: s, hash: format!("{:08x}", k.0), len: k.1, path: names.get(&k).cloned(), file: None })
        })
        .collect()
}

fn write_json<T: Serialize>(path: &Path, v: &T) -> Result<(), String> {
    let s = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    std::fs::write(path, s).map_err(|e| e.to_string())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let s = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&s).map_err(|e| format!("not a readable file: {e}"))
}

impl ElekProject {
    /// The project as edited (unpushed changes included). `pool` is the
    /// pool as it will be after a push (`ElekState::new_pool`).
    pub fn project_file(
        &self,
        order: &[u16],
        fills: &HashMap<u16, PoolEntry>,
        cells: &[Cell],
        pool: &[PoolEntry],
        names: &HashMap<(u32, u32), String>,
    ) -> Result<SaveFile, String> {
        if self.partial {
            return Err("the project is still being read".into());
        }
        let labels: HashMap<usize, String> = match &self.patterns {
            Ok(p) => (0..patterns::PATTERNS).map(|i| (p.msg_of(i), patterns::label(i))).collect(),
            Err(_) => HashMap::new(),
        };
        let messages = self
            .edited_msgs(order, fills, cells)
            .into_iter()
            .enumerate()
            .map(|(mi, (kind, index, data))| {
                let label = match kind {
                    PATTERN_KIT => labels.get(&mi).cloned().unwrap_or_else(|| format!("pattern {index}")),
                    SOUND => format!("pool sound {index}"),
                    SETTINGS => "project settings and sample pool".into(),
                    _ => String::new(),
                };
                Msg { kind: format!("{kind:02x}"), index, label, data: b64::encode(&data) }
            })
            .collect();
        let f = ProjectFile {
            head: head(PROJECT_FORMAT, self.model, self.versions.clone()),
            samples: samples(pool, 1..pool.len(), names),
            messages,
        };
        Ok(SaveFile::Project(f))
    }

    /// Read a project file, and its samples.
    pub fn load_file(path: &Path) -> Result<(Box<Self>, Vec<SampleRef>), String> {
        let f: ProjectFile = read_json(path)?;
        let model = check(&f.head, PROJECT_FORMAT)?;
        let msgs = f
            .messages
            .into_iter()
            .map(|m| {
                let kind = u8::from_str_radix(&m.kind, 16).map_err(|_| format!("bad message kind \"{}\"", m.kind))?;
                let data = b64::decode(&m.data).ok_or_else(|| format!("message {} #{}: bad data", m.kind, m.index))?;
                Ok((kind, m.index, data))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let mut p = Self::from_payloads(model, msgs)?;
        p.from_file = Some(path.to_path_buf());
        Ok((Box::new(p), f.samples))
    }

    /// The pattern in `slot` as edited, with the files its samples point
    /// at. `pool` as for `project_file`.
    #[allow(clippy::too_many_arguments)]
    pub fn pattern_file(
        &self,
        slot: usize,
        name: &str,
        order: &[u16],
        fills: &HashMap<u16, PoolEntry>,
        cells: &[Cell],
        pool: &[PoolEntry],
        names: &HashMap<(u32, u32), String>,
    ) -> Result<SaveFile, String> {
        let data = self
            .expected_patterns(order, fills, cells)
            .into_iter()
            .nth(slot)
            .ok_or("patterns aren't available")?;
        let slots = self.payload_slots(&data)?;
        let pv = be32(&data, 0).unwrap_or(0);
        let f = PatternFile {
            head: head(PATTERN_FORMAT, self.model, format!("pattern v{pv}")),
            name: name.into(),
            from_slot: patterns::label(slot),
            samples: samples(pool, slots.into_iter(), names),
            data: b64::encode(&data),
        };
        Ok(SaveFile::Pattern(f))
    }

    /// Read a pattern file meant for this project's model.
    pub fn load_pattern(&self, path: &Path) -> Result<LoadedPattern, String> {
        let f: PatternFile = read_json(path)?;
        let model = check(&f.head, PATTERN_FORMAT)?;
        if model != self.model {
            return Err(format!("it's a {} pattern; this project is for the {}", model.name(), self.model.name()));
        }
        let data = b64::decode(&f.data).ok_or("bad pattern data")?;
        Ok(LoadedPattern { name: f.name, data, samples: f.samples })
    }
}

/// Standard base64 with padding.
mod b64 {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(d: &[u8]) -> String {
        let mut s = String::with_capacity(d.len().div_ceil(3) * 4);
        for c in d.chunks(3) {
            let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
            for k in 0..4 {
                if k <= c.len() {
                    s.push(A[(n >> (18 - 6 * k) & 63) as usize] as char);
                } else {
                    s.push('=');
                }
            }
        }
        s
    }

    pub fn decode(s: &str) -> Option<Vec<u8>> {
        let v: Vec<u32> = s
            .bytes()
            .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
            .map(|b| A.iter().position(|&a| a == b).map(|p| p as u32))
            .collect::<Option<_>>()?;
        let mut out = Vec::with_capacity(v.len() * 3 / 4);
        for c in v.chunks(4) {
            if c.len() == 1 {
                return None;
            }
            let n = c.iter().enumerate().fold(0u32, |n, (k, &x)| n | x << (18 - 6 * k));
            out.extend(n.to_be_bytes()[1..c.len()].iter());
        }
        Some(out)
    }
}
