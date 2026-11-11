//! The 128 patterns of a Digitakt / Digitakt II project: what each slot holds, and
//! moving / copying / clearing / renaming whole patterns, and copying single
//! tracks between patterns (`Cell::tracks`, `Patterns::splice_track`).
//!
//! On the Digitakt each pattern carries its own copy of its kit, and nothing
//! in the project points at a pattern number, so a pattern moves as one
//! `0x50` payload plus its slot byte (`patternStorage_kitIndex`). Sample and
//! sound references are project-wide and stay valid.
//!
//! elk-herd was mostly tested on the Digitakt II, so every assumption made
//! here is checked against the dump and logged (`elek-pat:`) when it doesn't
//! hold. See `project_management_discovery.md` §8.

use super::*;
use std::collections::{BTreeMap, HashMap};

pub const PATTERNS: usize = 128;

/// Kit names are 16 bytes; keep one for a NUL. [unverified] the device's
/// own limit.
pub const NAME_MAX: usize = 15;

/// Patterns differing from an empty one in at most this many bytes are shown
/// as "nearly empty" (elk-herd's phantoms: kit tweaks, no trigs).
const PHANTOM_BYTES: usize = 32;

/// `patternStorage_kitIndex` (elk-herd CppStructs): the pattern's own slot.
fn pattern_slot_byte(m: Model, v: u32) -> Option<usize> {
    Some(match (m, v) {
        (DT, 0) => 22896,
        (DT, 1 | 2) => 24944,
        (DT, 3..=6) => 24976,
        (DT, 7 | 8) => 24992,
        (DT, 9 | 10) => 25008,
        (DT2, 0) => 55984,
        (DT2, 3 | 4) => 88768,
        _ => return None,
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PatState {
    /// Identical to an empty pattern (slot byte aside).
    Blank,
    /// Differs in only a few bytes: `PatternInfo::diff` says how many.
    Phantom,
    Used,
}

#[derive(Clone, Debug)]
pub struct TrackInfo {
    pub name: String,
    /// Pool slot as read (0 = none).
    pub slot: usize,
}

#[derive(Clone, Debug)]
pub struct PatternInfo {
    pub state: PatState,
    /// What the device shows: the pattern name when it's plain text, else
    /// the kit name (patterns from before the DT1 had pattern names).
    pub name: String,
    pub kit_name: String,
    /// The pattern-name field as text (· for non-printable).
    pub pattern_field: String,
    pub tracks: Vec<TrackInfo>,
    /// Sequencer tracks: the audio tracks, then the MIDI tracks. Empty when
    /// the layout isn't known.
    pub seq: Vec<TrackSeq>,
    /// Bytes that differ from an empty pattern.
    pub diff: usize,
    /// Raw bytes after the pattern name (pattern-level settings live around
    /// here: master length, scale…), and the payload offset of the first.
    /// For finding offsets by hand.
    pub tail: Vec<u8>,
    pub tail_at: usize,
    /// Pattern-level length and scale settings, when the layout is known.
    pub timing: Option<Timing>,
}

/// Pattern-level timing (DT1 v9/v10), in the bytes after the pattern name.
/// Similar to the Analog Rytm's pattern struct: tempo ×120, master length
/// (the device's RESET: all tracks restart; 1 = INF), master change (1 = OFF), then at +9 the scale mode (1 = per
/// track) and +10 the master scale. Checked against DT1 v10 dumps.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub tempo_x120: u16,
    pub master_len: u16,
    pub change: u16,
    pub per_track: bool,
    pub scale: u8,
}

impl Timing {
    fn read(t: &[u8]) -> Option<Self> {
        let be = |i: usize| Some(u16::from_be_bytes([*t.get(i)?, *t.get(i + 1)?]));
        Some(Self { tempo_x120: be(2)?, master_len: be(4)?, change: be(6)?, per_track: *t.get(9)? == 1, scale: *t.get(10)? })
    }
}

/// Elektron scale (speed) values, fastest first; 2 = 1x.
pub fn scale_label(s: u8) -> Option<&'static str> {
    ["2x", "3/2x", "1x", "3/4x", "1/2x", "1/4x", "1/8x"].get(s as usize).copied()
}

impl PatternInfo {
    /// A slot whose pattern hasn't arrived.
    fn not_loaded() -> Self {
        Self {
            state: PatState::Blank,
            name: String::new(),
            kit_name: String::new(),
            pattern_field: String::new(),
            tracks: Vec::new(),
            seq: Vec::new(),
            diff: 0,
            tail: Vec::new(),
            tail_at: 0,
            timing: None,
        }
    }
}

/// What's on one sequencer track, for a thumbnail.
#[derive(Clone, Debug)]
pub struct TrackSeq {
    /// Per step: `Step::None`, `Lock` (trig flags but no note) or `Note`.
    pub steps: Vec<Step>,
    /// Track length in steps.
    pub len: usize,
    /// `len` was read from the track, not guessed from the last used step.
    pub len_known: bool,
    /// Track scale (see `scale_label`), when the layout is known.
    pub scale: Option<u8>,
    /// The last `TAIL` bytes of the track struct (track settings: length,
    /// scale… [unverified] which is where), for finding offsets by hand.
    pub tail: Vec<u8>,
    /// Trig flag word per step.
    pub flags: Vec<u16>,
    /// The rest of the track struct after the trig flags, cut into
    /// one-byte-per-step arrays: `(offset in the track, bytes)`. One of them
    /// is likely the note [unverified which].
    pub arrays: Vec<(usize, Vec<u8>)>,
    /// P-lock rows for this track: `(param id, value per step)`, `None` = no lock.
    pub locks: Vec<(u8, Vec<Option<u16>>)>,
    /// The param id of sample locks (value = pool slot).
    pub sample_param: Option<u8>,
}

/// How many bytes at the end of each track struct `TrackSeq::tail` keeps.
pub const TAIL: usize = 32;
/// How many bytes after the pattern name `PatternInfo::tail` keeps.
pub const PAT_TAIL: usize = 48;

impl TrackSeq {
    pub fn notes(&self) -> usize {
        self.steps.iter().take(self.len).filter(|s| **s == Step::Note).count()
    }

    pub fn is_empty(&self) -> bool {
        self.steps.iter().all(|s| *s == Step::None)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    None,
    Lock,
    Note,
}

/// Sequencer tracks in a pattern: audio, then MIDI (DT1); 16 kit tracks (DT2).
const SEQ_TRACKS: usize = 16;

/// `trackStorage_sizeof` for the tracks in pattern `v` (elk-herd). On the
/// DT1 the tracks fill the space before the p-locks exactly, so that's used
/// instead; on DT2 v3/v4 there's a gap after them.
fn track_stride(m: Model, v: u32, plocks: usize) -> usize {
    match (m, v) {
        (DT2, 0) => 2207,
        (DT2, 3 | 4) => 1184,
        _ => plocks.saturating_sub(4) / SEQ_TRACKS,
    }
}

/// A p-lock row (`[param, track, value per step…]`) with no value set.
fn no_locks(model: Model, h: &[u8]) -> bool {
    h[2..].chunks_exact(2).all(|v| {
        let v = u16::from_be_bytes([v[0], v[1]]);
        if model == DT2 { v == 0xFFFF } else { v & 0xFF00 == 0xFF00 }
    })
}

/// Each track starts with one big-endian u16 of trig flags per step, right
/// after the pattern's 4-byte version. From DT1 v10 dumps: bit 0 = note
/// trig; 0x0010 = swing (set on every odd step by default, so ignored); any
/// other bit = something on the step without a note (lock trig) [unverified
/// for which bit is which]. Byte `stride - 6` is the track length in v10.
fn parse_seq(model: Model, pv: u32, d: &[u8], plocks: usize, sample_param: Option<u8>) -> Vec<TrackSeq> {
    let steps = model.steps();
    let body = plocks.saturating_sub(4);
    let stride = track_stride(model, pv, plocks);
    if (model == DT && body % SEQ_TRACKS != 0) || stride < steps * 2 || stride * SEQ_TRACKS > body {
        return Vec::new();
    }
    (0..SEQ_TRACKS)
        .map(|t| {
            let at = 4 + t * stride;
            let flags: Vec<u16> = (0..steps).map(|k| be16(d, at + k * 2).unwrap_or(0)).collect();
            let n_steps = steps;
            let arrays = (0..(stride - n_steps * 2) / n_steps)
                .map(|a| {
                    let o = n_steps * 2 + a * n_steps;
                    (o, d.get(at + o..at + o + n_steps).map(<[u8]>::to_vec).unwrap_or_default())
                })
                .collect();
            let row = 2 + n_steps * 2;
            let locks = (0..PLOCK_ROWS)
                .filter_map(|r| {
                    let ra = plocks + r * row;
                    let h = d.get(ra..ra + row)?;
                    if h[1] as usize != t {
                        return None;
                    }
                    let vals: Vec<Option<u16>> = (0..n_steps)
                        .map(|k| {
                            let v = u16::from_be_bytes([h[2 + k * 2], h[3 + k * 2]]);
                            let none = if model == DT2 { v == 0xFFFF } else { v & 0xFF00 == 0xFF00 };
                            (!none).then_some(v)
                        })
                        .collect();
                    vals.iter().any(Option::is_some).then_some((h[0], vals))
                })
                .collect();
            let steps: Vec<Step> = (0..steps)
                .map(|k| match be16(d, at + k * 2).unwrap_or(0) {
                    w if w & 1 != 0 => Step::Note,
                    w if w & !0x10 != 0 => Step::Lock,
                    _ => Step::None,
                })
                .collect();
            let byte = if model == DT && pv >= 9 { d.get(at + stride - 6).map(|&b| b as usize) } else { None };
            // Two bytes after the length: the track's scale (DT1 v10 dumps).
            let scale = if model == DT && pv >= 9 { d.get(at + stride - 4).copied().filter(|&b| scale_label(b).is_some()) } else { None };
            let (len, len_known) = match byte {
                Some(n) if (1..=steps.len()).contains(&n) => (n, true),
                // Unknown layout: up to the last used step, in whole pages.
                _ => ((steps.iter().rposition(|s| *s != Step::None).map_or(0, |p| p + 1).div_ceil(16) * 16).max(16), false),
            };
            let tail = d.get(at + stride - TAIL.min(stride)..at + stride).map(<[u8]>::to_vec).unwrap_or_default();
            TrackSeq { steps, len, len_known, scale, tail, flags, arrays, locks, sample_param }
        })
        .collect()
}

/// One slot of the edited layout.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Cell {
    /// Pattern (slot as read) that goes here; `None` = an empty pattern.
    pub src: Option<u8>,
    /// New kit name, if renamed.
    pub name: Option<String>,
    /// Tracks taken from another pattern: track → pattern (as `src`; `None`
    /// = an empty pattern's track). Same track number on both sides.
    pub tracks: BTreeMap<u8, Option<u8>>,
}

impl Cell {
    pub fn as_read(slot: usize) -> Self {
        Self { src: Some(slot as u8), name: None, tracks: BTreeMap::new() }
    }

    pub fn empty() -> Self {
        Self { src: None, name: None, tracks: BTreeMap::new() }
    }

    pub fn with_src(src: u8) -> Self {
        Self { src: Some(src), name: None, tracks: BTreeMap::new() }
    }

    /// The pattern track `t` comes from (`None`: an empty pattern).
    pub fn track_src(&self, t: usize) -> Option<u8> {
        self.tracks.get(&(t as u8)).copied().unwrap_or(self.src)
    }

    /// Take track `t` from pattern `from`; an override that matches the
    /// pattern itself is dropped.
    pub fn set_track(&mut self, t: usize, from: Option<u8>) {
        if from == self.src {
            self.tracks.remove(&(t as u8));
        } else {
            self.tracks.insert(t as u8, from);
        }
    }

    /// Every pattern this cell reads from.
    pub fn sources(&self) -> impl Iterator<Item = u8> + '_ {
        self.src.into_iter().chain(self.tracks.values().flatten().copied())
    }
}

