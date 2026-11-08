//! The 128 patterns of a Digitakt (1) project: what each slot holds, and
//! moving / copying / clearing / renaming whole patterns.
//!
//! On the Digitakt each pattern carries its own copy of its kit, and nothing
//! in the project points at a pattern number, so a pattern moves as one
//! `0x50` payload plus its slot byte (`patternStorage_kitIndex`). Sample and
//! sound references are project-wide and stay valid.
//!
//! elk-herd was mostly tested on the Digitakt II, so every assumption made
//! here is checked against the dump and logged (`elk-pat:`) when it doesn't
//! hold. See `project_management_discovery.md` §8.

use super::*;
use std::collections::HashMap;

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
}

/// What's on one sequencer track, for a thumbnail.
#[derive(Clone, Debug)]
pub struct TrackSeq {
    /// Per step: `Step::None`, `Lock` (trig flags but no note) or `Note`.
    pub steps: Vec<Step>,
    /// Track length in steps.
    pub len: usize,
}

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

/// Sequencer tracks in a pattern: audio, then MIDI.
const SEQ_TRACKS: usize = 16;

/// Each track starts with one big-endian u16 of trig flags per step, right
/// after the pattern's 4-byte version. From DT1 v10 dumps: bit 0 = note
/// trig; 0x0010 = swing (set on every odd step by default, so ignored); any
/// other bit = something on the step without a note (lock trig) [unverified
/// for which bit is which]. Byte `stride - 6` is the track length in v10.
fn parse_seq(model: Model, pv: u32, d: &[u8], plocks: usize) -> Vec<TrackSeq> {
    let steps = model.steps();
    let body = plocks.saturating_sub(4);
    if body % SEQ_TRACKS != 0 || body / SEQ_TRACKS < steps * 2 {
        return Vec::new();
    }
    let stride = body / SEQ_TRACKS;
    (0..SEQ_TRACKS)
        .map(|t| {
            let at = 4 + t * stride;
            let steps: Vec<Step> = (0..steps)
                .map(|k| match be16(d, at + k * 2).unwrap_or(0) {
                    w if w & 1 != 0 => Step::Note,
                    w if w & !0x10 != 0 => Step::Lock,
                    _ => Step::None,
                })
                .collect();
            let byte = if model == DT && pv >= 9 { d.get(at + stride - 6).map(|&b| b as usize) } else { None };
            let len = match byte {
                Some(n) if (1..=steps.len()).contains(&n) => n,
                // Unknown layout: up to the last used step, in whole pages.
                _ => (steps.iter().rposition(|s| *s != Step::None).map_or(0, |p| p + 1).div_ceil(16) * 16).max(16),
            };
            TrackSeq { steps, len }
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
}

impl Cell {
    pub fn as_read(slot: usize) -> Self {
        Self { src: Some(slot as u8), name: None }
    }
}

pub struct Patterns {
    pub info: Vec<PatternInfo>,
    /// Message index of each slot's `0x50`.
    msg: Vec<usize>,
    /// An empty pattern, slot byte 0. `None`: couldn't tell which one.
    blank: Option<Vec<u8>>,
    slot_at: usize,
    /// What every pattern's slot byte holds; `None`: its own slot.
    slot_value: Option<u8>,
    kit_name_at: usize,
    pattern_name_at: usize,
    /// Why moves / renames aren't allowed, if they aren't (names still show).
    pub read_only: Option<String>,
}

fn log(s: String) {
    crate::log::line(format!("elk-pat: {s}"));
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

impl Patterns {
    pub(super) fn parse(model: Model, msgs: &[DumpMsg]) -> Result<Self, String> {
        if model != DT {
            return Err("the project editor is Digitakt (1) only for now".into());
        }
        let mut msg = vec![usize::MAX; PATTERNS];
        for (mi, m) in msgs.iter().enumerate().filter(|(_, m)| m.kind == PATTERN_KIT) {
            let i = m.index as usize;
            if i >= PATTERNS || msg[i] != usize::MAX {
                log(format!("pattern message index {i} out of range or repeated"));
                return Err(format!("pattern message #{i} out of range or repeated"));
            }
            msg[i] = mi;
        }
        if let Some(i) = msg.iter().position(|&m| m == usize::MAX) {
            log(format!("no pattern message for slot {}", label(i)));
            return Err(format!("no pattern for {}", label(i)));
        }
        let data = |i: usize| &msgs[msg[i]].data;

        // Every pattern should share one struct version and length, or a
        // pattern moved to another slot might not fit there.
        let pv = be32(data(0), 0).ok_or("pattern A01 too short")?;
        let len = data(0).len();
        let mut problems: Vec<String> = Vec::new();
        for i in 1..PATTERNS {
            let (v, l) = (be32(data(i), 0), data(i).len());
            if v != Some(pv) || l != len {
                log(format!("{}: version {v:?} length {l}, A01 has v{pv} length {len}", label(i)));
                problems.push("patterns differ in version or length".into());
                break;
            }
        }

        let slot_at = pattern_slot_byte(model, pv).ok_or_else(|| {
            log(format!("no slot byte offset for pattern v{pv}"));
            format!("unknown pattern version v{pv}")
        })?;
        let kit = pattern_sizeof(model, pv).ok_or(format!("unknown pattern version v{pv}"))?;
        let kv = be32(data(0), kit).unwrap_or(u32::MAX);
        let kit_name_at = kit + 4;
        let pattern_name_at = pattern_plocks(model, pv).unwrap_or(0) + PLOCK_ROWS * (2 + model.steps() * 2);
        let sounds = kit_sounds(model, kv);
        let sv = kit_sound_version(model, kv);
        let ssize = sv.and_then(|v| sound_sizeof(model, v));
        let sslot = sv.and_then(|v| sound_slot(model, v));
        if len < slot_at + 1 || len < kit + kit_sizeof(model, kv).unwrap_or(usize::MAX) {
            log(format!("pattern payload {len} bytes: shorter than slot byte {slot_at} or kit end"));
            return Err("pattern payload shorter than expected".into());
        }

        // The slot byte either holds each pattern's own slot (seen on an older
        // DT1 project) or the same value in every pattern (0 on a fresh one).
        let off: Vec<String> = (0..PATTERNS)
            .filter(|&i| data(i)[slot_at] as usize != i)
            .map(|i| format!("{}={}", label(i), data(i)[slot_at]))
            .collect();
        let first = data(0)[slot_at];
        let slot_value = if off.is_empty() {
            None
        } else if (0..PATTERNS).all(|i| data(i)[slot_at] == first) {
            log(format!("slot byte at {slot_at} is {first} in every pattern: keeping it at {first} on moves"));
            Some(first)
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
        for i in 0..PATTERNS {
            *counts.entry(norm(i)).or_default() += 1;
        }
        let (blank, n_blank) = counts.into_iter().max_by_key(|(_, n)| *n).unwrap_or_default();
        let blank = if n_blank >= 2 && text_at(&blank, kit_name_at).is_empty() {
            log(format!("empty pattern template: {n_blank} of {PATTERNS} slots identical apart from the slot byte"));
            Some(blank)
        } else {
            // Show where two likely-empty patterns differ, to find the next
            // per-slot field.
            let (a, b) = (data(PATTERNS - 2), data(PATTERNS - 1));
            let diffs: Vec<String> = a.iter().zip(b).enumerate().filter(|(_, (x, y))| x != y).take(16).map(|(o, (x, y))| format!("{o}:{x:02x}/{y:02x}")).collect();
            log(format!(
                "no empty-pattern template (largest identical group {n_blank}); G15 vs H16 differ at {}",
                diffs.join(" ")
            ));
            problems.push("couldn't find an empty pattern to clear slots with".into());
            None
        };

        let mut odd_kit = Vec::new();
        let mut pattern_fields = Vec::new();
        let mut bad_sound = Vec::new();
        let info = (0..PATTERNS)
            .map(|i| {
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
                let tracks = match (sounds, ssize, sslot) {
                    (Some(s0), Some(sz), Some(sl)) => (0..model.kit_tracks())
                        .map(|t| {
                            let s = kit + s0 + t * sz;
                            if be32(&d, s) != Some(MAGIC) {
                                bad_sound.push(format!("{} t{}", label(i), t + 1));
                            }
                            TrackInfo { name: text_at(&d, s + 12), slot: d.get(s + sl).copied().unwrap_or(0) as usize }
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                if i == 0 && tracks.is_empty() {
                    log(format!("no track sound layout for kit v{kv}"));
                }
                let seq = pattern_plocks(model, pv).map_or_else(Vec::new, |pl| parse_seq(model, pv, &d, pl));
                let kit_name = text_at(&d, kit_name_at);
                let name = if !pf.is_empty() && printable(&d, pattern_name_at) { pf.clone() } else { kit_name.clone() };
                PatternInfo { state, name, kit_name, pattern_field: pf, tracks, seq, diff }
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
        log(format!("pattern v{pv} kit v{kv}: {used} used, {ph} nearly empty, slot byte {slot_at}, kit name {kit_name_at}"));
        for (i, p) in info.iter().enumerate().filter(|(_, p)| p.state == PatState::Phantom) {
            if let Some(b) = &blank {
                let d = norm(i);
                let at: Vec<String> = d.iter().zip(b).enumerate().filter(|(_, (x, y))| x != y).map(|(o, _)| format!("kit+{}", o as isize - kit as isize)).collect();
                log(format!("{} nearly empty ({} bytes): {}", label(i), p.diff, at.join(" ")));
            }
        }

        problems.dedup();
        Ok(Self {
            info,
            msg,
            blank,
            slot_at,
            slot_value,
            kit_name_at,
            pattern_name_at,
            read_only: (!problems.is_empty()).then(|| problems.join("; ")),
        })
    }

    pub fn can_clear(&self) -> bool {
        self.blank.is_some()
    }

    pub(super) fn msg_of(&self, slot: usize) -> usize {
        self.msg[slot]
    }

    /// The payload for slot `i` holding `cell`, from `src_data(slot as read)`
    /// (the pool-remapped payloads). `None` when it needs an empty pattern
    /// and there isn't one.
    pub(super) fn build(&self, i: usize, cell: &Cell, src_data: impl Fn(usize) -> Vec<u8>) -> Option<Vec<u8>> {
        let mut d = match cell.src {
            Some(s) => src_data(s as usize),
            None => self.blank.clone()?,
        };
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

    /// Bytes where `got` (read back) differs from `want`, for the log.
    pub fn describe_diff(&self, want: &[u8], got: &[u8]) -> String {
        if want.len() != got.len() {
            return format!("length {} vs sent {}", got.len(), want.len());
        }
        let d: Vec<String> = want.iter().zip(got).enumerate().filter(|(_, (a, b))| a != b).take(12).map(|(o, (a, b))| format!("{o}:{a:02x}→{b:02x}")).collect();
        d.join(" ")
    }
}
