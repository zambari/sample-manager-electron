//! The loaded project on a Digitakt / Digitakt II, over the device's own
//! "SysEx Dump" messages: read it, reorder its sample pool, write it back.
//!
//! Struct layouts come from elk-herd (BSD-2); see `project_discovery.md`.
//! Only the working copy in the device's memory is ever written. Saved
//! projects on the +Drive are listed, never touched (see `proto::FORBIDDEN`).

use super::*;

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

    fn kit_tracks(self) -> usize {
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
    pub fn is_empty(&self) -> bool {
        self.inode == 0xFFFF_FFFF
    }

    /// The key the +Drive knows the file by: hash and sample length.
    pub fn key(&self) -> (u32, u32) {
        (self.hash, self.size.saturating_sub(16))
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
}

impl SlotRef {
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
        for r in &self.refs[from..] {
            if let Some(s) = r.get(&m.data).filter(|&s| s > 0 && s < uses.len()) {
                uses[s] += 1;
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
                    });
                }
            }
            _ => {}
        }
    }
}
pub struct ElkProject {
    pub model: Model,
    msgs: Vec<DumpMsg>,
    settings_msg: usize,
    pool_at: usize,
    pub pool: Vec<PoolEntry>,
    refs: Vec<SlotRef>,
    /// References to each slot: kit tracks, sample locks, sound pool.
    pub uses: Vec<u32>,
    /// Struct versions found, for display.
    pub versions: String,
    /// Why pushing is not allowed, if it isn't.
    pub read_only: Option<String>,
    /// Only the pool so far: the patterns are still coming in.
    pub partial: bool,
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

impl ElkProject {
    pub(super) fn parse(model: Model, msgs: Vec<DumpMsg>) -> Result<Self, String> {
        let patterns = msgs.iter().filter(|m| m.kind == PATTERN_KIT).count();
        let settings_msg = msgs
            .iter()
            .position(|m| m.kind == SETTINGS)
            .ok_or("no project settings in the dump")?;
        let mut problems: Vec<String> = Vec::new();
        if patterns != 128 {
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
        for r in &refs {
            match r.get(&msgs[r.msg].data) {
                Some(s) if s < n => uses[s] += 1,
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
        Ok(Self {
            model,
            msgs,
            settings_msg,
            pool_at,
            pool,
            refs,
            uses,
            versions,
            read_only,
            partial: false,
        })
    }

    /// Just the pool, from a settings message read ahead of the full dump.
    /// Not pushable; counts start at zero and fill in as patterns arrive.
    pub(super) fn partial(model: Model, settings: DumpMsg) -> Result<Self, String> {
        let (sv, pool_at, pool) = read_pool(model, &settings.data)?;
        let n = pool.len();
        Ok(Self {
            model,
            msgs: vec![settings],
            settings_msg: 0,
            pool_at,
            pool,
            refs: Vec::new(),
            uses: vec![0; n],
            versions: format!("settings v{sv}"),
            read_only: Some("still reading the patterns".into()),
            partial: true,
        })
    }

    pub fn msg_count(&self) -> usize {
        self.msgs.len()
    }

    /// The project settings struct as read, to check the device still has it.
    pub fn settings_data(&self) -> &[u8] {
        &self.msgs[self.settings_msg].data
    }

    /// The messages to send for the pool laid out as `order`, where
    /// `order[new_slot] = old_slot`: only those whose contents change, in
    /// dump order (settings last). Untouched patterns, kits and sounds are
    /// left alone on the device, so edits made there since the read survive.
    pub fn build_push(&self, order: &[u16]) -> Vec<Vec<u8>> {
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
        if order.iter().enumerate().any(|(i, &o)| i != o as usize) {
            let d = data[self.settings_msg]
                .get_or_insert_with(|| self.msgs[self.settings_msg].data.clone());
            for (new, &old) in order.iter().enumerate() {
                let src = self.pool_at + old as usize * 16;
                let dst = self.pool_at + new * 16;
                d[dst..dst + 16].copy_from_slice(&self.msgs[self.settings_msg].data[src..src + 16]);
            }
        }
        self.msgs
            .iter()
            .zip(data)
            .filter_map(|(m, d)| {
                let d = d.filter(|d| *d != m.data)?;
                Some(build_dump(self.model, m.kind, m.index, &d))
            })
            .collect()
    }
}

/// A project slot on the +Drive.
#[derive(Clone, Debug)]
pub struct SavedProject {
    pub slot: u32,
    pub name: String,
    pub size: u32,
}

pub enum ElkCmd {
    ListProjects,
    /// Read the project; `known` names needn't be looked up again.
    Read {
        known: std::collections::HashSet<(u32, u32)>,
    },
    /// Messages to send, and the settings struct they were built from.
    Push {
        msgs: Vec<Vec<u8>>,
        settings: Vec<u8>,
    },
}

pub enum ElkEv {
    Projects(Result<Vec<SavedProject>, String>),
    Progress {
        label: String,
        done: usize,
        total: usize,
    },
    /// The pool, before the patterns: shown while the rest streams in.
    Partial(Box<ElkProject>),
    /// Use counts so far, as patterns arrive.
    Uses(Vec<u32>),
    Loaded(Result<Box<ElkProject>, String>),
    /// `(hash, length) → +Drive path` for pool entries.
    Names(Vec<((u32, u32), String)>),
    Pushed(Result<usize, String>),
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
        on_msg: &mut dyn FnMut(usize, &DumpMsg),
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
                crate::log::line(format!("elk: {e} ({ignored} other SysEx messages ignored)"));
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
                            "elk: ignored SysEx, {} bytes, starts {head:02x?}",
                            raw.len()
                        ));
                    }
                    continue;
                }
                Err(e) => {
                    crate::log::line(format!("elk: {e}"));
                    return refused(e);
                }
            };
            if ![PATTERN_KIT, SOUND, SETTINGS].contains(&msg.kind) {
                continue;
            }
            let kind = msg.kind;
            on_msg(out.len(), &msg);
            out.push(msg);
            if kind == last {
                return Ok(out);
            }
        }
    }

    pub(super) fn read_project(
        &mut self,
        on_msg: &mut dyn FnMut(usize, &DumpMsg),
    ) -> R<(Model, Vec<DumpMsg>)> {
        let m = self.model()?;
        while self.rx.try_recv().is_ok() {}
        let req = build_dump(m, REQ_PROJECT, 0, &[]);
        crate::log::line(format!(
            "elk: requesting project dump from {} ({req:02x?})",
            m.name()
        ));
        self.send_sysex(&req)?;
        let started = Instant::now();
        let msgs = self.collect_dump(m, SETTINGS, on_msg)?;
        let bytes: usize = msgs.iter().map(|m| m.raw.len()).sum();
        crate::log::line(format!(
            "elk: got {} dump messages, {} KB in {:.1}s",
            msgs.len(),
            bytes / 1024,
            started.elapsed().as_secs_f32()
        ));
        Ok((m, msgs))
    }

    pub(super) fn read_settings_msg(&mut self, m: Model) -> R<DumpMsg> {
        while self.rx.try_recv().is_ok() {}
        self.send_sysex(&build_dump(m, REQ_SETTINGS, 0, &[]))?;
        let msgs = self.collect_dump(m, SETTINGS, &mut |_, _| {})?;
        msgs.into_iter().last().map_or_else(|| refused("no project settings came back"), Ok)
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
        let _ = rd.u32();
        let _ = rd.u32();
        Ok(rd.str().ok().filter(|s| !s.is_empty()))
    }

    /// Check the device still has the project we read, then send `msgs`
    /// paced so it keeps up. `on_sent(n)` after each.
    pub(super) fn push_project(
        &mut self,
        msgs: &[Vec<u8>],
        settings: &[u8],
        on_sent: &mut dyn FnMut(usize),
    ) -> R<()> {
        let m = self.model()?;
        if self.read_settings(m)? != settings {
            return refused(
                "the project on the device changed since it was read. Read it again first.",
            );
        }
        for (i, msg) in msgs.iter().enumerate() {
            self.send_sysex(msg)?;
            std::thread::sleep(Duration::from_millis(
                msg.len() as u64 / m.bytes_per_ms() + 20,
            ));
            on_sent(i + 1);
        }
        Ok(())
    }
}