/// Where things sit in this project's pattern payloads (all share one layout).
#[derive(Clone)]
struct Geo {
    model: Model,
    pv: u32,
    len: usize,
    kit: usize,
    /// Track sounds: start (from the kit), stride, and slot offset (a u16
    /// on the DT2).
    sounds: Option<usize>,
    ssize: Option<usize>,
    sslot: Option<usize>,
    sample_param: Option<u8>,
    /// P-lock rows start, and one sequencer track's struct size (when it
    /// checks out, see `parse_seq`).
    plocks: Option<usize>,
    stride: Option<usize>,
    /// The kit's MIDI-track bit mask (DT2), from the payload start.
    midi_mask: Option<usize>,
}

pub struct Patterns {
    /// Slots 0..128 as read, then patterns loaded from files (see `add`).
    pub info: Vec<PatternInfo>,
    /// Which of `info` hold a pattern that has arrived (all of them once a
    /// read finishes; a stopped read leaves some out).
    pub loaded: Vec<bool>,
    /// Message index of each slot's `0x50` (`usize::MAX`: not loaded).
    msg: Vec<usize>,
    /// An empty pattern, slot byte 0. `None`: couldn't tell which one.
    blank: Option<Vec<u8>>,
    slot_at: usize,
    /// What every pattern's slot byte holds; `None`: its own slot.
    slot_value: Option<u8>,
    kit_name_at: usize,
    pattern_name_at: usize,
    /// What an unused p-lock row looks like (from the empty pattern, else
    /// the most common row with no locks in it).
    empty_row: Option<Vec<u8>>,
    /// Why moves / renames aren't allowed, if they aren't (names still show).
    pub read_only: Option<String>,
    geo: Geo,
}

