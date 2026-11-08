//! The loaded project on a Digitakt / Digitakt II, over the device's own
//! "SysEx Dump" messages: read it, reorder its sample pool, write it back.
//!
//! Struct layouts come from elk-herd (BSD-2); see `project_discovery.md`.
//! Only the working copy in the device's memory is ever written. Saved
//! projects on the +Drive are listed, never touched (see `proto::FORBIDDEN`).

use super::*;

mod dump;
pub mod file;
pub mod patterns;

pub use patterns::{Cell, PatState, Patterns};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Model {
    Digitakt,
    Digitakt2,
}

impl Model {
    pub(super) fn from_product(id: u8) -> Option<Self> {
        match id {
            12 => Some(Model::Digitakt),
            42 => Some(Model::Digitakt2),
            _ => None,
        }
    }

    fn dev_byte(self) -> u8 {
        match self {
            Model::Digitakt => 0x0A,
            Model::Digitakt2 => 0x14,
        }
    }

    pub fn pool_size(self) -> usize {
        match self {
            Model::Digitakt => 128,
            Model::Digitakt2 => 1024,
        }
    }

    fn steps(self) -> usize {
        match self {
            Model::Digitakt => 64,
            Model::Digitakt2 => 128,
        }
    }

    pub fn kit_tracks(self) -> usize {
        match self {
            Model::Digitakt => 8,
            Model::Digitakt2 => 16,
        }
    }

    /// Send pacing, as elk-herd uses.
    fn bytes_per_ms(self) -> u64 {
        match self {
            Model::Digitakt => 200,
            Model::Digitakt2 => 800,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Model::Digitakt => "Digitakt",
            Model::Digitakt2 => "Digitakt II",
        }
    }
}

// ---- struct offsets (elk-herd CppStructs.elm) -------------------------------

use Model::{Digitakt as DT, Digitakt2 as DT2};

fn pattern_plocks(m: Model, v: u32) -> Option<usize> {
    Some(match (m, v) {
        (DT, 0) => 12468,
        (DT, 1 | 2) => 14516,
        (DT, 3..=6) => 14548,
        (DT, 7 | 8) => 14564,
        (DT, 9 | 10) => 14580,
        (DT2, 0) => 35316,
        (DT2, 3 | 4) => 68100,
        _ => return None,
    })
}

fn pattern_sizeof(m: Model, v: u32) -> Option<usize> {
    Some(match (m, v) {
        (DT, 0) => 23040,
        (DT, 1..=10) => 25088,
        (DT2, 0) => 56320,
        (DT2, 3 | 4) => 89088,
        _ => return None,
    })
}

fn kit_sounds(m: Model, v: u32) -> Option<usize> {
    Some(match (m, v) {
        (DT, 0..=10) => 36,
        (DT2, 0 | 3 | 4) => 60,
        _ => return None,
    })
}

fn kit_midi_mask(m: Model, v: u32) -> Option<usize> {
    Some(match (m, v) {
        (DT2, 0) => 9296,
        (DT2, 3) => 9972,
        (DT2, 4) => 22260,
        _ => return None,
    })
}

fn kit_sizeof(m: Model, v: u32) -> Option<usize> {
    Some(match (m, v) {
        (DT, 0..=10) => 2560,
        (DT2, 0) => 9728,
        (DT2, 3) => 10240,
        (DT2, 4) => 22528,
        _ => return None,
    })
}

/// Version of the sounds inside a kit of version `v`.
fn kit_sound_version(m: Model, v: u32) -> Option<u32> {
    Some(match (m, v) {
        (DT, 0..=5) => 0,
        (DT, 6 | 7) => 1,
        (DT, 8 | 9) => 2,
        (DT, 10) => 3,
        (DT2, 0) => 0,
        (DT2, 3) => 2,
        (DT2, 4) => 3,
        _ => return None,
    })
}

fn sound_slot(m: Model, v: u32) -> Option<usize> {
    Some(match (m, v) {
        (DT, 0) => 52,
        (DT, 1..=3) => 68,
        (DT2, 0 | 2 | 3) => 100,
        _ => return None,
    })
}

fn sound_sizeof(m: Model, v: u32) -> Option<usize> {
    Some(match (m, v) {
        (DT, 0..=3) => 160,
        (DT2, 0) => 299,
        (DT2, 2) => 341,
        (DT2, 3) => 1109,
        _ => return None,
    })
}

fn sample_param(m: Model, v: u32) -> Option<u8> {
    Some(match (m, v) {
        (DT, 0) => 12,
        (DT, 1..=3) => 20,
        (DT2, 0 | 2 | 3) => 36,
        _ => return None,
    })
}

fn settings_samples(m: Model, v: u32) -> Option<usize> {
    Some(match (m, v) {
        (DT, 0) => 69,
        (DT, 1..=8) => 70,
        (DT2, 0..=2) => 251,
        _ => return None,
    })
}

const MAGIC: u32 = 0xbeef_bace;
const PLOCK_ROWS: usize = 80;

// ---- dump messages ------------------------------------------------------------

pub const PATTERN_KIT: u8 = 0x50;
pub const SOUND: u8 = 0x53;
pub const SETTINGS: u8 = 0x54;
const REQ_PATTERN: u8 = 0x60;
const REQ_SETTINGS: u8 = 0x64;
const REQ_PROJECT: u8 = 0x6F;

/// One dump message: as received, and its decoded struct.
#[derive(Clone)]
pub struct DumpMsg {
    pub kind: u8,
    pub index: u8,
    pub raw: Vec<u8>,
    pub data: Vec<u8>,
}

/// `F0 00 20 3C <dev> 00 <kind> 01 01 <index> <7-bit data> <csum:2> <count:2> F7`
fn build_dump(m: Model, kind: u8, index: u8, data: &[u8]) -> Vec<u8> {
    let enc = proto::encode7(data);
    let sum: u32 = enc.iter().map(|&b| b as u32).sum();
    let count = enc.len() as u32 + 5;
    let mut out = vec![
        0xF0,
        0x00,
        0x20,
        0x3C,
        m.dev_byte(),
        0x00,
        kind,
        0x01,
        0x01,
        index,
    ];
    out.extend(enc);
    out.extend([((sum >> 7) & 0x7F) as u8, (sum & 0x7F) as u8]);
    out.extend([((count >> 7) & 0x7F) as u8, (count & 0x7F) as u8]);
    out.push(0xF7);
    out
}

