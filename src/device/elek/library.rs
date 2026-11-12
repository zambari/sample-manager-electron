//! Read-only lookups for the Sounds page (`ui::elek_library_view`): the
//! project's sound pool (one `0x63` request per sound, falling back to the
//! whole-project dump, which carries the non-empty ones), the MIDI track
//! names in Digitakt II kits, and the +Drive's data tree (`/soundbanks`,
//! `/projects`, …) over the data API list `0x53`.
//!
//! The project read also takes the sound pool in (`Worker::read_sounds`),
//! so a sample pool push can renumber the samples pool sounds play.
//!
//! Sound pool edits (`SoundCell`: moved, emptied, renamed, filled from a
//! preset) go out with the project push (`ElekProject::build_push`): each
//! changed pool sound as one `0x53` message, an emptied one as a blank
//! sound (`blank_sound`), and every pattern's sound locks renumbered to
//! follow (`lock_spots`), or cleared when their sound goes.

use super::*;
use std::sync::Arc;

/// Where pool sound `i` comes from after the Sounds page's edits.
#[derive(Clone, PartialEq, Debug)]
pub enum SoundSrc {
    /// The pool sound read at this index.
    Read(u8),
    Empty,
    /// A preset from the +Drive: its sound struct, and the pool sample it
    /// plays (numbered as read, 0 = none).
    Preset { data: Arc<Vec<u8>>, sample: u16, from: String },
}

/// One pool sound after the edits: where it comes from, and its new name.
#[derive(Clone, PartialEq, Debug)]
pub struct SoundCell {
    pub src: SoundSrc,
    pub name: Option<String>,
}

impl SoundCell {
    pub fn as_read(i: usize) -> Self {
        Self { src: SoundSrc::Read(i as u8), name: None }
    }
}

