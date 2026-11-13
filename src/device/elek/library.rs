//! The project's sound pool for the Sounds page (`ui::elek_library_view`):
//! one `0x63` request per sound, falling back to the whole-project dump,
//! which carries the non-empty ones.
//!
//! The project read also takes the sound pool in (`Worker::read_sound_step`),
//! so a sample pool push can renumber the samples pool sounds play.
//!
//! Sound pool edits (`SoundCell`: moved, emptied, renamed, or a kit track's sound pasted in) go out with the project push (`ElekProject::build_push`): each
//! changed pool sound as one `0x53` message, an emptied one as a blank
//! sound (`blank_sound`), and every pattern's sound locks renumbered to
//! follow (`lock_spots`), or cleared when their sound goes.

use super::*;
use super::machines::{machine_id, machine_name, src_params, SrcParam};

/// Where pool sound `i` comes from after the Sounds page's edits.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SoundSrc {
    /// The pool sound read at this index.
    Read(u8),
    Empty,
    /// Kit track `track`'s sound in pattern `pattern` (slot as read),
    /// pasted from the Patterns page's track sound clipboard.
    Track { pattern: u8, track: u8 },
    /// A saved preset, with its sample slot translated into this project.
    Preset { model: Model, data: Vec<u8> },
}

/// One pool sound after the edits: where it comes from, and its new name.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SoundCell {
    pub src: SoundSrc,
    pub name: Option<String>,
    /// Replacement tag bitmap; None keeps the source sound's tags.
    pub tags: Option<u32>,
}

impl SoundCell {
    pub fn as_read(i: usize) -> Self {
        Self { src: SoundSrc::Read(i as u8), name: None, tags: None }
    }
}

/// The sound pool untouched: each sound where it was read.
pub fn sounds_as_read() -> Vec<SoundCell> {
    (0..SOUND_POOL).map(SoundCell::as_read).collect()
}

/// Where each pool sound's locks go (old index → new; `NO_LOCK`: removed).
/// A sound found in several places (pasted copies) keeps its locks where it
/// still sits as read, else at its first place; a place whose own sound went
/// nowhere keeps its locks, which then play whatever was pasted there.
pub fn lock_map(cells: &[SoundCell]) -> Vec<u8> {
    let mut map = vec![NO_LOCK; SOUND_POOL];
    if cells.is_empty() {
        return (0..SOUND_POOL as u8).collect();
    }
    for (new, c) in cells.iter().enumerate() {
        if let SoundSrc::Read(o) = c.src {
            if map[o as usize] == NO_LOCK || new == o as usize {
                map[o as usize] = new as u8;
            }
        }
    }
    for (new, c) in cells.iter().enumerate() {
        if c.src != SoundSrc::Empty && map[new] == NO_LOCK {
            map[new] = new as u8;
        }
    }
    map
}

/// Pool sounds that differ from the read (`cells` empty: none).
pub fn sounds_changed(cells: &[SoundCell]) -> usize {
    cells.iter().enumerate().filter(|(i, c)| **c != SoundCell::as_read(*i)).count()
}

/// Where the per-step sound locks start in a track of pattern version
/// `pv` (elk-herd `trackStorage_soundSlotLocks`, by track version).
fn sound_lock_at(m: Model, pv: u32) -> Option<usize> {
    Some(match (m, pv) {
        (DT, 1..=10) => 832,
        (DT2, 0) => 1920,
        (DT2, 3 | 4) => 1024,
        _ => return None,
    })
}

const NO_LOCK: u8 = 0xFF;

/// Write `name` (upper case, up to 15 chars, NUL padded) into a sound struct.
fn set_sound_name(d: &mut [u8], name: &str) {
    let name = patterns::clean_name(name);
    if let Some(field) = d.get_mut(12..28) {
        field.fill(0);
        field[..name.len()].copy_from_slice(name.as_bytes());
    }
}