fn log(s: String) {
    crate::log::line(format!("elek-pat: {s}"));
}

fn text_at(d: &[u8], at: usize) -> String {
    d.get(at..at + 16)
        .map(|b| {
            b.iter()
                .take_while(|&&c| c != 0)
                .map(|&c| if (0x20..0x7F).contains(&c) { c as char } else { '·' })
                .collect()
        })
        .unwrap_or_default()
}

fn printable(d: &[u8], at: usize) -> bool {
    d.get(at..at + 16).is_some_and(|b| {
        b.iter().take_while(|&&c| c != 0).all(|c| (0x20..0x7F).contains(c))
            && b.iter().skip_while(|&&c| c != 0).all(|&c| c == 0)
    })
}

/// "A01".."H16".
pub fn label(i: usize) -> String {
    format!("{}{:02}", (b'A' + (i / 16) as u8) as char, i % 16 + 1)
}

/// What a typed name becomes on the device: upper case, printable ASCII,
/// at most `NAME_MAX` chars. [unverified] which characters the DT1 shows.
pub fn clean_name(s: &str) -> String {
    s.chars()
        .map(|c| c.to_ascii_uppercase())
        .filter(|c| (' '..='~').contains(c))
        .take(NAME_MAX)
        .collect()
}

/// Names, tracks and steps of one pattern payload.
fn make_info(g: &Geo, kit_name_at: usize, pattern_name_at: usize, state: PatState, diff: usize, d: &[u8]) -> PatternInfo {
    let pf = text_at(d, pattern_name_at);
    let tracks = match (g.sounds, g.ssize, g.sslot) {
        (Some(s0), Some(sz), Some(sl)) => (0..g.model.kit_tracks())
            .map(|t| {
                let s = g.kit + s0 + t * sz;
                let slot = if g.model == DT2 { be16(d, s + sl).map(usize::from) } else { d.get(s + sl).map(|&b| b as usize) };
                TrackInfo { name: text_at(d, s + 12), slot: slot.unwrap_or(0) }
            })
            .collect(),
        _ => Vec::new(),
    };
    let seq = pattern_plocks(g.model, g.pv).map_or_else(Vec::new, |pl| parse_seq(g.model, g.pv, d, pl, g.sample_param));
    let kit_name = text_at(d, kit_name_at);
    let name = if !pf.is_empty() && printable(d, pattern_name_at) { pf.clone() } else { kit_name.clone() };
    let tail_at = pattern_name_at + 16;
    let tail = d.get(tail_at..(tail_at + PAT_TAIL).min(d.len())).map(<[u8]>::to_vec).unwrap_or_default();
    let timing = (g.model == DT && g.pv >= 9).then(|| Timing::read(&tail)).flatten().filter(|t| scale_label(t.scale).is_some());
    PatternInfo { state, name, kit_name, pattern_field: pf, tracks, seq, diff, tail, tail_at, timing }
}