/// The sound pool untouched: each sound where it was read.
pub fn sounds_as_read() -> Vec<SoundCell> {
    (0..SOUND_POOL).map(SoundCell::as_read).collect()
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
fn set_sound_slot(m: Model, d: &mut [u8], slot: usize) {
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
        from_pool.or_else(|| self.blank_kit_sound().and_then(|d| be32(&d, 4)))
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
        let mut map = vec![NO_LOCK; SOUND_POOL];
        for (new, c) in cells.iter().enumerate() {
            if let SoundSrc::Read(o) = c.src {
                map[o as usize] = new as u8;
            }
        }
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
    pub(super) fn sound_want(&self, cells: &[SoundCell], i: usize, data: &[Option<Vec<u8>>], new_of: &[usize]) -> Option<Vec<u8>> {
        let cell = cells.get(i).cloned().unwrap_or_else(|| SoundCell::as_read(i));
        let mut d = match &cell.src {
            SoundSrc::Read(o) => {
                let mi = self.sound_msg(*o as usize)?;
                let d = data.get(mi).cloned().flatten().unwrap_or_else(|| self.msgs[mi].data.clone());
                if be32(&d, 0) != Some(MAGIC) || parse_sound(self.model, i, &d).blank {
                    return None;
                }
                d
            }
            SoundSrc::Empty => return None,
            SoundSrc::Preset { data: p, sample, .. } => {
                let mut d = p.to_vec();
                set_sound_slot(self.model, &mut d, new_of.get(*sample as usize).copied().unwrap_or(0));
                d
            }
        };
        if let Some(n) = &cell.name {
            set_sound_name(&mut d, n);
        }
        Some(d)
    }

    /// The `0x53` payloads a push sends: every pool sound that changes
    /// (all non-empty ones with `all`), an emptied one as a blank sound.
    pub(super) fn sound_writes(&self, cells: &[SoundCell], data: &[Option<Vec<u8>>], new_of: &[usize], all: bool) -> Vec<(u8, Vec<u8>)> {
        let mut blank = None;
        let mut out = Vec::new();
        for i in 0..SOUND_POOL {
            let now = self.sound_now(i);
            match self.sound_want(cells, i, data, new_of) {
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
        let ident: Vec<usize> = (0..self.pool.len()).collect();
        (0..SOUND_POOL)
            .map(|i| match self.sound_want(cells, i, &[], &ident) {
                Some(d) => parse_sound(self.model, i, &d),
                None => PoolSound::blank(i),
            })
            .collect()
    }

    /// A preset's sound struct, ready for the pool, and the sample file it
    /// plays (`None`: no sample).
    pub fn preset_sound(&self, p: &Preset) -> Result<(Vec<u8>, Option<PoolEntry>), String> {
        let m = self.model;
        let at = p.at.ok_or("not a sound preset this app can read")?;
        let blob = &p.data[at..];
        let sv = be32(blob, 4).ok_or("preset too short")?;
        let size = sound_sizeof(m, sv).ok_or_else(|| format!("unknown sound version {sv}"))?;
        if let Some(pool_v) = self.pool_sound_version() {
            if pool_v != sv {
                return Err(format!("the preset is a version {sv} sound and this project's are version {pool_v} (made on other firmware)"));
            }
        }
        let d = blob.get(..size).ok_or("preset shorter than a sound")?.to_vec();
        let file = sound_file(m, sv).and_then(|f| {
            let e = PoolEntry { inode: be32(&d, f)?, hash: be32(&d, f + 4)?, size: be32(&d, f + 8)?, seqnr: be32(&d, f + 12)? };
            (!e.is_empty() && e.hash != 0 && e.size > 16).then_some(e)
        });
        Ok((d, file))
    }
}

/// Sounds in a project's sound pool (the dump index is 7-bit).
pub const SOUND_POOL: usize = 128;

const REQ_SOUND: u8 = 0x63;
/// Data API reads (see project_discovery.md §3). Read-only.
const DATA_READ_OPEN: u8 = 0x54;
const DATA_READ_CHUNK: u8 = 0x55;
const DATA_READ_CLOSE: u8 = 0x56;
/// First sound request: long enough for the device to answer at all.
const FIRST_SOUND_WAIT: Duration = Duration::from_secs(3);
/// Later ones: an unanswered sound counts as empty.
const SOUND_WAIT: Duration = Duration::from_millis(1500);

/// One entry of the project's sound pool (or a sound preset).
#[derive(Clone, Debug, Default)]
pub struct PoolSound {
    pub index: usize,
    pub name: String,
    /// Pool sample slot (0 = none).
    pub slot: usize,
    /// No sound here: not a sound struct, or no name and no sample.
    pub blank: bool,
    /// The SRC page as `(value, fine)`, in the order the device shows it
    /// for a one-shot sound: TUNE, PLAY, BIT, SMP, STRT, LEN, LOOP, LEV.
    /// Empty where the layout isn't known. On the Digitakt II other
    /// machines put other parameters here (machine byte not found yet).
    pub src: Vec<(u8, u8)>,
    /// The sample file the sound was made with (+Drive hash), if any.
    pub file_hash: Option<u32>,
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
fn src_offsets(m: Model, sv: u32) -> Option<[usize; 8]> {
    let slot = sound_slot(m, sv)?;
    Some(if m == DT2 {
        [slot - 6, slot - 4, slot - 2, slot, slot + 6, slot + 8, slot + 10, slot + 12]
    } else {
        [slot - 6, slot - 4, slot - 2, slot, slot + 2, slot + 4, slot + 6, slot + 8]
    })
}

/// The sample file entry (inode, hash, size, seqnr) in a sound struct.
fn sound_file(m: Model, sv: u32) -> Option<usize> {
    Some(match (m, sv) {
        (DT, 1..=3) => 132,
        (DT2, 3) => 297,
        _ => return None,
    })
}

pub(super) fn parse_sound(m: Model, index: usize, d: &[u8]) -> PoolSound {
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
    let file_hash = sound_file(m, sv).and_then(|at| {
        let inode = be32(d, at)?;
        (inode != u32::MAX).then(|| be32(d, at + 4)).flatten().filter(|&h| h != 0)
    });
    PoolSound { index, name, slot, blank, src, file_hash }
}

/// A sound preset read from the +Drive (`/soundbanks/…`): the bytes as
/// they came, and the sound struct found in them, if any.
#[derive(Clone, Debug)]
pub struct Preset {
    pub bytes: usize,
    /// Where the sound struct starts in the blob.
    pub at: Option<usize>,
    pub sound: Option<PoolSound>,
    /// The first bytes, in hex, for the log and the tooltip.
    pub head: String,
    /// Where the blob was saved for research.
    pub saved: Option<std::path::PathBuf>,
    /// The blob itself, to put the sound into the pool.
    pub data: Arc<Vec<u8>>,
}

fn parse_preset(m: Model, blob: &[u8]) -> Preset {
    let at = blob.windows(4).take(256).position(|w| w == MAGIC.to_be_bytes());
    let sound = at.map(|a| parse_sound(m, 0, &blob[a..]));
    let head = blob.iter().take(48).map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ");
    Preset { bytes: blob.len(), at, sound, head, saved: None, data: Arc::new(blob.to_vec()) }
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

/// The 16 MIDI track names of a Digitakt II kit, where the layout is known
/// (kit v4: kit+0x4638, 268 bytes apart; found in a DT II 1.15+ dump).
/// Empty for anything else.
pub fn midi_names(m: Model, d: &[u8]) -> Vec<String> {
    if m != DT2 {
        return Vec::new();
    }
    let Some(kit) = be32(d, 0).and_then(|pv| pattern_sizeof(m, pv)) else { return Vec::new() };
    let kv_at = if be32(d, kit) == Some(MAGIC) { kit + 4 } else { kit };
    if be32(d, kv_at) != Some(4) {
        return Vec::new();
    }
    (0..16).map(|t| text16(d, kit + 0x4638 + t * 0x10C)).collect()
}

/// One entry of a +Drive data listing: a folder, or an item in a slot.
#[derive(Clone, Debug)]
pub struct DataItem {
    pub name: String,
    pub dir: bool,
    /// Slot number (items); for folders whatever the device sent.
    pub slot: u32,
    pub size: u32,
}

/// `path` joined with `name`, for listing a folder inside it.
pub fn data_child(path: &str, name: &str) -> String {
    if path.ends_with('/') { format!("{path}{name}") } else { format!("{path}/{name}") }
}

impl Conn {
    /// A +Drive data folder: same request and reply as `list_projects`,
    /// folders kept.
    pub(super) fn list_data(&mut self, path: &str) -> R<Vec<DataItem>> {
        let args = Args::default().str(path).u32(0).u32(0).bytes(&[1]);
        let r = self.request(proto::DATA_LIST, args)?;
        if r.first() != Some(&1) || r.len() < 13 {
            return refused(format!("the device wouldn't list {path}"));
        }
        let mut rd = Reader::new(&r[13..]);
        let mut out = Vec::new();
        let mut next = 1;
        let bad = |e: String| Fail::Refused(format!("odd listing of {path}: {e}"));
        while !rd.is_empty() {
            let name = rd.str().map_err(bad)?;
            let _children = rd.u8().map_err(bad)?;
            match rd.u8().map_err(bad)? {
                1 => {
                    let v = rd.u32().map_err(bad)?;
                    out.push(DataItem { name, dir: true, slot: v, size: 0 });
                }
                2 => {
                    let id = rd.u32().map_err(bad)?;
                    let size = rd.u32().map_err(bad)?;
                    rd.u8().map_err(bad)?;
                    rd.u8().map_err(bad)?;
                    let _valid = rd.u8().map_err(bad)?;
                    let _meta = rd.u8().map_err(bad)?;
                    let slot = if id == 0 || id == u32::MAX { next } else { id };
                    next = slot + 1;
                    out.push(DataItem { name, dir: false, slot, size });
                }
                t => {
                    crate::log::line(format!("elek-lib: listing {path}: unknown entry type {t} after {} entries", out.len()));
                    break;
                }
            }
        }
        Ok(out)
    }

    /// A +Drive data item (a sound preset), read whole over the data API's
    /// read open / chunk / close, uncompressed. Read-only.
    pub(super) fn read_data(&mut self, path: &str) -> R<Vec<u8>> {
        let r = self.request(DATA_READ_OPEN, Args::default().str(path).u32(proto::CHUNK as u32).bytes(&[0]))?;
        crate::log::line(format!("elek-lib: read open {path}: {:02x?}", &r[..r.len().min(16)]));
        if r.first() != Some(&1) || r.len() < 10 {
            return refused(format!("the device wouldn't open {path}"));
        }
        let job = Reader::new(&r[1..]).u32().map_err(Fail::Refused)?;
        let mut out = Vec::new();
        let mut result = Ok(());
        for seq in 0..4096u32 {
            let c = match self.request(DATA_READ_CHUNK, Args::default().u32(job).u32(seq)) {
                Ok(c) => c,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            };
            // ok, job, seq, status, last, hash, size, data
            if c.first() != Some(&1) || c.len() < 22 {
                crate::log::line(format!("elek-lib: odd chunk {seq} of {path}: {:02x?}", &c[..c.len().min(32)]));
                result = refused(format!("odd reply reading {path}"));
                break;
            }
            let last = c[13] != 0;
            if seq == 0 {
                crate::log::line(format!("elek-lib: first chunk of {path}: header {:02x?}, {} data bytes", &c[..22], c.len() - 22));
            }
            out.extend_from_slice(&c[22..]);
            if last {
                break;
            }
        }
        let _ = self.request(DATA_READ_CLOSE, Args::default().u32(job));
        result.map(|()| out)
    }

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
    /// A sound preset from the +Drive, parsed, and its bytes saved under
    /// `~/Documents/sample_manager/project_dumps/presets` for research.
    pub(super) fn read_preset(&mut self, path: &str) -> Result<Preset, String> {
        let m = self.with_conn(|c| c.model())?;
        let blob = self.with_conn(|c| c.read_data(path))?;
        let mut p = parse_preset(m, &blob);
        crate::log::line(format!(
            "elek-lib: preset {path}: {} bytes, sound struct at {:?}, name {:?}, starts {}",
            p.bytes,
            p.at,
            p.sound.as_ref().map(|s| s.name.clone()),
            p.head
        ));
        if let Some(home) = std::env::var_os("HOME") {
            let dir = std::path::PathBuf::from(home).join("Documents/sample_manager/project_dumps/presets");
            let file = dir.join(format!("{}_{}.bin", m.name().replace(' ', ""), path.trim_start_matches('/').replace('/', "_")));
            if std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&file, &blob)).is_ok() {
                p.saved = Some(file);
            }
        }
        Ok(p)
    }

    /// The project read's last part: every pool sound, one request each,
    /// into `st` (only real sound structs are kept, as the whole dump
    /// would). A sound that doesn't come even when asked twice means the
    /// device doesn't answer these, and the whole dump is read instead.
    /// `st.sounds` is set once all are in.
    pub(super) fn read_sounds(&mut self, st: &mut Stream, ctl: &ReadCtl) -> Result<(), String> {
        let em = self.em.clone();
        let m = match st.m {
            Some(m) => m,
            None => self.with_conn(|c| c.model())?,
        };
        let started = Instant::now();
        let mut kept = 0;
        for i in 0..SOUND_POOL {
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
                return self.read_whole(st, ctl);
            };
            if be32(&msg.data, 0) == Some(MAGIC) {
                st.add(&em, msg);
                kept += 1;
            }
            self.serve_pending();
        }
        st.sounds = true;
        crate::log::line(format!(
            "elek: sound pool read, {kept} sound structs of {SOUND_POOL} in {:.1}s",
            started.elapsed().as_secs_f32()
        ));
        Ok(())
    }

    /// The whole sound pool, sound by sound (progress as `SoundProgress`).
    /// If the device doesn't answer single-sound requests, the whole
    /// project dump instead, which holds the non-empty sounds.
    pub(super) fn read_sound_pool(&mut self) -> Result<Vec<PoolSound>, String> {
        let em = self.em.clone();
        let m = self.with_conn(|c| c.model())?;
        let first = self.with_conn(|c| c.read_sound(m, 0, FIRST_SOUND_WAIT))?;
        let Some(first) = first else {
            crate::log::line("elek-lib: no answer to a single-sound request; reading the whole project for its sounds");
            let (_, msgs) = self.with_conn(|c| {
                c.read_project(&mut |i, _| {
                    em.send(Ev::Elek(ElekEv::SoundProgress { done: i + 1, total: 129.max(i + 2) }));
                    true
                })
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