/// Point a sound struct at pool sample `slot`.
pub(super) fn set_sound_slot(m: Model, d: &mut [u8], slot: usize) {
    let Some(at) = be32(d, 4).and_then(|sv| sound_slot(m, sv)) else { return };
    if m == DT2 {
        if let Some(b) = d.get_mut(at..at + 2) {
            b.copy_from_slice(&(slot as u16).to_be_bytes());
        }
    } else if let Some(b) = d.get_mut(at) {
        *b = slot as u8;
    }
}

impl ElekProject {
    /// The message holding pool sound `i` as read, if one came.
    fn sound_msg(&self, i: usize) -> Option<usize> {
        self.msgs[..self.base].iter().position(|m| m.kind == SOUND && m.index as usize == i)
    }

    /// Sound struct version of the pool, from a pool sound or else a kit.
    fn pool_sound_version(&self) -> Option<u32> {
        let from_pool = self.msgs[..self.base]
            .iter()
            .filter(|m| m.kind == SOUND && be32(&m.data, 0) == Some(MAGIC))
            .find_map(|m| be32(&m.data, 4));
        from_pool.or_else(|| self.blank_kit_sound().and_then(|d| be32(&d, 4))).or_else(|| {
            let pats = self.patterns.as_ref().ok()?;
            (0..patterns::PATTERNS).filter_map(|i| pats.msg_of(i)).find_map(|mi| {
                (0..self.model.kit_tracks()).find_map(|track| {
                    pats.track_sound(&self.msgs[mi].data, track).and_then(|(s, _)| be32(s, 4))
                })
            })
        })
    }

    /// Kit track 1's sound in an empty pattern: the device's default sound.
    fn blank_kit_sound(&self) -> Option<Vec<u8>> {
        let pats = self.patterns.as_ref().ok()?;
        let m = self.model;
        (0..patterns::PATTERNS).filter(|&i| pats.info.get(i).is_some_and(|f| f.state == PatState::Blank)).find_map(|i| {
            let d = &self.msgs[pats.msg_of(i)?].data;
            let kit = pattern_sizeof(m, be32(d, 0)?)?;
            let kv_at = if m == DT2 && be32(d, kit) == Some(MAGIC) { kit + 4 } else { kit };
            let kv = be32(d, kv_at)?;
            let sv = kit_sound_version(m, kv)?;
            let s = kit + kit_sounds(m, kv)?;
            let snd = d.get(s..s + sound_sizeof(m, sv)?)?;
            (be32(snd, 0) == Some(MAGIC)).then(|| snd.to_vec())
        })
    }

    /// What an empty pool sound is sent as: an empty one the device gave
    /// us, else (as elk-herd does) the default kit sound with no name and
    /// no tags. `None` when neither is to be had.
    pub fn blank_sound(&self) -> Option<Vec<u8>> {
        let m = self.model;
        let read = self.msgs[..self.base]
            .iter()
            .filter(|x| x.kind == SOUND && be32(&x.data, 0) == Some(MAGIC))
            .find(|x| parse_sound(m, x.index as usize, &x.data).blank)
            .map(|x| x.data.clone());
        if read.is_some() {
            return read;
        }
        let mut d = self.blank_kit_sound()?;
        if self.pool_sound_version().is_some_and(|v| be32(&d, 4) != Some(v)) {
            return None;
        }
        d.get_mut(8..12)?.fill(0);
        set_sound_name(&mut d, "");
        set_sound_slot(m, &mut d, 0);
        Some(d)
    }

    /// Byte offsets of every sound-lock step in a pattern payload.
    fn lock_spots(&self, d: &[u8]) -> Vec<usize> {
        let m = self.model;
        let Some(pv) = be32(d, 0) else { return Vec::new() };
        let (Some(off), Some(plocks)) = (sound_lock_at(m, pv), pattern_plocks(m, pv)) else { return Vec::new() };
        let stride = patterns::track_stride(m, pv, plocks);
        (0..16)
            .flat_map(|t| (0..m.steps()).map(move |s| 4 + t * stride + off + s))
            .filter(|&at| at < d.len())
            .collect()
    }