impl Patterns {
    /// The patterns in `msgs`. With `all`, every slot must be there;
    /// without, missing slots show as not loaded yet (a read in progress or
    /// stopped) and the checks run on the ones that are. `quiet`: no log
    /// lines (re-parsed after every pattern while a read streams in).
    pub(super) fn parse(model: Model, msgs: &[DumpMsg], all: bool, quiet: bool) -> Result<Self, String> {
        let log = |s: String| {
            if !quiet {
                log(s)
            }
        };
        let mut msg = vec![usize::MAX; PATTERNS];
        for (mi, m) in msgs.iter().enumerate().filter(|(_, m)| m.kind == PATTERN_KIT) {
            let i = m.index as usize;
            if i >= PATTERNS || msg[i] != usize::MAX {
                log(format!("pattern message index {i} out of range or repeated"));
                return Err(format!("pattern message #{i} out of range or repeated"));
            }
            msg[i] = mi;
        }
        if let Some(i) = msg.iter().position(|&m| m == usize::MAX).filter(|_| all) {
            log(format!("no pattern message for slot {}", label(i)));
            return Err(format!("no pattern for {}", label(i)));
        }
        let loaded: Vec<bool> = msg.iter().map(|&m| m != usize::MAX).collect();
        let have: Vec<usize> = (0..PATTERNS).filter(|&i| loaded[i]).collect();
        let Some(&f) = have.first() else {
            return Err("no patterns loaded".into());
        };
        let data = |i: usize| &msgs[msg[i]].data;

        // Every pattern should share one struct version and length, or a
        // pattern moved to another slot might not fit there.
        let pv = be32(data(f), 0).ok_or_else(|| format!("pattern {} too short", label(f)))?;
        let len = data(f).len();
        let mut problems: Vec<String> = Vec::new();
        for &i in &have[1..] {
            let (v, l) = (be32(data(i), 0), data(i).len());
            if v != Some(pv) || l != len {
                log(format!("{}: version {v:?} length {l}, {} has v{pv} length {len}", label(i), label(f)));
                problems.push("patterns differ in version or length".into());
                break;
            }
        }

        let slot_at = pattern_slot_byte(model, pv).ok_or_else(|| {
            log(format!("no slot byte offset for pattern v{pv}"));
            format!("unknown pattern version v{pv}")
        })?;
        let kit = pattern_sizeof(model, pv).ok_or(format!("unknown pattern version v{pv}"))?;
        // DT2 kits start with the beefbace magic, then the version and name.
        let kv_at = if model == DT2 && be32(data(f), kit) == Some(MAGIC) { kit + 4 } else { kit };
        let kv = be32(data(f), kv_at).unwrap_or(u32::MAX);
        let kit_name_at = kv_at + 4;
        let pattern_name_at = pattern_plocks(model, pv).unwrap_or(0) + PLOCK_ROWS * (2 + model.steps() * 2);
        let sounds = kit_sounds(model, kv);
        let sv = kit_sound_version(model, kv);
        let ssize = sv.and_then(|v| sound_sizeof(model, v));
        let sslot = sv.and_then(|v| sound_slot(model, v));
        if len < slot_at + 1 || len < kit + kit_sizeof(model, kv).unwrap_or(usize::MAX) {
            log(format!("pattern payload {len} bytes: shorter than slot byte {slot_at} or kit end"));
            return Err("pattern payload shorter than expected".into());
        }

        // The slot byte holds each pattern's own slot (seen on an older DT1
        // project), the same value in every pattern (0 on a fresh one), or a
        // mix of the two: the DT1 writes 0 when a pattern is saved on the
        // device and leaves the others alone, and loads either fine.
        let off_slots: Vec<usize> = have.iter().copied().filter(|&i| data(i)[slot_at] as usize != i).collect();
        let off: Vec<String> = off_slots.iter().map(|&i| format!("{}={}", label(i), data(i)[slot_at])).collect();
        let first = data(f)[slot_at];
        let off_value = off_slots.first().map(|&i| data(i)[slot_at]);
        let slot_value = if off.is_empty() {
            None
        } else if have.iter().all(|&i| data(i)[slot_at] == first) {
            log(format!("slot byte at {slot_at} is {first} in every pattern: keeping it at {first} on moves"));
            Some(first)
        } else if let Some(v) = off_value.filter(|&v| off_slots.iter().all(|&i| data(i)[slot_at] == v)) {
            log(format!(
                "slot byte at {slot_at}: own slot in most patterns, {v} in {} (saved on the device): {}; writing the own slot on moves",
                off.len(),
                off.iter().take(16).cloned().collect::<Vec<_>>().join(", ")
            ));
            None
        } else {
            log(format!(
                "slot byte at {slot_at} is neither the own slot nor the same everywhere; {} differ from the slot: {}",
                off.len(),
                off.iter().take(16).cloned().collect::<Vec<_>>().join(", ")
            ));
            problems.push(format!("slot byte at {slot_at} doesn't match {} pattern(s)", off.len()));
            None
        };

        // The empty pattern: the most common payload with the slot byte zeroed.
        let norm = |i: usize| {
            let mut d = data(i).clone();
            d[slot_at] = 0;
            d
        };
        let mut counts: HashMap<Vec<u8>, usize> = HashMap::new();
        for &i in &have {
            *counts.entry(norm(i)).or_default() += 1;
        }
        let (blank, n_blank) = counts.into_iter().max_by_key(|(_, n)| *n).unwrap_or_default();
        let blank = if n_blank >= 2 && text_at(&blank, kit_name_at).is_empty() {
            log(format!("empty pattern template: {n_blank} of {} slots identical apart from the slot byte", have.len()));
            Some(blank)
        } else {
            if have.len() >= 2 {
                // Show where two likely-empty patterns differ, to find the
                // next per-slot field.
                let (ia, ib) = (have[have.len() - 2], have[have.len() - 1]);
                let (a, b) = (data(ia), data(ib));
                let diffs: Vec<String> = a.iter().zip(b).enumerate().filter(|(_, (x, y))| x != y).take(16).map(|(o, (x, y))| format!("{o}:{x:02x}/{y:02x}")).collect();
                log(format!(
                    "no empty-pattern template (largest identical group {n_blank}); {} vs {} differ at {}",
                    label(ia),
                    label(ib),
                    diffs.join(" ")
                ));
            }
            // With only some banks loaded there may just not be two empty
            // ones yet: clearing is off then (`can_clear`), editing isn't.
            if all {
                problems.push("couldn't find an empty pattern to clear slots with".into());
            }
            None
        };

        let sample_param = sv.and_then(|v| super::sample_param(model, v));
        let plocks = pattern_plocks(model, pv);
        let stride = plocks.map(|pl| (pl, track_stride(model, pv, pl))).filter(|&(pl, st)| {
            let body = pl.saturating_sub(4);
            !(model == DT && body % SEQ_TRACKS != 0) && st >= model.steps() * 2 && st * SEQ_TRACKS <= body
        });
        let midi_mask = kit_midi_mask(model, kv).map(|o| kit + o);
        let empty_row = plocks.and_then(|pl| {
            let row = 2 + model.steps() * 2;
            if let Some(r) = blank.as_ref().and_then(|b| b.get(pl..pl + row)) {
                return Some(r.to_vec());
            }
            let mut c: HashMap<&[u8], usize> = HashMap::new();
            for &i in &have {
                for r in 0..PLOCK_ROWS {
                    if let Some(h) = data(i).get(pl + r * row..pl + (r + 1) * row).filter(|h| no_locks(model, h)) {
                        *c.entry(h).or_default() += 1;
                    }
                }
            }
            c.into_iter().max_by_key(|(_, n)| *n).map(|(r, _)| r.to_vec())
        });
        let geo = Geo { model, pv, len, kit, sounds, ssize, sslot, sample_param, plocks, stride: stride.map(|s| s.1), midi_mask };
        let mut odd_kit = Vec::new();
        let mut pattern_fields = Vec::new();
        let mut bad_sound = Vec::new();
        let info = (0..PATTERNS)
            .map(|i| {
                if !loaded[i] {
                    return PatternInfo::not_loaded();
                }
                let d = norm(i);
                let diff = blank.as_ref().map_or(usize::MAX, |b| d.iter().zip(b).filter(|(x, y)| x != y).count());
                let state = match diff {
                    0 => PatState::Blank,
                    n if n <= PHANTOM_BYTES => PatState::Phantom,
                    _ => PatState::Used,
                };
                if !printable(&d, kit_name_at) {
                    odd_kit.push(format!("{}:{:02x?}", label(i), &d[kit_name_at..kit_name_at + 16]));
                }
                let pf = text_at(&d, pattern_name_at);
                if !pf.is_empty() {
                    pattern_fields.push(format!("{}:{:02x?}", label(i), &d[pattern_name_at..pattern_name_at + 16]));
                }
                if let (Some(s0), Some(sz)) = (sounds, ssize) {
                    for t in 0..model.kit_tracks() {
                        if be32(&d, kit + s0 + t * sz) != Some(MAGIC) {
                            bad_sound.push(format!("{} t{}", label(i), t + 1));
                        }
                    }
                } else if i == f {
                    log(format!("no track sound layout for kit v{kv}"));
                }
                make_info(&geo, kit_name_at, pattern_name_at, state, diff, &d)
            })
            .collect::<Vec<_>>();
        if !odd_kit.is_empty() {
            log(format!("kit name at {kit_name_at} isn't plain text in {} pattern(s): {}", odd_kit.len(), odd_kit.iter().take(8).cloned().collect::<Vec<_>>().join("  ")));
            problems.push("some kit names aren't plain text".into());
        }
        if !pattern_fields.is_empty() {
            log(format!(
                "pattern-name field at {pattern_name_at} is non-empty in {} pattern(s) {}",
                pattern_fields.len(),
                pattern_fields.iter().take(8).cloned().collect::<Vec<_>>().join("  ")
            ));
        }
        if !bad_sound.is_empty() {
            log(format!("track sounds without the beefbace magic: {}", bad_sound.iter().take(12).cloned().collect::<Vec<_>>().join(", ")));
        }
        let used = info.iter().filter(|p| p.state == PatState::Used).count();
        let ph = info.iter().filter(|p| p.state == PatState::Phantom).count();
        log(format!(
            "pattern v{pv} kit v{kv}: {} of {PATTERNS} loaded, {used} used, {ph} nearly empty, slot byte {slot_at}, kit name {kit_name_at}",
            have.len()
        ));
        for (i, p) in info.iter().enumerate().filter(|(_, p)| p.state == PatState::Phantom) {
            if let Some(b) = blank.as_ref().filter(|_| !quiet) {
                let d = norm(i);
                let at: Vec<String> = d.iter().zip(b).enumerate().filter(|(_, (x, y))| x != y).map(|(o, _)| format!("kit+{}", o as isize - kit as isize)).collect();
                log(format!("{} nearly empty ({} bytes): {}", label(i), p.diff, at.join(" ")));
            }
        }

        problems.dedup();
        Ok(Self {
            info,
            loaded,
            msg,
            blank,
            slot_at,
            slot_value,
            kit_name_at,
            pattern_name_at,
            empty_row,
            read_only: (!problems.is_empty()).then(|| problems.join("; ")),
            geo,
        })
    }