/// `Ok(None)`: not a dump message from this device.
fn parse_dump(m: Model, raw: &[u8]) -> Result<Option<DumpMsg>, String> {
    if raw.len() < 15
        || raw[..6] != [0xF0, 0x00, 0x20, 0x3C, m.dev_byte(), 0x00]
        || raw[7..9] != [1, 1]
    {
        return Ok(None);
    }
    let n = raw.len();
    if raw[n - 1] != 0xF7 {
        return Err("dump message cut short".into());
    }
    let enc = &raw[10..n - 5];
    let sum: u32 = enc.iter().map(|&b| b as u32).sum();
    let csum = raw[n - 5] as u32 * 128 + raw[n - 4] as u32;
    let count = raw[n - 3] as u32 * 128 + raw[n - 2] as u32;
    if csum != sum & 0x3FFF || count != (enc.len() as u32 + 5) & 0x3FFF {
        return Err(format!(
            "dump message 0x{:02x} #{} failed its checksum",
            raw[6], raw[9]
        ));
    }
    Ok(Some(DumpMsg {
        kind: raw[6],
        index: raw[9],
        raw: raw.to_vec(),
        data: proto::decode7(enc),
    }))
}

fn be32(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..at + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn be16(d: &[u8], at: usize) -> Option<u16> {
    d.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]))
}

// ---- the project --------------------------------------------------------------

/// One sample pool entry: which +Drive file a slot holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PoolEntry {
    pub inode: u32,
    pub hash: u32,
    /// File size plus 16 bytes of padding.
    pub size: u32,
    pub seqnr: u32,
}

impl PoolEntry {
    /// An empty slot, as the device writes it.
    pub const EMPTY: Self = Self { inode: 0xFFFF_FFFF, hash: 0, size: 0, seqnr: 0 };

    pub fn is_empty(&self) -> bool {
        self.inode == 0xFFFF_FFFF
    }

    /// The key the +Drive knows the file by: hash and sample length.
    pub fn key(&self) -> (u32, u32) {
        (self.hash, self.size.saturating_sub(16))
    }

    /// A +Drive file not in any pool yet. Inode and seqnr 0 mean "unknown":
    /// the device works them out from the hash (elk-herd's notes on
    /// `uniqueSampleFileID_t`).
    pub fn new_file(hash: u32, len: u32) -> Self {
        Self {
            inode: 0,
            hash,
            size: len + 16,
            seqnr: 0,
        }
    }

    fn bytes(&self) -> [u8; 16] {
        let mut b = [0u8; 16];
        for (i, v) in [self.inode, self.hash, self.size, self.seqnr].into_iter().enumerate() {
            b[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
        }
        b
    }
}

#[derive(Clone, Copy)]
enum Enc {
    U8,
    U16,
    /// Digitakt sample lock: slot in the high byte, `0xFFxx` = none.
    LockHi,
    /// Digitakt II sample lock: `0xFFFF` = none.
    Lock16,
}

/// A place in a message that points at a pool slot.
#[derive(Clone, Copy)]
struct SlotRef {
    msg: usize,
    at: usize,
    enc: Enc,
    /// Kit track the reference belongs to (`NO_TRACK` for pool sounds), so a
    /// slot counts once per track per pattern, not once per locked step.
    track: u8,
}

const NO_TRACK: u8 = u8::MAX;

impl SlotRef {
    /// Key for counting: one use per message, track and slot.
    fn use_key(&self, slot: usize) -> (usize, u8, usize) {
        (self.msg, self.track, slot)
    }

    fn get(&self, d: &[u8]) -> Option<usize> {
        match self.enc {
            Enc::U8 => d.get(self.at).map(|&v| v as usize),
            Enc::U16 => be16(d, self.at).map(|v| v as usize),
            Enc::LockHi => be16(d, self.at)
                .filter(|v| v & 0xFF00 != 0xFF00)
                .map(|v| (v >> 8) as usize),
            Enc::Lock16 => be16(d, self.at)
                .filter(|&v| v != 0xFFFF)
                .map(|v| v as usize),
        }
    }

    fn set(&self, d: &mut [u8], slot: usize) {
        match self.enc {
            Enc::U8 => d[self.at] = slot as u8,
            Enc::U16 | Enc::Lock16 => {
                d[self.at..self.at + 2].copy_from_slice(&(slot as u16).to_be_bytes())
            }
            Enc::LockHi => d[self.at] = slot as u8,
        }
    }
}

/// What the parser finds message by message.
#[derive(Default)]
pub(super) struct Scan {
    refs: Vec<SlotRef>,
    problems: Vec<String>,
    pv_seen: std::collections::BTreeSet<u32>,
    snd_seen: std::collections::BTreeSet<u32>,
    unknown: std::collections::BTreeSet<String>,
}

impl Scan {
    /// Finds the slot references in message `mi` and adds one to `uses`
    /// for each (slot 0 aside).
    pub(super) fn count(&mut self, model: Model, mi: usize, m: &DumpMsg, uses: &mut [u32]) {
        let from = self.refs.len();
        self.msg(model, mi, m);
        let mut seen = std::collections::HashSet::new();
        for r in &self.refs[from..] {
            if let Some(s) = r.get(&m.data).filter(|&s| s > 0 && s < uses.len()) {
                if seen.insert(r.use_key(s)) {
                    uses[s] += 1;
                }
            }
        }
    }