    /// Sound-locked steps per pool sound (as read), over every pattern.
    pub fn sound_lock_uses(&self) -> Vec<u32> {
        let mut out = vec![0u32; SOUND_POOL];
        for m in self.msgs[..self.base].iter().filter(|m| m.kind == PATTERN_KIT) {
            for at in self.lock_spots(&m.data) {
                if let Some(c) = out.get_mut(m.data[at] as usize) {
                    *c += 1;
                }
            }
        }
        out
    }

    /// Sound locks can be renumbered in this project's patterns.
    pub fn locks_known(&self) -> bool {
        self.msgs[..self.base]
            .iter()
            .filter(|m| m.kind == PATTERN_KIT)
            .all(|m| be32(&m.data, 0).is_some_and(|pv| sound_lock_at(self.model, pv).is_some()))
    }

    /// Old pool sound → new index (`NO_LOCK` when it's gone), or `None`
    /// when nothing moved.
    pub(super) fn sound_moves(cells: &[SoundCell]) -> Option<Vec<u8>> {
        if sounds_changed(cells) == 0 {
            return None;
        }
        let map = lock_map(cells);
        (map.iter().enumerate().any(|(i, &n)| n as usize != i)).then_some(map)
    }

    /// Renumber the sound locks in a pattern payload.
    pub(super) fn move_locks(&self, d: &mut [u8], map: &[u8]) {
        for at in self.lock_spots(d) {
            let v = d[at];
            if v != NO_LOCK {
                d[at] = map.get(v as usize).copied().unwrap_or(NO_LOCK);
            }
        }
    }

    /// Pool sound `i` as read: `None` when empty.
    fn sound_now(&self, i: usize) -> Option<&[u8]> {
        let mi = self.sound_msg(i)?;
        let d = &self.msgs[mi].data;
        (be32(d, 0) == Some(MAGIC) && !parse_sound(self.model, i, d).blank).then_some(&d[..])
    }

    /// Pool sound `i` after the edits (`None`: empty), from the payloads
    /// with the sample pool renumbered (`data`, from `new_data`) and the
    /// sample renumbering itself (`new_of[old] = new`).
    pub(super) fn sound_want(&self, cells: &[SoundCell], i: usize, data: &[Option<Vec<u8>>]) -> Option<Vec<u8>> {
        let cell = cells.get(i).cloned().unwrap_or_else(|| SoundCell::as_read(i));
        self.cell_sound(&cell, data)
    }

    /// The sound struct pool cell `cell` stands for (`None`: empty), from
    /// the payloads with the sample pool renumbered (`data`, before patterns
    /// are placed; empty: as read).
    pub(super) fn cell_sound(&self, cell: &SoundCell, data: &[Option<Vec<u8>>]) -> Option<Vec<u8>> {
        let mut d = match &cell.src {
            SoundSrc::Read(o) => {
                let mi = self.sound_msg(*o as usize)?;
                let d = data.get(mi).cloned().flatten().unwrap_or_else(|| self.msgs[mi].data.clone());
                if be32(&d, 0) != Some(MAGIC) || parse_sound(self.model, 0, &d).blank {
                    return None;
                }
                d
            }
            SoundSrc::Preset { model, data: raw } => {
                if *model != self.model { return None; }
                let mut d = raw.clone();
                if !data.is_empty() {
                    if let Some(key) = presets::sample_key(self.model, &d) {
                        let settings = data.get(self.settings_msg).and_then(Option::as_deref)
                            .unwrap_or(&self.msgs[self.settings_msg].data);
                        let (_, _, pool) = read_pool(self.model, settings).ok()?;
                        let slot = pool.iter().enumerate().skip(1)
                            .find(|(_, e)| !e.is_empty() && e.key() == key).map_or(0, |(i, _)| i);
                        set_sound_slot(self.model, &mut d, slot);
                    }
                }
                d
            }
            SoundSrc::Empty => return None,
            SoundSrc::Track { pattern, track } => self.track_sound_for_pool(Some(*pattern), *track, data).ok()?,
        };
        if let Some(n) = &cell.name {
            set_sound_name(&mut d, n);
        }
        if let Some(tags) = cell.tags {
            d.get_mut(8..12)?.copy_from_slice(&tags.to_be_bytes());
        }
        Some(d)
    }