impl Worker {
    pub(super) fn elk(&mut self, cmd: ElkCmd) {
        let em = self.em.clone();
        let prog = |label: &str, done: usize, total: usize| {
            em.send(Ev::Elk(ElkEv::Progress {
                label: label.to_owned(),
                done,
                total,
            }));
        };
        match cmd {
            ElkCmd::ListProjects => {
                let r = self.with_conn(|c| c.list_projects());
                self.em.send(Ev::Elk(ElkEv::Projects(r)));
            }
            ElkCmd::Read { known } => {
                let mut asked = known;
                prog("Reading sample pool", 0, 129);
                // The pool comes last in the dump, so ask for it on its own
                // first: the list shows right away, counts fill in after.
                let early = self.with_conn(|c| {
                    let m = c.model()?;
                    Ok((m, c.read_settings_msg(m)?))
                });
                let mut live = match early.map(|(m, s)| (m, ElkProject::partial(m, s))) {
                    Ok((m, Ok(p))) => {
                        let uses = vec![0u32; p.pool.len()];
                        let keys = pool_keys(&p.pool, &asked);
                        em.send(Ev::Elk(ElkEv::Partial(Box::new(p))));
                        // Names before the dump: a quick request each, where
                        // the dump takes half a minute.
                        self.look_up_names(&keys, &mut asked, true);
                        Some((m, Scan::default(), uses))
                    }
                    Ok((_, Err(e))) | Err(e) => {
                        crate::log::line(format!("elk: early pool read skipped: {e}"));
                        None
                    }
                };
                let r = self.with_conn(|c| {
                    c.read_project(&mut |i, msg| {
                        if let Some((m, scan, uses)) = &mut live {
                            let before: u32 = uses.iter().sum();
                            scan.count(*m, i, msg, uses);
                            if uses.iter().sum::<u32>() != before {
                                em.send(Ev::Elk(ElkEv::Uses(uses.clone())));
                            }
                        }
                        prog("Reading project", i + 1, 129.max(i + 2))
                    })
                });
                let r = r.and_then(|(m, msgs)| ElkProject::parse(m, msgs).map(Box::new));
                match &r {
                    Ok(p) => crate::log::line(format!(
                        "elk: parsed {}, {} references{}",
                        p.versions,
                        p.uses.iter().sum::<u32>(),
                        p.read_only
                            .as_ref()
                            .map(|w| format!(", READ-ONLY: {w}"))
                            .unwrap_or_default()
                    )),
                    Err(e) => crate::log::line(format!("elk: read failed: {e}")),
                }
                let keys = r.as_ref().map(|p| pool_keys(&p.pool, &asked)).unwrap_or_default();
                self.em.send(Ev::Elk(ElkEv::Loaded(r)));
                // After `Loaded`: no progress, nothing would clear it.
                self.look_up_names(&keys, &mut asked, false);
            }
            ElkCmd::Push { msgs, settings } => {
                let total = msgs.len();
                prog("Checking project", 0, total);
                let r = self.with_conn(|c| {
                    c.push_project(&msgs, &settings, &mut |n| {
                        prog("Sending to device", n, total)
                    })
                });
                if r.is_ok() {
                    // The device keeps writing after the last message lands.
                    prog("Waiting for the device", total, total);
                    std::thread::sleep(Duration::from_secs(3));
                }
                if let Err(e) = &r {
                    crate::log::line(format!("elk: push failed: {e}"));
                }
                self.em.send(Ev::Elk(ElkEv::Pushed(r.map(|_| total))));
            }
        }
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
                self.em.send(Ev::Elk(ElkEv::Progress { label: "Looking up names".into(), done: i + 1, total: keys.len() }));
            }
            if batch.len() >= 8 || i + 1 == keys.len() {
                self.em.send(Ev::Elk(ElkEv::Names(std::mem::take(&mut batch))));
                self.serve_pending();
            }
        }
        if !batch.is_empty() {
            self.em.send(Ev::Elk(ElkEv::Names(batch)));
        }
    }
}