    fn msg(&mut self, model: Model, mi: usize, m: &DumpMsg) {
        let d = &m.data;
        match m.kind {
            PATTERN_KIT => {
                let Some(pv) = be32(d, 0) else {
                    self.problems.push(format!("pattern {} too short", m.index));
                    return;
                };
                self.pv_seen.insert(pv);
                let (Some(psize), Some(plocks)) =
                    (pattern_sizeof(model, pv), pattern_plocks(model, pv))
                else {
                    self.unknown.insert(format!("pattern v{pv}"));
                    return;
                };
                let kit = psize;
                let kv_at = if model == DT2 && be32(d, kit) == Some(MAGIC) {
                    kit + 4
                } else {
                    kit
                };
                let kv = be32(d, kv_at).unwrap_or(u32::MAX);
                let (Some(ks), Some(ksize), Some(sv)) = (
                    kit_sounds(model, kv),
                    kit_sizeof(model, kv),
                    kit_sound_version(model, kv),
                ) else {
                    self.unknown.insert(format!("kit v{kv}"));
                    return;
                };
                if kv != pv {
                    self.problems.push(format!(
                        "pattern {} has kit v{kv} but pattern v{pv}",
                        m.index
                    ));
                }
                if d.len() < kit + ksize {
                    self.problems.push(format!("pattern {} shorter than its kit", m.index));
                    return;
                }
                let (Some(slot_at), Some(ssize), Some(param)) = (
                    sound_slot(model, sv),
                    sound_sizeof(model, sv),
                    sample_param(model, sv),
                ) else {
                    self.unknown.insert(format!("sound v{sv}"));
                    return;
                };
                self.snd_seen.insert(sv);
                for t in 0..model.kit_tracks() {
                    let s = kit + ks + t * ssize;
                    if be32(d, s) != Some(MAGIC) || be32(d, s + 4) != Some(sv) {
                        self.problems.push(format!(
                            "pattern {} track {} sound doesn't look right",
                            m.index,
                            t + 1
                        ));
                        continue;
                    }
                    let enc = if model == DT2 { Enc::U16 } else { Enc::U8 };
                    self.refs.push(SlotRef {
                        msg: mi,
                        at: s + slot_at,
                        enc,
                        track: t as u8,
                    });
                }
                let midi_mask = kit_midi_mask(model, kv)
                    .and_then(|o| be16(d, kit + o))
                    .unwrap_or(0);
                let sample_track =
                    |t: usize| t < model.kit_tracks() && midi_mask & (1 << t) == 0;
                let row = 2 + model.steps() * 2;
                for r in 0..PLOCK_ROWS {
                    let at = plocks + r * row;
                    if d.len() < at + row {
                        break;
                    }
                    if d[at] != param || !sample_track(d[at + 1] as usize) {
                        continue;
                    }
                    let enc = if model == DT2 {
                        Enc::Lock16
                    } else {
                        Enc::LockHi
                    };
                    for s in 0..model.steps() {
                        let r = SlotRef {
                            msg: mi,
                            at: at + 2 + s * 2,
                            enc,
                            track: d[at + 1],
                        };
                        if r.get(d).is_some() {
                            self.refs.push(r);
                        }
                    }
                }
            }
            SOUND => {
                let sv = be32(d, 4).unwrap_or(u32::MAX);
                if be32(d, 0) != Some(MAGIC) {
                    self.problems.push(format!("pool sound {} doesn't look right", m.index));
                    return;
                }
                let Some(slot_at) = sound_slot(model, sv) else {
                    self.unknown.insert(format!("sound v{sv}"));
                    return;
                };
                self.snd_seen.insert(sv);
                let enc = if model == DT2 { Enc::U16 } else { Enc::U8 };
                if d.len() >= slot_at + 2 {
                    self.refs.push(SlotRef {
                        msg: mi,
                        at: slot_at,
                        enc,
                        track: NO_TRACK,
                    });
                }
            }
            _ => {}
        }
    }
}
pub struct ElekProject {
    pub model: Model,
    /// The project's own messages, then patterns loaded from files
    /// (`import_pattern`), which are only ever sent placed into a slot.
    msgs: Vec<DumpMsg>,
    /// How many of `msgs` are the project's own.
    base: usize,
    /// Loaded from this file rather than read from the device: a push sends
    /// every message, not just the changed ones.
    pub from_file: Option<std::path::PathBuf>,
    settings_msg: usize,
    pool_at: usize,
    pub pool: Vec<PoolEntry>,
    refs: Vec<SlotRef>,
    /// Uses of each slot: once per pattern track that plays it (kit sound or
    /// sample locks), plus once per pool sound.
    pub uses: Vec<u32>,
    /// Struct versions found, for display.
    pub versions: String,
    /// Why pushing is not allowed, if it isn't.
    pub read_only: Option<String>,
    /// Not every pattern is here: still coming in, or the read was stopped.
    /// Pool edits can't be pushed (they touch every pattern) and use
    /// counts are short; pattern edits in fully loaded banks can.
    pub partial: bool,
    /// The 128 patterns for the project editor, or why they can't be shown.
    pub patterns: Result<Box<Patterns>, String>,
}

/// The pool from a project settings struct: where it starts, and its entries.
fn read_pool(model: Model, sd: &[u8]) -> Result<(u32, usize, Vec<PoolEntry>), String> {
    let sv = be32(sd, 0).ok_or("project settings too short")?;
    let pool_at = settings_samples(model, sv).ok_or_else(|| {
        format!("unknown project settings version {sv} (firmware newer than elk-herd's tables?)")
    })?;
    let pool: Vec<PoolEntry> = (0..model.pool_size())
        .map(|i| {
            let at = pool_at + i * 16;
            Some(PoolEntry {
                inode: be32(sd, at)?,
                hash: be32(sd, at + 4)?,
                size: be32(sd, at + 8)?,
                seqnr: be32(sd, at + 12)?,
            })
        })
        .collect::<Option<_>>()
        .ok_or("project settings too short for the sample pool")?;
    Ok((sv, pool_at, pool))
}

impl ElekProject {
    /// `stopped`: a read that was stopped early, so some patterns may be
    /// missing (the result is `partial`).
    pub(super) fn parse(model: Model, msgs: Vec<DumpMsg>, stopped: bool) -> Result<Self, String> {
        let patterns = msgs.iter().filter(|m| m.kind == PATTERN_KIT).count();
        let partial = stopped && patterns < 128;
        let settings_msg = msgs
            .iter()
            .position(|m| m.kind == SETTINGS)
            .ok_or("no project settings in the dump")?;
        let mut problems: Vec<String> = Vec::new();
        if patterns != 128 && !partial {
            problems.push(format!("got {patterns} patterns, expected 128"));
        }
        // Our encoder must reproduce every message exactly, or edits can't be trusted.
        if let Some(m) = msgs
            .iter()
            .find(|m| build_dump(model, m.kind, m.index, &m.data) != m.raw)
        {
            problems.push(format!(
                "message 0x{:02x} #{} doesn't round-trip",
                m.kind, m.index
            ));
        }

        let (sv, pool_at, pool) = read_pool(model, &msgs[settings_msg].data)?;
        let n = pool.len();

        let mut scan = Scan::default();
        for (mi, m) in msgs.iter().enumerate() {
            scan.msg(model, mi, m);
        }
        let Scan { refs, problems: found, pv_seen, snd_seen, unknown } = scan;
        problems.extend(found);
        if !unknown.is_empty() {
            problems.push(format!(
                "unknown struct versions: {}",
                unknown.into_iter().collect::<Vec<_>>().join(", ")
            ));
        }

        let mut uses = vec![0u32; n];
        let mut seen = std::collections::HashSet::new();
        for r in &refs {
            match r.get(&msgs[r.msg].data) {
                Some(s) if s < n => {
                    if seen.insert(r.use_key(s)) {
                        uses[s] += 1;
                    }
                }
                Some(s) => {
                    problems.push(format!("a reference points at slot {s}, past the pool"));
                    break;
                }
                None => {}
            }
        }
        // Slot 0 is "off"; kits point at it all the time.
        uses[0] = 0;

        let list = |s: &std::collections::BTreeSet<u32>| {
            s.iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join("/")
        };
        let versions = format!(
            "pattern v{} · sound v{} · settings v{sv}",
            list(&pv_seen),
            list(&snd_seen)
        );
        let read_only = (!problems.is_empty()).then(|| problems.join("; "));
        let patterns = if read_only.is_some() {
            Err("the dump doesn't match the layouts we know".to_owned())
        } else {
            Patterns::parse(model, &msgs, !partial, false).map(Box::new)
        };
        if let Err(e) = &patterns {
            crate::log::line(format!("elek-pat: project editor unavailable: {e}"));
        }
        Ok(Self {
            model,
            base: msgs.len(),
            from_file: None,
            msgs,
            settings_msg,
            pool_at,
            pool,
            refs,
            uses,
            versions,
            read_only,
            partial,
            patterns,
        })
    }