    /// Kit track `track`'s sound in pattern `pattern` (slot as read; `None`:
    /// an empty pattern) as a pool sound, from the payloads with the sample
    /// pool renumbered (`data`, from `new_data`; empty: as read). `Err` says
    /// why it can't go into the pool.
    pub fn track_sound_for_pool(&self, pattern: Option<u8>, track: u8, data: &[Option<Vec<u8>>]) -> Result<Vec<u8>, String> {
        let t = track as usize + 1;
        let pattern = pattern.ok_or_else(|| format!("track {t} comes from an empty pattern: no sound to paste"))?;
        let pats = self.patterns.as_ref().map_err(|e| e.clone())?;
        let mi = pats.msg_of(pattern as usize).ok_or_else(|| format!("pattern {} isn't read", patterns::label(pattern as usize)))?;
        let d = data.get(mi).cloned().flatten().unwrap_or_else(|| self.msgs[mi].data.clone());
        let (snd, midi) = pats.track_sound(&d, track as usize).ok_or_else(|| format!("track {t} has no kit sound (MIDI track or kit layout not known)"))?;
        if midi {
            return Err(format!("track {t} is a MIDI track: the pool holds audio sounds only"));
        }
        if be32(snd, 0) != Some(MAGIC) || parse_sound(self.model, 0, snd).blank {
            return Err(format!("track {t} has an empty sound"));
        }
        let from_pool = self.msgs[..self.base]
            .iter()
            .filter(|m| m.kind == SOUND && be32(&m.data, 0) == Some(MAGIC))
            .find_map(|m| be32(&m.data, 4));
        if let (Some(v), Some(kv)) = (from_pool, be32(snd, 4)) {
            if v != kv {
                return Err(format!("the kit sound is version {kv}, the pool's sounds are version {v}: not safe to paste"));
            }
        }
        Ok(snd.to_vec())
    }

    /// Snapshot a copied sound, including a pending rename.
    pub fn copied_sound_data(&self, cell: &SoundCell) -> Option<Vec<u8>> {
        self.cell_sound(cell, &[])
    }

    /// Check a preset before importing it into this project's sound pool.
    pub fn accepts_preset(&self, model: Model, data: &[u8]) -> Result<(), String> {
        if model != self.model { return Err("The copied sound belongs to another device model".into()); }
        let version = be32(data, 4).ok_or("Missing sound version")?;
        if sound_sizeof(model, version) != Some(data.len()) || be32(data, 0) != Some(MAGIC) {
            return Err("Unsupported preset sound layout".into());
        }
        let target = self.pool_sound_version().ok_or("Cannot determine the project's sound version; reload the project")?;
        // Pool entries are independent versioned SysEx sound dumps. Keep an
        // older supported payload intact for the firmware's loader to upgrade.
        // Kit track pastes still require matching layouts: those splice bytes
        // directly into a fixed-size kit (pool_sound_for_track).
        if version > target {
            return Err(format!("This preset uses sound version {version}, newer than this project's version {target}. Update the device firmware and reload the project before pasting"));
        }
        Ok(())
    }