    /// Take in a pattern payload loaded from a file, already in this
    /// project's sample numbering, as message `mi`. Returns its source index
    /// for a `Cell`. It must have the same struct version and length as the
    /// project's own patterns: nothing is converted.
    pub(super) fn add(&mut self, mi: usize, d: &[u8]) -> Result<u8, String> {
        let g = &self.geo;
        let pv = be32(d, 0).ok_or("pattern payload too short")?;
        if pv != g.pv || d.len() != g.len {
            return Err(format!(
                "this pattern is v{pv} ({} bytes); the project's patterns are v{} ({} bytes). Converting between versions isn't supported yet",
                d.len(),
                g.pv,
                g.len
            ));
        }
        let idx = u8::try_from(self.info.len()).map_err(|_| "too many patterns loaded from files; reload the project first".to_owned())?;
        let mut n = d.to_vec();
        n[self.slot_at] = 0;
        let diff = self.blank.as_ref().map_or(usize::MAX, |b| n.iter().zip(b).filter(|(x, y)| x != y).count());
        let state = match diff {
            0 => PatState::Blank,
            k if k <= PHANTOM_BYTES => PatState::Phantom,
            _ => PatState::Used,
        };
        let info = make_info(&self.geo, self.kit_name_at, self.pattern_name_at, state, diff, &n);
        self.info.push(info);
        self.loaded.push(true);
        self.msg.push(mi);
        Ok(idx)
    }