    /// A pattern that arrived during the read, and the patterns re-parsed
    /// with it (by the worker, over the settings then the patterns in
    /// arrival order: the order they're kept here).
    pub fn add_streamed(&mut self, msg: DumpMsg, pats: Result<Box<Patterns>, String>) {
        if !self.partial {
            return;
        }
        self.msgs.push(msg);
        self.base = self.msgs.len();
        self.patterns = pats;
    }

    /// Just the pool, from a settings message read ahead of the full dump.
    /// Not pushable; counts start at zero and fill in as patterns arrive.
    pub(super) fn partial(model: Model, settings: DumpMsg) -> Result<Self, String> {
        let (sv, pool_at, pool) = read_pool(model, &settings.data)?;
        let n = pool.len();
        Ok(Self {
            model,
            base: 1,
            from_file: None,
            msgs: vec![settings],
            settings_msg: 0,
            pool_at,
            pool,
            refs: Vec::new(),
            uses: vec![0; n],
            versions: format!("settings v{sv}"),
            read_only: Some("still reading the patterns".into()),
            partial: true,
            patterns: Err("still reading the patterns".into()),
        })
    }

    pub fn msg_count(&self) -> usize {
        self.base
    }

    /// The project settings struct as read, to check the device still has it.
    pub fn settings_data(&self) -> &[u8] {
        &self.msgs[self.settings_msg].data
    }

    /// The messages to send for the pool laid out as `order`, where
    /// `order[new_slot] = old_slot`, and the patterns laid out as `cells`
    /// (empty = as read): only those whose contents change, in dump order
    /// (settings last). Untouched patterns, kits and sounds are left alone on
    /// the device, so edits made there since the read survive.
    /// `fills` are new files put into slots that were empty, by old slot.
    pub fn build_push(&self, order: &[u16], fills: &std::collections::HashMap<u16, PoolEntry>, cells: &[Cell]) -> Vec<Vec<u8>> {
        let all = self.from_file.is_some();
        self.new_data(order, fills, cells)
            .into_iter()
            .take(self.base)
            .enumerate()
            .filter_map(|(i, d)| {
                let m = &self.msgs[i];
                if all {
                    return Some(build_dump(self.model, m.kind, m.index, d.as_ref().unwrap_or(&m.data)));
                }
                let d = d.filter(|d| *d != m.data)?;
                Some(build_dump(self.model, m.kind, m.index, &d))
            })
            .collect()
    }

    /// Every pattern payload (by slot) the device should hold after the push,
    /// to compare with the read-back.
    /// `None` for slots whose pattern never arrived (a stopped read).
    pub fn expected_patterns(&self, order: &[u16], fills: &std::collections::HashMap<u16, PoolEntry>, cells: &[Cell]) -> Vec<Option<Vec<u8>>> {
        let Ok(pats) = &self.patterns else { return Vec::new() };
        let data = self.new_data(order, fills, cells);
        (0..patterns::PATTERNS)
            .map(|i| {
                let mi = pats.msg_of(i)?;
                Some(data[mi].clone().unwrap_or_else(|| self.msgs[mi].data.clone()))
            })
            .collect()
    }

    /// Every one of the project's own messages as it would be after a push:
    /// `(kind, index, payload)`, in dump order.
    pub fn edited_msgs(&self, order: &[u16], fills: &std::collections::HashMap<u16, PoolEntry>, cells: &[Cell]) -> Vec<(u8, u8, Vec<u8>)> {
        self.new_data(order, fills, cells)
            .into_iter()
            .take(self.base)
            .zip(&self.msgs)
            .map(|(d, m)| (m.kind, m.index, d.unwrap_or_else(|| m.data.clone())))
            .collect()
    }

    /// A project from payloads saved earlier (see `file`).
    pub(super) fn from_payloads(model: Model, msgs: Vec<(u8, u8, Vec<u8>)>) -> Result<Self, String> {
        let msgs = msgs
            .into_iter()
            .map(|(kind, index, data)| DumpMsg { kind, index, raw: build_dump(model, kind, index, &data), data })
            .collect();
        Self::parse(model, msgs, false)
    }

    /// Pool slots a pattern payload points at (slot 0, "off", left out).
    pub fn payload_slots(&self, data: &[u8]) -> Result<std::collections::BTreeSet<usize>, String> {
        let refs = self.payload_refs(0, data)?;
        Ok(refs.iter().filter_map(|r| r.get(data)).filter(|&s| s > 0).collect())
    }