    /// Pool cell `cell` as a kit track sound (decoded), or why it can't be
    /// one: empty, or a sound version unlike the kits'.
    pub fn pool_sound_for_track(&self, cell: &SoundCell) -> Result<PoolSound, String> {
        let d = self.cell_sound(cell, &[]).ok_or("that pool sound is empty")?;
        let pats = self.patterns.as_ref().map_err(|e| e.clone())?;
        let kit_v = (0..patterns::PATTERNS)
            .filter_map(|i| pats.msg_of(i))
            .find_map(|mi| pats.track_sound(&self.msgs[mi].data, 0).and_then(|(s, _)| be32(s, 4)));
        if let (Some(kv), Some(v)) = (kit_v, be32(&d, 4)) {
            if kv != v {
                return Err(format!("the pool sound is version {v}, the kits' sounds are version {kv}: not safe to paste"));
            }
        }
        Ok(parse_sound(self.model, 0, &d))
    }

    /// The `0x53` payloads a push sends: every pool sound that changes
    /// (all non-empty ones with `all`), an emptied one as a blank sound.
    pub(super) fn sound_writes(&self, cells: &[SoundCell], data: &[Option<Vec<u8>>], all: bool) -> Vec<(u8, Vec<u8>)> {
        let mut blank = None;
        let mut out = Vec::new();
        for i in 0..SOUND_POOL {
            let now = self.sound_now(i);
            match self.sound_want(cells, i, data) {
                Some(w) => {
                    if all || now != Some(&w[..]) {
                        out.push((i as u8, w));
                    }
                }
                None if now.is_some() => {
                    if blank.is_none() {
                        blank = Some(self.blank_sound());
                    }
                    match blank.clone().flatten() {
                        Some(b) => out.push((i as u8, b)),
                        None => crate::log::line(format!("elek-lib: no blank sound to clear pool sound {i} with; left as is")),
                    }
                }
                None => {}
            }
        }
        out
    }

    /// The pool sounds as edited, for showing (samples numbered as read).
    pub fn edited_sounds(&self, cells: &[SoundCell]) -> Vec<PoolSound> {
        (0..SOUND_POOL)
            .map(|i| match self.sound_want(cells, i, &[]) {
                Some(d) => parse_sound(self.model, i, &d),
                None => PoolSound::blank(i),
            })
            .collect()
    }

}

/// Sounds in a project's sound pool (the dump index is 7-bit).
pub const SOUND_POOL: usize = 128;

const REQ_SOUND: u8 = 0x63;

/// The device answers `0x63` with pool sound `i`. The Digitakt (I) answers
/// 0..7 with the active kit's track sounds (same `0x53` reply, same index)
/// and stays quiet from 8 on, so its pool comes from the whole dump only.
fn single_sounds(m: Model) -> bool {
    m != DT
}
/// First sound request: long enough for the device to answer at all.
const FIRST_SOUND_WAIT: Duration = Duration::from_secs(3);
/// Later ones: an unanswered sound counts as empty.
const SOUND_WAIT: Duration = Duration::from_millis(1500);

/// One entry of the project's sound pool.
#[derive(Clone, Debug, Default)]
pub struct PoolSound {
    pub index: usize,
    pub name: String,
    /// Sound-header tag bitmap, least-significant bit first.
    pub tags: u32,
    /// Pool sample slot (0 = none).
    pub slot: usize,
    /// No sound here: not a sound struct, or no name and no sample.
    pub blank: bool,
    /// The SRC page as `(value, fine)`, in the order the device shows it
    /// for a one-shot sound: TUNE, PLAY, BIT, SMP, STRT, LEN, LOOP, LEV.
    /// Empty where the layout isn't known. Other machines put other
    /// parameters in these places: see `params` and `machines.rs`.
    pub src: Vec<(u8, u8)>,
    /// The machine byte and its name, where the layout is known.
    pub machine: Option<(u8, String)>,
    /// `src` as this machine shows it (one-shot names when the machine
    /// isn't known), the sample place included.
    pub params: Vec<SrcParam>,
}

/// SRC page names, in `PoolSound::src` order.
pub const SRC_NAMES: [&str; 8] = ["TUNE", "PLAY", "BIT", "SMP", "STRT", "LEN", "LOOP", "LEV"];