    /// Every pattern of bank `b` (0 = A) has arrived.
    pub fn bank_loaded(&self, b: usize) -> bool {
        self.loaded[b * 16..(b + 1) * 16].iter().all(|&l| l)
    }

    /// Patterns of the project's own 128 that have arrived.
    pub fn loaded_count(&self) -> usize {
        self.loaded[..PATTERNS].iter().filter(|&&l| l).count()
    }

    pub fn can_clear(&self) -> bool {
        self.blank.is_some()
    }

    /// Message index of slot `slot`'s pattern, if it has arrived.
    pub(super) fn msg_of(&self, slot: usize) -> Option<usize> {
        self.loaded[slot].then(|| self.msg[slot])
    }

    /// The payload for slot `i` holding `cell`, from `src_data(slot as read)`
    /// (the pool-remapped payloads). `None` when it needs an empty pattern
    /// and there isn't one.
    pub(super) fn build(&self, i: usize, cell: &Cell, src_data: impl Fn(usize) -> Vec<u8>) -> Option<Vec<u8>> {
        let mut d = match cell.src {
            Some(s) => src_data(s as usize),
            None => self.blank.clone()?,
        };
        for (&t, from) in &cell.tracks {
            let s = match from {
                Some(s) => src_data(*s as usize),
                None => self.blank.clone()?,
            };
            if let Err(e) = self.splice_track(&mut d, &s, t as usize) {
                log(format!("{}: track {} not copied: {e}", label(i), t + 1));
                return None;
            }
        }
        d[self.slot_at] = self.slot_value.unwrap_or(i as u8);
        if let Some(n) = &cell.name {
            // The device shows the pattern name; the kit name is kept in step
            // like elk-herd does.
            let n = clean_name(n);
            for at in [self.pattern_name_at, self.kit_name_at] {
                let f = &mut d[at..at + 16];
                f.fill(0);
                f[..n.len()].copy_from_slice(n.as_bytes());
            }
        }
        Some(d)
    }