    fn payload_refs(&self, mi: usize, data: &[u8]) -> Result<Vec<SlotRef>, String> {
        let m = DumpMsg { kind: PATTERN_KIT, index: 0, raw: Vec::new(), data: data.to_vec() };
        let mut scan = Scan::default();
        scan.msg(self.model, mi, &m);
        if let Some(u) = scan.unknown.iter().next() {
            return Err(format!("unknown struct version ({u})"));
        }
        if let Some(p) = scan.problems.first() {
            return Err(p.clone());
        }
        Ok(scan.refs)
    }

    /// Take in a pattern payload from a file, with its sample slots moved by
    /// `map` (slot in the file → slot as read here; unmapped ones become 0,
    /// "off"). Returns the source index to put in a `Cell`.
    pub fn import_pattern(&mut self, mut data: Vec<u8>, map: &std::collections::HashMap<usize, usize>) -> Result<u8, String> {
        if self.partial {
            return Err("not every pattern has been read; load the whole project first".into());
        }
        let mi = self.msgs.len();
        let refs = self.payload_refs(mi, &data)?;
        let orig = data.clone();
        for r in &refs {
            if let Some(s) = r.get(&orig).filter(|&s| s > 0) {
                r.set(&mut data, map.get(&s).copied().unwrap_or(0));
            }
        }
        let pats = self.patterns.as_mut().map_err(|e| e.clone())?;
        let idx = pats.add(mi, &data)?;
        self.msgs.push(DumpMsg { kind: PATTERN_KIT, index: u8::MAX, raw: Vec::new(), data });
        self.refs.extend(refs);
        Ok(idx)
    }

    /// The pattern payloads as read, by slot (empty if not parsed; `None`
    /// for ones that never arrived).
    pub fn pattern_data(&self) -> Vec<Option<&[u8]>> {
        let Ok(pats) = &self.patterns else { return Vec::new() };
        (0..patterns::PATTERNS).map(|i| pats.msg_of(i).map(|mi| &self.msgs[mi].data[..])).collect()
    }

    /// New payload per message (`None` = untouched).
    fn new_data(&self, order: &[u16], fills: &std::collections::HashMap<u16, PoolEntry>, cells: &[Cell]) -> Vec<Option<Vec<u8>>> {
        let n = self.pool.len();
        let mut new_of = vec![0usize; n];
        for (new, &old) in order.iter().enumerate() {
            new_of[old as usize] = new;
        }
        let mut data: Vec<Option<Vec<u8>>> = vec![None; self.msgs.len()];
        for r in &self.refs {
            let src = &self.msgs[r.msg].data;
            let Some(old) = r.get(src).filter(|&s| s < n) else {
                continue;
            };
            let new = new_of[old];
            if new != old {
                let d = data[r.msg].get_or_insert_with(|| src.clone());
                r.set(d, new);
            }
        }
        if !fills.is_empty() || order.iter().enumerate().any(|(i, &o)| i != o as usize) {
            let d = data[self.settings_msg]
                .get_or_insert_with(|| self.msgs[self.settings_msg].data.clone());
            for (new, &old) in order.iter().enumerate() {
                let dst = self.pool_at + new * 16;
                match fills.get(&old) {
                    Some(e) => d[dst..dst + 16].copy_from_slice(&e.bytes()),
                    None => {
                        let src = self.pool_at + old as usize * 16;
                        d[dst..dst + 16].copy_from_slice(&self.msgs[self.settings_msg].data[src..src + 16]);
                    }
                }
            }
        }
        // Patterns: built from the pool-remapped payloads, then placed.
        if let Ok(pats) = &self.patterns {
            if cells.len() == patterns::PATTERNS {
                // Only slots (and sources) that arrived: the view keeps
                // edits out of banks that didn't load.
                let src = |s: usize| {
                    let mi = pats.msg_of(s).expect("source pattern loaded");
                    data[mi].clone().unwrap_or_else(|| self.msgs[mi].data.clone())
                };
                let placed: Vec<(usize, Vec<u8>)> = cells
                    .iter()
                    .enumerate()
                    .filter(|(i, c)| **c != Cell::as_read(*i))
                    .filter(|(_, c)| c.src.is_none_or(|s| pats.loaded[s as usize]))
                    .filter_map(|(i, c)| Some((pats.msg_of(i)?, pats.build(i, c, src)?)))
                    .collect();
                for (mi, d) in placed {
                    data[mi] = Some(d);
                }
            }
        }
        data
    }
}

/// A project slot on the +Drive.
#[derive(Clone, Debug)]
pub struct SavedProject {
    pub slot: u32,
    pub name: String,
    pub size: u32,
}

pub enum ElekCmd {
    ListProjects,
    /// Read the project; `known` names needn't be looked up again. `ctl`
    /// steers it while it runs (which banks first, stop).
    Read {
        known: std::collections::HashSet<(u32, u32)>,
        ctl: std::sync::Arc<ReadCtl>,
    },
    /// Which of `lens` the +Drive knows file `hash` by (asked in order),
    /// for filling pool slot `slot` (old numbering).
    Resolve {
        slot: u16,
        hash: u32,
        lens: Vec<u32>,
    },
    /// Messages to send, and the settings struct they were built from.
    /// `force` skips the check that the device still has what we read.
    Push {
        msgs: Vec<Vec<u8>>,
        settings: Vec<u8>,
        force: bool,
        /// The model the messages were built for.
        model: Model,
    },
}

/// Shared with the view while a read runs.
#[derive(Default)]
pub struct ReadCtl {
    /// Banks folded out in the pattern view (bit 0 = A): read first.
    pub open: std::sync::atomic::AtomicU8,
    /// Stop after the pattern on its way; what arrived stays usable.
    pub stop: std::sync::atomic::AtomicBool,
}

impl ReadCtl {
    fn stopped(&self) -> bool {
        self.stop.load(std::sync::atomic::Ordering::Relaxed)
    }
}

pub enum ElekEv {
    Projects(Result<Vec<SavedProject>, String>),
    Progress {
        label: String,
        done: usize,
        total: usize,
    },
    /// The pool, before the patterns: shown while the rest streams in.
    Partial(Box<ElekProject>),
    /// Use counts so far, as patterns arrive.
    Uses(Vec<u32>),
    /// One more pattern for the early project, and the patterns parsed so far.
    Pattern {
        msg: DumpMsg,
        pats: Result<Box<Patterns>, String>,
    },
    Loaded(Result<Box<ElekProject>, String>),
    /// `(hash, length) → +Drive path` for pool entries.
    Names(Vec<((u32, u32), String)>),
    Pushed(Result<usize, String>),
    /// Push not sent: the device's project changed since the read.
    PushStale,
    /// Answer to `Resolve`: the length that matched and the +Drive path.
    Resolved {
        slot: u16,
        hash: u32,
        found: Result<Option<(u32, String)>, String>,
    },
}

// ---- talking to the device ----------------------------------------------------

/// Largest piece handed to CoreMIDI at once (a packet holds under 64 KB).
const SEND_PIECE: usize = 4096;
const FIRST_WAIT: Duration = Duration::from_secs(10);
const GAP_WAIT: Duration = Duration::from_secs(8);

impl Conn {
    pub(super) fn model(&self) -> R<Model> {
        Model::from_product(self.product).map_or_else(
            || refused(format!("{} has no projects we can read", self.name)),
            Ok,
        )
    }