impl PoolSound {
    pub fn blank(index: usize) -> Self {
        Self { index, blank: true, ..Self::default() }
    }
}

/// Where each SRC page parameter sits in a sound struct (2 bytes each,
/// value then fine), from comparing default and edited sounds: the sample
/// slot sits in the middle of the page on both models, the level of 100
/// after it. Digitakt II has 4 more bytes after its 2-byte slot.
///
/// What each place means depends on the sound's machine: see `machines.rs`
/// (machine byte DT1 sound+124, DT II sound+210, tables per machine).
fn src_offsets(m: Model, sv: u32) -> Option<[usize; 8]> {
    let slot = sound_slot(m, sv)?;
    Some(if m == DT2 {
        [slot - 6, slot - 4, slot - 2, slot, slot + 6, slot + 8, slot + 10, slot + 12]
    } else {
        [slot - 6, slot - 4, slot - 2, slot, slot + 2, slot + 4, slot + 6, slot + 8]
    })
}

pub(crate) fn parse_sound(m: Model, index: usize, d: &[u8]) -> PoolSound {
    if be32(d, 0) != Some(MAGIC) {
        return PoolSound::blank(index);
    }
    let sv = be32(d, 4).unwrap_or(u32::MAX);
    let slot = sound_slot(m, sv)
        .and_then(|at| if m == DT2 { be16(d, at).map(usize::from) } else { d.get(at).map(|&b| b as usize) })
        .unwrap_or(0);
    let name = text16(d, 12);
    let blank = name.trim().is_empty() && slot == 0;
    let src = src_offsets(m, sv)
        .and_then(|o| o.iter().map(|&at| Some((*d.get(at)?, *d.get(at + 1)?))).collect::<Option<Vec<_>>>())
        .unwrap_or_default();
    let id = machine_id(m, sv, d);
    let params = src_params(m, id.unwrap_or(0), &src);
    let machine = id.map(|i| (i, machine_name(m, i)));
    PoolSound { index, name, tags: be32(d, 8).unwrap_or(0), slot, blank, src, machine, params }
}

/// All 128 pool sounds from the sound messages there are (blank for the rest).
pub(super) fn sounds_from<'a>(m: Model, msgs: impl Iterator<Item = &'a DumpMsg>) -> Vec<PoolSound> {
    let mut out: Vec<PoolSound> = (0..SOUND_POOL).map(|i| PoolSound::blank(i)).collect();
    for msg in msgs {
        let i = msg.index as usize;
        if i < SOUND_POOL {
            out[i] = parse_sound(m, i, &msg.data);
        }
    }
    out
}

/// Up to 16 chars, stopping at the first NUL (· for non-printable).
fn text16(d: &[u8], at: usize) -> String {
    d.get(at..at + 16)
        .map(|b| {
            b.iter()
                .take_while(|&&c| c != 0)
                .map(|&c| if (0x20..0x7F).contains(&c) { c as char } else { '·' })
                .collect()
        })
        .unwrap_or_default()
}

impl Conn {
    /// Sound pool entry `i` on its own. `Ok(None)`: nothing within `wait`.
    fn read_sound(&mut self, m: Model, i: u8, wait: Duration) -> R<Option<DumpMsg>> {
        while self.rx.try_recv().is_ok() {}
        self.send_sysex(&build_dump(m, REQ_SOUND, i, &[]))?;
        let until = Instant::now() + wait;
        loop {
            let Ok(raw) = self.rx.recv_timeout(until.saturating_duration_since(Instant::now())) else {
                return Ok(None);
            };
            match parse_dump(m, &raw) {
                Ok(Some(msg)) if msg.kind == SOUND && msg.index == i => return Ok(Some(msg)),
                Ok(_) => {}
                Err(e) => return refused(e),
            }
        }
    }
}