    /// Why single tracks can't be copied in this project, if they can't.
    pub fn track_copy_blocked(&self) -> Option<String> {
        let g = &self.geo;
        if let Some(r) = &self.read_only {
            return Some(r.clone());
        }
        if g.stride.is_none() {
            return Some(format!("the track layout of pattern v{} isn't known", g.pv));
        }
        if g.sounds.is_none() || g.ssize.is_none() {
            return Some("the kit's track sound layout isn't known for this version".into());
        }
        if self.empty_row.is_none() {
            return Some("couldn't tell what an unused p-lock row looks like".into());
        }
        None
    }

    /// Copy track `t` of payload `s` over track `t` of `d`: its sequencer
    /// track (trigs, notes, length, scale…), its p-lock rows and, for kit
    /// tracks, its sound (sample, parameters) and MIDI-track bit. The p-lock
    /// rows are packed to the front afterwards, in their old order.
    pub(super) fn splice_track(&self, d: &mut [u8], s: &[u8], t: usize) -> Result<(), String> {
        if let Some(why) = self.track_copy_blocked() {
            return Err(why);
        }
        let g = &self.geo;
        let (Some(pl), Some(stride), Some(empty)) = (g.plocks, g.stride, &self.empty_row) else { unreachable!() };
        if t >= SEQ_TRACKS || d.len() != s.len() || d.len() != g.len {
            return Err("track or payload size out of range".into());
        }
        let mut copy = |at: usize, n: usize| d[at..at + n].copy_from_slice(&s[at..at + n]);
        copy(4 + t * stride, stride);
        if t < g.model.kit_tracks() {
            let (Some(s0), Some(sz)) = (g.sounds, g.ssize) else { unreachable!() };
            copy(g.kit + s0 + t * sz, sz);
            if let Some(m) = g.midi_mask {
                let bit = 1u16 << t;
                let v = (be16(d, m).unwrap_or(0) & !bit) | (be16(s, m).unwrap_or(0) & bit);
                d[m..m + 2].copy_from_slice(&v.to_be_bytes());
            }
        }
        let row = empty.len();
        let rows = |x: &[u8]| -> Vec<Vec<u8>> { (0..PLOCK_ROWS).map(|r| x[pl + r * row..pl + (r + 1) * row].to_vec()).collect() };
        let mut keep: Vec<Vec<u8>> = rows(d).into_iter().filter(|r| r != empty && r[1] as usize != t).collect();
        keep.extend(rows(s).into_iter().filter(|r| r != empty && r[1] as usize == t));
        if keep.len() > PLOCK_ROWS {
            return Err(format!("needs {} p-lock rows, a pattern has {PLOCK_ROWS}", keep.len()));
        }
        for r in 0..PLOCK_ROWS {
            d[pl + r * row..pl + (r + 1) * row].copy_from_slice(keep.get(r).unwrap_or(empty));
        }
        Ok(())
    }

    /// Bytes where `got` (read back) differs from `want`, for the log.
    pub fn describe_diff(&self, want: &[u8], got: &[u8]) -> String {
        if want.len() != got.len() {
            return format!("length {} vs sent {}", got.len(), want.len());
        }
        let d: Vec<String> = want.iter().zip(got).enumerate().filter(|(_, (a, b))| a != b).take(12).map(|(o, (a, b))| format!("{o}:{a:02x}→{b:02x}")).collect();
        d.join(" ")
    }
}