    fn send_sysex(&mut self, msg: &[u8]) -> R<()> {
        for piece in msg.chunks(SEND_PIECE) {
            self.out
                .send(piece)
                .map_err(|e| Fail::Link(format!("MIDI send failed: {e}")))?;
        }
        Ok(())
    }

    /// Dump messages until one of `last` kind arrives.
    fn collect_dump(
        &mut self,
        m: Model,
        last: u8,
        on_msg: &mut dyn FnMut(usize, &DumpMsg) -> bool,
    ) -> R<Vec<DumpMsg>> {
        let mut out = Vec::new();
        let mut wait = FIRST_WAIT;
        let mut ignored = 0usize;
        loop {
            let raw = self.rx.recv_timeout(wait).map_err(|_| {
                let e = if out.is_empty() {
                    "the device didn't answer the project dump request. Is it set to receive SysEx over USB?".to_owned()
                } else {
                    format!("the project dump stopped after {} parts", out.len())
                };
                crate::log::line(format!("elek: {e} ({ignored} other SysEx messages ignored)"));
                Fail::Refused(e)
            })?;
            wait = GAP_WAIT;
            let msg = match parse_dump(m, &raw) {
                Ok(Some(msg)) => msg,
                Ok(None) => {
                    ignored += 1;
                    if ignored <= 5 {
                        let head: Vec<u8> = raw.iter().take(12).copied().collect();
                        crate::log::line(format!(
                            "elek: ignored SysEx, {} bytes, starts {head:02x?}",
                            raw.len()
                        ));
                    }
                    continue;
                }
                Err(e) => {
                    crate::log::line(format!("elek: {e}"));
                    return refused(e);
                }
            };
            if ![PATTERN_KIT, SOUND, SETTINGS].contains(&msg.kind) {
                continue;
            }
            let kind = msg.kind;
            let more = on_msg(out.len(), &msg);
            out.push(msg);
            // Stopped: the device sends the rest anyway; later requests
            // empty the input first.
            if kind == last || !more {
                return Ok(out);
            }
        }
    }

    pub(super) fn read_project(
        &mut self,
        on_msg: &mut dyn FnMut(usize, &DumpMsg) -> bool,
    ) -> R<(Model, Vec<DumpMsg>)> {
        let m = self.model()?;
        while self.rx.try_recv().is_ok() {}
        let req = build_dump(m, REQ_PROJECT, 0, &[]);
        crate::log::line(format!(
            "elek: requesting project dump from {} ({req:02x?})",
            m.name()
        ));
        self.send_sysex(&req)?;
        let started = Instant::now();
        let msgs = self.collect_dump(m, SETTINGS, on_msg)?;
        let bytes: usize = msgs.iter().map(|m| m.raw.len()).sum();
        crate::log::line(format!(
            "elek: got {} dump messages, {} KB in {:.1}s",
            msgs.len(),
            bytes / 1024,
            started.elapsed().as_secs_f32()
        ));
        Ok((m, msgs))
    }

    pub(super) fn read_settings_msg(&mut self, m: Model) -> R<DumpMsg> {
        while self.rx.try_recv().is_ok() {}
        self.send_sysex(&build_dump(m, REQ_SETTINGS, 0, &[]))?;
        let msgs = self.collect_dump(m, SETTINGS, &mut |_, _| true)?;
        msgs.into_iter().last().map_or_else(|| refused("no project settings came back"), Ok)
    }

    /// Pattern `i` (with its kit) on its own. `Ok(None)`: nothing within `wait`.
    pub(super) fn read_pattern(&mut self, m: Model, i: u8, wait: Duration) -> R<Option<DumpMsg>> {
        while self.rx.try_recv().is_ok() {}
        self.send_sysex(&build_dump(m, REQ_PATTERN, i, &[]))?;
        let until = Instant::now() + wait;
        loop {
            let Ok(raw) = self.rx.recv_timeout(until.saturating_duration_since(Instant::now())) else {
                return Ok(None);
            };
            match parse_dump(m, &raw) {
                Ok(Some(msg)) if msg.kind == PATTERN_KIT && msg.index == i => return Ok(Some(msg)),
                Ok(_) => {}
                Err(e) => {
                    crate::log::line(format!("elek: {e}"));
                    return refused(e);
                }
            }
        }
    }

    fn read_settings(&mut self, m: Model) -> R<Vec<u8>> {
        Ok(self.read_settings_msg(m)?.data)
    }

    pub(super) fn list_projects(&mut self) -> R<Vec<SavedProject>> {
        let args = Args::default().str("/projects").u32(0).u32(0).bytes(&[1]);
        let r = self.request(proto::DATA_LIST, args)?;
        if r.first() != Some(&1) || r.len() < 13 {
            return refused("the device wouldn't list /projects");
        }
        let mut rd = Reader::new(&r[13..]);
        let mut out = Vec::new();
        let mut next = 1;
        let bad = |e: String| Fail::Refused(format!("odd project listing: {e}"));
        while !rd.is_empty() {
            let name = rd.str().map_err(bad)?;
            let _children = rd.u8().map_err(bad)?;
            match rd.u8().map_err(bad)? {
                1 => {
                    rd.u32().map_err(bad)?;
                }
                2 => {
                    let id = rd.u32().map_err(bad)?;
                    let size = rd.u32().map_err(bad)?;
                    rd.u8().map_err(bad)?;
                    rd.u8().map_err(bad)?; // operations: u16
                    let _valid = rd.u8().map_err(bad)?;
                    let _meta = rd.u8().map_err(bad)?;
                    let slot = if id == 0 || id == u32::MAX { next } else { id };
                    next = slot + 1;
                    out.push(SavedProject { slot, name, size });
                }
                _ => break,
            }
        }
        Ok(out)
    }