impl Worker {
    /// One sound-pool read job. The cursor survives a page switch even
    /// for empty sounds, whose structs aren't kept in the project.
    pub(super) fn read_sound_step(&mut self, st: &mut Stream, ctl: &ReadCtl) -> Result<(), String> {
        let em = self.em.clone();
        let m = match st.m {
            Some(m) => m,
            None => {
                let m = self.with_conn(|c| c.model())?;
                st.m = Some(m);
                m
            }
        };
        if !single_sounds(m) {
            crate::log::line("elek: Digitakt (I) answers single-sound requests with kit tracks; reading the whole project for the sound pool");
            return self.read_whole(st, ctl);
        }
        let i = st.sound_next;
        if i < SOUND_POOL {
            if ctl.stopped() {
                crate::log::line(format!("elek: read stopped after {i} of {SOUND_POOL} pool sounds"));
                return Ok(());
            }
            em.send(Ev::Elek(ElekEv::Progress { label: "Reading sound pool".into(), done: i, total: SOUND_POOL }));
            let wait = if i == 0 { FIRST_SOUND_WAIT } else { SOUND_WAIT };
            let mut got = self.with_conn(|c| c.read_sound(m, i as u8, wait))?;
            if got.is_none() && i > 0 {
                got = self.with_conn(|c| c.read_sound(m, i as u8, SOUND_WAIT))?;
            }
            let Some(msg) = got else {
                crate::log::line(format!("elek: pool sound {i} didn't arrive; reading the whole project for the sound pool"));
                // What did answer may not be pool sounds: the dump's own win.
                st.msgs.retain(|x| x.kind != SOUND);
                return self.read_whole(st, ctl);
            };
            if be32(&msg.data, 0) == Some(MAGIC) {
                st.add(&em, msg);
            }
            st.sound_next += 1;
        }
        st.sounds = st.sound_next == SOUND_POOL;
        Ok(())
    }

    /// The whole sound pool, sound by sound (progress as `SoundProgress`).
    /// If the device doesn't answer single-sound requests, the whole
    /// project dump instead, which holds the non-empty sounds.
    pub(super) fn read_sound_pool(&mut self) -> Result<Vec<PoolSound>, String> {
        let em = self.em.clone();
        let m = self.with_conn(|c| c.model())?;
        let first = if single_sounds(m) { self.with_conn(|c| c.read_sound(m, 0, FIRST_SOUND_WAIT))? } else { None };
        let Some(first) = first else {
            crate::log::line("elek-lib: no answer to a single-sound request; reading the whole project for its sounds");
            let (_, msgs) = self.read_project_browsing(&mut |i, _| {
                em.send(Ev::Elek(ElekEv::SoundProgress { done: i + 1, total: 129.max(i + 2) }));
                true
            })?;
            let got: Vec<PoolSound> = msgs.iter().filter(|m| m.kind == SOUND).map(|s| parse_sound(m, s.index as usize, &s.data)).collect();
            crate::log::line(format!("elek-lib: whole dump held {} sounds", got.len()));
            let mut out: Vec<PoolSound> = (0..SOUND_POOL).map(|i| PoolSound::blank(i)).collect();
            for s in got {
                if s.index < SOUND_POOL {
                    let i = s.index;
                    out[i] = s;
                }
            }
            return Ok(out);
        };
        let mut out = vec![parse_sound(m, 0, &first.data)];
        let mut missing = 0;
        for i in 1..SOUND_POOL {
            em.send(Ev::Elek(ElekEv::SoundProgress { done: i, total: SOUND_POOL }));
            let got = self.with_conn(|c| c.read_sound(m, i as u8, SOUND_WAIT))?;
            out.push(match got {
                Some(msg) => parse_sound(m, i, &msg.data),
                None => {
                    missing += 1;
                    PoolSound::blank(i)
                }
            });
            self.serve_pending();
        }
        crate::log::line(format!(
            "elek-lib: sound pool read, {} with a name or sample, {missing} unanswered",
            out.iter().filter(|s| !s.blank).count()
        ));
        Ok(out)
    }
}