    /// The +Drive path of the file with this hash and length, if there is one.
    pub(super) fn sample_path(&mut self, hash: u32, len: u32) -> R<Option<String>> {
        let r = self.request(proto::SAMPLE_INFO, Args::default().u32(hash).u32(len))?;
        let mut rd = Reader::new(&r);
        if !rd.bool().unwrap_or(false) {
            return Ok(None);
        }
        // Then the length and hash asked about (in that order), then the path.
        let _ = rd.u32();
        let _ = rd.u32();
        Ok(rd.str().ok().filter(|s| !s.is_empty()))
    }

    /// Check the device still has the project we read (unless `force`),
    /// then send `msgs` paced so it keeps up. `on_sent(n)` after each.
    /// `Ok(false)`: nothing sent, the device's project changed.
    pub(super) fn push_project(
        &mut self,
        msgs: &[Vec<u8>],
        settings: &[u8],
        force: bool,
        want: Model,
        on_sent: &mut dyn FnMut(usize),
    ) -> R<bool> {
        let m = self.model()?;
        if m != want {
            return refused(format!("this project is for the {}, but the device is a {}", want.name(), m.name()));
        }
        if !force && self.read_settings(m)? != settings {
            return Ok(false);
        }
        for (i, msg) in msgs.iter().enumerate() {
            self.send_sysex(msg)?;
            std::thread::sleep(Duration::from_millis(
                msg.len() as u64 / m.bytes_per_ms() + 20,
            ));
            on_sent(i + 1);
        }
        Ok(true)
    }
}

impl Worker {
    pub(super) fn elek(&mut self, cmd: ElekCmd) {
        let em = self.em.clone();
        let prog = |label: &str, done: usize, total: usize| {
            em.send(Ev::Elek(ElekEv::Progress {
                label: label.to_owned(),
                done,
                total,
            }));
        };
        match cmd {
            ElekCmd::ListProjects => {
                let r = self.with_conn(|c| c.list_projects());
                self.em.send(Ev::Elek(ElekEv::Projects(r)));
            }
            ElekCmd::Read { known, ctl } => {
                let mut asked = known;
                prog("Reading sample pool", 0, 129);
                // The pool comes last in the dump, so ask for it on its own
                // first: the list shows right away, counts fill in after.
                let early = self.with_conn(|c| {
                    let m = c.model()?;
                    Ok((m, c.read_settings_msg(m)?))
                });
                let mut st = match early.map(|(m, s)| (m, ElekProject::partial(m, s.clone()), s)) {
                    Ok((m, Ok(p), s)) => {
                        let keys = pool_keys(&p.pool, &asked);
                        let uses = vec![0u32; p.pool.len()];
                        em.send(Ev::Elek(ElekEv::Partial(Box::new(p))));
                        // Names before the patterns: a quick request each,
                        // where the patterns take half a minute.
                        self.look_up_names(&keys, &mut asked, true);
                        Stream { m: Some(m), msgs: vec![s], live: true, scan: Scan::default(), uses }
                    }
                    Ok((_, Err(e), _)) | Err(e) => {
                        crate::log::line(format!("elek: early pool read skipped: {e}"));
                        Stream { m: None, msgs: Vec::new(), live: false, scan: Scan::default(), uses: Vec::new() }
                    }
                };
                let started = Instant::now();
                let r = self.read_patterns(&mut st, &ctl);
                let stopped = ctl.stopped();
                // Without the early settings (or a whole dump), ask for them now.
                let r = r.and_then(|()| {
                    if st.settings().is_none() {
                        let s = self.with_conn(|c| {
                            let m = c.model()?;
                            Ok((m, c.read_settings_msg(m)?))
                        })?;
                        st.m = Some(s.0);
                        st.msgs.push(s.1);
                    }
                    st.finish()
                });
                let r = r.and_then(|(m, msgs)| ElekProject::parse(m, msgs, stopped).map(Box::new));
                match &r {
                    Ok(p) => crate::log::line(format!(
                        "elek: parsed {}, {} references in {:.1}s{}{}",
                        p.versions,
                        p.uses.iter().sum::<u32>(),
                        started.elapsed().as_secs_f32(),
                        if p.partial { ", STOPPED early" } else { "" },
                        p.read_only
                            .as_ref()
                            .map(|w| format!(", READ-ONLY: {w}"))
                            .unwrap_or_default()
                    )),
                    Err(e) => crate::log::line(format!("elek: read failed: {e}")),
                }
                let keys = r.as_ref().map(|p| pool_keys(&p.pool, &asked)).unwrap_or_default();
                self.em.send(Ev::Elek(ElekEv::Loaded(r)));
                // After `Loaded`: no progress, nothing would clear it.
                self.look_up_names(&keys, &mut asked, false);
            }
            ElekCmd::Resolve { slot, hash, lens } => {
                let found = self.with_conn(|c| {
                    for &len in &lens {
                        if let Some(p) = c.sample_path(hash, len)? {
                            return Ok(Some((len, p)));
                        }
                    }
                    Ok(None)
                });
                crate::log::line(format!("elek: resolve {hash:08x} lens {lens:?} -> {found:?}"));
                self.em.send(Ev::Elek(ElekEv::Resolved { slot, hash, found }));
            }
            ElekCmd::Push { msgs, settings, force, model } => {
                let total = msgs.len();
                prog("Checking project", 0, total);
                let r = self.with_conn(|c| {
                    c.push_project(&msgs, &settings, force, model, &mut |n| {
                        prog("Sending to device", n, total)
                    })
                });
                if let Ok(false) = r {
                    crate::log::line("elek: push held back: the device's project changed since the read");
                    self.em.send(Ev::Elek(ElekEv::PushStale));
                    return;
                }
                if r.is_ok() {
                    // The device keeps writing after the last message lands.
                    prog("Waiting for the device", total, total);
                    std::thread::sleep(Duration::from_secs(3));
                }
                if let Err(e) = &r {
                    crate::log::line(format!("elek: push failed: {e}"));
                }
                self.em.send(Ev::Elek(ElekEv::Pushed(r.map(|_| total))));
            }
        }
    }
}

/// The patterns of a read as they arrive. With the early settings (`live`),
/// each one goes to the view straight away, with use counts and the
/// patterns re-parsed so far.
struct Stream {
    m: Option<Model>,
    /// The early settings first when `live`, then patterns in arrival order:
    /// the order the view's early project keeps them in.
    msgs: Vec<DumpMsg>,
    live: bool,
    scan: Scan,
    uses: Vec<u32>,
}

impl Stream {
    fn have(&self, i: usize) -> bool {
        self.msgs.iter().any(|m| m.kind == PATTERN_KIT && m.index as usize == i)
    }

    fn settings(&self) -> Option<&DumpMsg> {
        self.msgs.iter().rev().find(|m| m.kind == SETTINGS)
    }

    fn add(&mut self, em: &Emitter, msg: DumpMsg) {
        if msg.kind != PATTERN_KIT {
            // The whole dump's own settings, at its end.
            self.msgs.push(msg);
            return;
        }
        if self.have(msg.index as usize) {
            return;
        }
        let mi = self.msgs.len();
        self.msgs.push(msg);
        let (true, Some(m)) = (self.live, self.m) else { return };
        let before: u32 = self.uses.iter().sum();
        self.scan.count(m, mi, &self.msgs[mi], &mut self.uses);
        if self.uses.iter().sum::<u32>() != before {
            em.send(Ev::Elek(ElekEv::Uses(self.uses.clone())));
        }
        let pats = Patterns::parse(m, &self.msgs, false, true).map(Box::new);
        em.send(Ev::Elek(ElekEv::Pattern { msg: self.msgs[mi].clone(), pats }));
    }

    /// Everything read, in dump order: patterns by slot, settings last.
    fn finish(self) -> Result<(Model, Vec<DumpMsg>), String> {
        let m = self.m.ok_or("no device model")?;
        let settings = self.settings().cloned().ok_or("no project settings came back")?;
        let mut msgs: Vec<DumpMsg> = self.msgs.into_iter().filter(|m| m.kind == PATTERN_KIT).collect();
        msgs.sort_by_key(|m| m.index);
        msgs.push(settings);
        Ok((m, msgs))
    }
}

/// How long the first single-pattern request may take before falling back
/// to the whole-project dump.
const FIRST_PATTERN_WAIT: Duration = Duration::from_secs(4);

impl Worker {
    /// The 128 patterns one at a time, banks folded out in the view first,
    /// until done or stopped. Falls back to the whole-project dump (slot
    /// order) if the device doesn't answer single-pattern requests.
    fn read_patterns(&mut self, st: &mut Stream, ctl: &ReadCtl) -> Result<(), String> {
        let em = self.em.clone();
        let m = match st.m {
            Some(m) => m,
            None => {
                let m = self.with_conn(|c| c.model())?;
                st.m = Some(m);
                m
            }
        };
        let prog = |done: usize| {
            em.send(Ev::Elek(ElekEv::Progress { label: "Reading patterns".into(), done, total: patterns::PATTERNS }));
        };
        let mut done = 0;
        prog(0);
        while !ctl.stopped() {
            let open = ctl.open.load(std::sync::atomic::Ordering::Relaxed);
            let Some(i) = (0..patterns::PATTERNS).filter(|&i| !st.have(i)).min_by_key(|&i| (open & (1 << (i / 16)) == 0, i)) else {
                return Ok(());
            };
            let wait = if done == 0 { FIRST_PATTERN_WAIT } else { GAP_WAIT };
            let mut got = self.with_conn(|c| c.read_pattern(m, i as u8, wait))?;
            if got.is_none() && done > 0 {
                crate::log::line(format!("elek: pattern {} didn't arrive, asking again", patterns::label(i)));
                got = self.with_conn(|c| c.read_pattern(m, i as u8, GAP_WAIT))?;
            }
            let Some(msg) = got else {
                if done > 0 {
                    return Err(format!("pattern {} didn't arrive", patterns::label(i)));
                }
                crate::log::line("elek: no answer to a single-pattern request; reading the whole project instead");
                return self.read_whole(st, ctl);
            };
            st.add(&em, msg);
            done += 1;
            prog(done);
            // Previews clicked meanwhile go between patterns.
            self.serve_pending();
        }
        crate::log::line(format!("elek: read stopped after {done} of {} patterns", patterns::PATTERNS));
        Ok(())
    }

    /// The whole-project dump, streamed in slot order.
    fn read_whole(&mut self, st: &mut Stream, ctl: &ReadCtl) -> Result<(), String> {
        let em = self.em.clone();
        let (m, _) = self.with_conn(|c| {
            c.read_project(&mut |i, msg| {
                st.add(&em, msg.clone());
                em.send(Ev::Elek(ElekEv::Progress { label: "Reading project".into(), done: i + 1, total: 129.max(i + 2) }));
                !ctl.stopped()
            })
        })?;
        st.m = Some(m);
        Ok(())
    }
}

/// Distinct files in the pool not in `skip`.
fn pool_keys(pool: &[PoolEntry], skip: &std::collections::HashSet<(u32, u32)>) -> Vec<(u32, u32)> {
    let mut k: Vec<_> = pool.iter().skip(1).filter(|e| !e.is_empty()).map(|e| e.key()).filter(|k| !skip.contains(k)).collect();
    k.sort();
    k.dedup();
    k
}

impl Worker {
    /// Asks the device for each file's +Drive path, sending them on in small
    /// batches so the list fills in as it goes.
    fn look_up_names(&mut self, keys: &[(u32, u32)], asked: &mut std::collections::HashSet<(u32, u32)>, progress: bool) {
        let mut batch = Vec::new();
        for (i, &(h, l)) in keys.iter().enumerate() {
            match self.with_conn(|c| c.sample_path(h, l)) {
                Ok(Some(p)) => batch.push(((h, l), p)),
                Ok(None) => {}
                Err(_) => break,
            }
            asked.insert((h, l));
            if progress {
                self.em.send(Ev::Elek(ElekEv::Progress { label: "Looking up names".into(), done: i + 1, total: keys.len() }));
            }
            if batch.len() >= 8 || i + 1 == keys.len() {
                self.em.send(Ev::Elek(ElekEv::Names(std::mem::take(&mut batch))));
                self.serve_pending();
            }
        }
        if !batch.is_empty() {
            self.em.send(Ev::Elek(ElekEv::Names(batch)));
        }
    }
}
