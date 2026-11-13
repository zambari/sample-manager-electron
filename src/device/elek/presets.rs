//! +Drive presets: `/soundbanks`, bank listings and uncompressed
//! Data API sound containers. Independent of the current project's pools.
//! Verified on Digitakt II build 0082: B:001's container starts its sound
//! at byte 36; the full v3 struct exactly matches project pool sound #009.

use super::library::{PoolSound, SoundCell};
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresetEntry {
    /// Device id, used verbatim in `/soundbanks/<bank>/<slot>` (one based).
    pub slot: u32,
    pub name: String,
}

#[derive(Clone)]
pub struct Preset {
    pub model: Model,
    pub container: Vec<u8>,
    pub sound: PoolSound,
    pub data: Vec<u8>,
    pub sample: Option<((u32, u32), Option<String>)>,
}

impl Preset {
    /// Shared sound header: big-endian tag bits at sound+8 on both Digitakts.
    pub fn tag_mask(&self) -> u32 {
        be32(&self.data, 8).unwrap_or(0)
    }

    /// Validate an importable container before any edit can be staged.
    pub(super) fn from_container(model: Model, loc: &PresetSlot, container: Vec<u8>) -> Result<Self, String> {
        let data = decode_sound(model, &container)?.to_vec();
        preset_container(model, loc, &data, &container)?;
        Ok(Self {
            model,
            sound: library::parse_sound(model, loc.slot as usize, &data),
            sample: sample_key(model, &data).map(|key| (key, None)),
            container,
            data,
        })
    }
}

/// Device tag order, least-significant bit first. Confirmed in the preset
/// dump: B001 = 0x400 (Bass), B002 = 0x40 (Hat), B003 = 0x10400
/// (Bass + Electronic). The mask occupies all 32 bits, including Favourite.
pub fn tag_names(mask: u32) -> impl Iterator<Item = &'static str> {
    const NAMES: [&str; 32] = [
        "Kick", "Snare", "Rim", "Clap", "Top", "Perc", "Hat", "Cymbal",
        "Cowbell", "Synth", "Bass", "Lead", "Pad", "Texture", "Chords", "Sound Fx",
        "Electronic", "Metallic", "Acoustic", "Atmosphere", "Noisy", "Glitch",
        "Hard", "Soft", "Dark", "Bright", "Vintage", "Epic", "Fail", "Loop",
        "Mine", "Favourite",
    ];
    NAMES.into_iter().enumerate()
        .filter_map(move |(bit, name)| (mask & (1u32 << bit) != 0).then_some(name))
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PresetSlot {
    pub bank: String,
    pub slot: u32,
}

impl PresetSlot {
    pub fn new(bank: &str, slot: u32) -> Self {
        Self {
            bank: bank.into(),
            slot,
        }
    }

    fn path(&self) -> String {
        format!("/soundbanks/{}/{}", self.bank, self.slot)
    }
}

#[derive(Clone)]
pub enum PresetOp {
    Move(PresetSlot, PresetSlot),
    Swap(PresetSlot, PresetSlot),
    Clear(PresetSlot),
    Write(PresetSlot, Preset),
}

impl PresetOp {
    pub fn slots(&self) -> Vec<PresetSlot> {
        match self {
            Self::Move(a, b) | Self::Swap(a, b) => vec![a.clone(), b.clone()],
            Self::Clear(a) | Self::Write(a, _) => vec![a.clone()],
        }
    }
}

/// Snapshots of every touched slot, plus the staged native operations.
pub struct PresetPush {
    pub ops: Vec<PresetOp>,
    pub before: Vec<(PresetSlot, Option<Vec<u8>>)>,
    pub after: Vec<(PresetSlot, Option<Vec<u8>>)>,
}

/// Native preset edits are restricted to exact soundbank slot paths.
/// Saved project paths remain blocked at the wire boundary; preset uploads
/// also require a writer scoped to a validated preset slot (`allowed_write`).
pub(in crate::device) fn allowed_edit(cmd: u8, args: &[u8]) -> bool {
    let count = match cmd {
        0x5a | 0x5d => 2,
        0x5c => 1,
        _ => return false,
    };
    let parts: Vec<_> = args.split(|b| *b == 0).collect();
    parts.len() == count + 1
        && parts[count].is_empty()
        && parts[..count].iter().all(|p| {
            let Ok(path) = std::str::from_utf8(p) else {
                return false;
            };
            let Some(rest) = path.strip_prefix("/soundbanks/") else {
                return false;
            };
            let Some((bank, slot)) = rest.split_once('/') else {
                return false;
            };
            valid_bank(bank)
                && !slot.is_empty()
                && slot.bytes().all(|b| b.is_ascii_digit())
                && slot.parse::<u32>().is_ok_and(|s| (1..=256).contains(&s))
        })
}

struct DataEntry {
    name: String,
    children: bool,
    slot: Option<u32>,
    valid: bool,
}

impl Conn {
    fn preset_listing(&mut self, path: &str) -> R<Vec<DataEntry>> {
        let reply = self.request(
            proto::DATA_LIST,
            Args::default().str(path).u32(0).u32(0).bytes(&[1]),
        )?;
        if reply.first() != Some(&1) || reply.len() < 13 {
            return refused(format!("The device wouldn't list {path}"));
        }
        let mut rd = Reader::new(&reply[13..]);
        let mut entries = Vec::new();
        let bad = |e: String| Fail::Refused(format!("Invalid preset listing: {e}"));
        while !rd.is_empty() {
            let name = rd.str().map_err(bad)?;
            let children = rd.bool().map_err(bad)?;
            let kind = rd.u8().map_err(bad)?;
            let (slot, valid) = match kind {
                1 => {
                    rd.u32().map_err(bad)?;
                    (None, true)
                }
                2 => {
                    let id = rd.u32().map_err(bad)?;
                    rd.u32().map_err(bad)?; // size
                    rd.u8().map_err(bad)?;
                    rd.u8().map_err(bad)?; // allowed operations
                    let valid = rd.bool().map_err(bad)?;
                    rd.u8().map_err(bad)?; // metadata present
                    (Some(id), valid)
                }
                _ => return refused(format!("Unknown preset entry type {kind}")),
            };
            entries.push(DataEntry {
                name,
                children: children || kind == 1,
                slot,
                valid,
            });
        }
        Ok(entries)
    }

    pub(super) fn preset_banks(&mut self) -> R<Vec<String>> {
        self.model()?;
        Ok(self
            .preset_listing("/soundbanks")?
            .into_iter()
            .filter(|e| {
                e.children && e.name.len() == 1 && e.name.as_bytes()[0].is_ascii_uppercase()
            })
            .map(|e| e.name)
            .collect())
    }

    pub(super) fn preset_entries(&mut self, bank: &str) -> R<Vec<PresetEntry>> {
        if !valid_bank(bank) {
            return refused("Invalid preset bank");
        }
        let mut entries: Vec<_> = self
            .preset_listing(&format!("/soundbanks/{bank}"))?
            .into_iter()
            .filter(|e| e.valid)
            .filter_map(|e| {
                Some(PresetEntry {
                    slot: e.slot?,
                    name: e.name,
                })
            })
            .collect();
        entries.sort_by_key(|e| e.slot);
        Ok(entries)
    }

    /// Compression zero gives the original sound structure in a small
    /// container, so no saved-file compression codec is needed.
    pub(super) fn preset_data(&mut self, path: &str, progress: &impl Fn(f32)) -> R<Vec<u8>> {
        let r = self.request(
            0x54,
            Args::default()
                .str(path)
                .u32(proto::CHUNK as u32)
                .bytes(&[0]),
        )?;
        if r.first() != Some(&1) || r.len() < 10 {
            return refused(format!("The device wouldn't open {path}"));
        }
        let job = u32::from_be_bytes(r[1..5].try_into().unwrap());
        let read = (|| {
            if r[9] != 0 {
                return refused(
                    "The device returned a compressed preset; decoding isn't supported yet",
                );
            }
            let mut data = Vec::new();
            for seq in 0..128 {
                std::thread::sleep(Duration::from_millis(50));
                let r = self.request(0x55, Args::default().u32(job).u32(seq))?;
                if r.first() != Some(&1) || r.len() < 22 {
                    return refused("Invalid preset read response");
                }
                let id = u32::from_be_bytes(r[1..5].try_into().unwrap());
                let size = u32::from_be_bytes(r[18..22].try_into().unwrap()) as usize;
                if id != job || size > proto::CHUNK || data.len() + size > 65536 {
                    return refused("Invalid preset chunk id or size");
                }
                // The first reply can have zero bytes and uninitialized
                // sequence/last fields. Ask for the following sequence.
                if size == 0 {
                    continue;
                }
                if be32(&r, 5) != Some(seq) {
                    return refused("Preset chunk arrived out of sequence");
                }
                let Some(bytes) = r.get(22..22 + size) else {
                    return refused("Short preset chunk");
                };
                data.extend_from_slice(bytes);
                progress((be32(&r, 9).unwrap_or(0) as f32 / 1000.0).clamp(0.0, 1.0));
                if r[13] != 0 {
                    return Ok(data);
                }
            }
            refused("Preset read didn't finish")
        })();
        let close = self.request(0x56, Args::default().u32(job));
        let data = read?;
        if close?.first() != Some(&1) {
            return refused("The device wouldn't close the preset reader");
        }
        Ok(data)
    }

    /// Background row metadata needs no sample-path lookup or editable snapshot.
    pub(super) fn preset_tags(&mut self, bank: &str, slot: u32) -> R<u32> {
        if !valid_bank(bank) || !(1..=256).contains(&slot) {
            return refused("Invalid preset location");
        }
        let model = self.model()?;
        let container = self.preset_data(&PresetSlot::new(bank, slot).path(), &|_| {})?;
        let data = decode_sound(model, &container).map_err(Fail::Refused)?;
        be32(data, 8).ok_or_else(|| Fail::Refused("Missing preset tags".into()))
    }

    pub(super) fn read_preset(
        &mut self,
        bank: &str,
        slot: u32,
        progress: &impl Fn(f32),
    ) -> R<Preset> {
        if !valid_bank(bank) || slot == 0 || slot > 256 {
            return refused("Invalid preset location");
        }
        let model = self.model()?;
        let container = self.preset_data(&format!("/soundbanks/{bank}/{slot}"), progress)?;
        let data = decode_sound(model, &container)
            .map_err(Fail::Refused)?
            .to_vec();
        let sound = library::parse_sound(model, slot as usize, &data);
        let version = be32(&data, 4).unwrap();
        // A preset's sampleSlot is the slot at save time, not necessarily
        // a slot in the project now. Resolve its embedded sample file id.
        let at = sample_id_at(model, &data).ok_or_else(|| Fail::Refused("Unknown preset sample layout".into()))?;
        let sample =
            if machines::plays_sample(model, sound.machine.as_ref().map_or(0, |(id, _)| *id)) {
                match (be32(&data, at), be32(&data, at + 4), be32(&data, at + 8)) {
                    (Some(inode), Some(hash), Some(size))
                        if inode != u32::MAX && hash != 0 && size > 16 =>
                    {
                        let key = (hash, size - 16);
                        let path = self.sample_path(key.0, key.1)?;
                        Some((key, path))
                    }
                    _ => None,
                }
            } else {
                None
            };
        crate::log::line(format!(
            "presets: decoded {bank}:{slot:03} {:?}, sound v{version}, {} bytes",
            sound.name,
            data.len()
        ));
        Ok(Preset {
            model,
            container,
            sound,
            data,
            sample,
        })
    }

    /// Use a container read from this device, retaining its firmware and format
    /// header. Do not invent a header for an unknown device/firmware layout.
    fn preset_template(&mut self, model: Model, version: u32) -> R<Vec<u8>> {
        for bank in self.preset_banks()? {
            for entry in self.preset_entries(&bank)? {
                let container = self.preset_data(&PresetSlot::new(&bank, entry.slot).path(), &|_| {})?;
                if decode_sound(model, &container).ok().and_then(|d| be32(d, 4)) == Some(version) {
                    return Ok(container);
                }
            }
        }
        refused("Saving a pool sound needs an existing saved preset with the same sound version as a format reference. Save one sound on the device first.")
    }

    fn write_preset(&mut self, loc: &PresetSlot, parts: &(Vec<u8>, Vec<u8>)) -> R<()> {
        let size = parts.0.len() + parts.1.len();
        let r = self.request(0x57, Args::default().u32(size as u32).str(&loc.path()))?;
        if r.first() != Some(&1) || r.len() < 5 {
            return refused("The device refused to open the preset writer");
        }
        let job = be32(&r, 1).unwrap();
        self.preset_writer = Some(job);
        let result = (|| {
            let mut seq = 0;
            let mut sent = 0;
            // Uncompressed containers require the footer in its own request.
            for part in [&parts.0, &parts.1] {
                for chunk in part.chunks(proto::CHUNK) {
                    std::thread::sleep(Duration::from_millis(50));
                    let r = self.request(0x58, Args::default().u32(job).u32(seq)
                        .u32(preset_crc(chunk)).u32(chunk.len() as u32).bytes(chunk))?;
                    sent += chunk.len();
                    if r.first() != Some(&1) || be32(&r, 1) != Some(job)
                        || be32(&r, 5) != Some(seq) || be32(&r, 9) != Some(sent as u32) {
                        return refused("Invalid preset write acknowledgement");
                    }
                    seq += 1;
                }
            }
            Ok(())
        })();
        let close = self.request(0x59, Args::default().u32(job).u32(size as u32));
        self.preset_writer = None;
        result?;
        let r = close?;
        if r.first() != Some(&1) || be32(&r, 1) != Some(job) || be32(&r, 5) != Some(size as u32) {
            return refused("The device did not finish saving the preset");
        }
        Ok(())
    }

    pub(super) fn push_presets(
        &mut self,
        plan: &PresetPush,
        progress: &impl Fn(&str, usize, usize),
    ) -> R<()> {
        let model = self.model()?;
        let total = plan.before.len() + plan.ops.len() + plan.after.len();
        let mut done = 0;
        // Check every source AND destination before making any changes.
        for (loc, expected) in &plan.before {
            progress("Checking presets on device", done, total);
            let entries = self.preset_entries(&loc.bank)?;
            let exists = entries.iter().any(|e| e.slot == loc.slot);
            if exists != expected.is_some() {
                return refused(format!(
                    "Preset {}:{:03} changed on device. Reload presets before pushing.",
                    loc.bank, loc.slot
                ));
            }
            if let Some(expected) = expected {
                let container = self.preset_data(&loc.path(), &|_| {})?;
                if decode_sound(model, &container).map_err(Fail::Refused)? != expected {
                    return refused(format!(
                        "Preset {}:{:03} changed on device. Reload presets before pushing.",
                        loc.bank, loc.slot
                    ));
                }
            }
            done += 1;
        }
        // Prepare and validate all containers before the first mutation.
        let mut template = None;
        let mut writes = Vec::new();
        for op in &plan.ops {
            if let PresetOp::Write(loc, preset) = op {
                if preset.model != model { return refused("Copied preset belongs to another device model"); }
                let reference = if preset.container.is_empty() {
                    if template.is_none() {
                        template = Some(self.preset_template(model, be32(&preset.data, 4).unwrap_or(u32::MAX))?);
                    }
                    template.as_ref().unwrap()
                } else { &preset.container };
                writes.push(Some(preset_container(model, loc, &preset.data, reference).map_err(Fail::Refused)?));
            } else { writes.push(None); }
        }
        for (op, write) in plan.ops.iter().zip(&writes) {
            progress("Pushing presets to +Drive", done, total);
            if let PresetOp::Write(loc, _) = op {
                self.write_preset(loc, write.as_ref().unwrap())?;
                done += 1;
                continue;
            }
            let (cmd, args) = match op {
                PresetOp::Move(a, b) => (0x5a, Args::default().str(&a.path()).str(&b.path())),
                PresetOp::Swap(a, b) => (0x5d, Args::default().str(&a.path()).str(&b.path())),
                PresetOp::Clear(a) => (0x5c, Args::default().str(&a.path())),
                PresetOp::Write(..) => unreachable!(),
            };
            if !allowed_edit(cmd, &args.0) {
                return refused("Invalid preset edit");
            }
            let reply = self.request(cmd, args)?;
            if reply.first() != Some(&1) {
                return refused(
                    "The device refused a preset edit. Some edits may already have landed; reload presets before trying again.",
                );
            }
            done += 1;
        }
        for (loc, expected) in &plan.after {
            progress("Verifying presets on device", done, total);
            let exists = self
                .preset_entries(&loc.bank)?
                .iter()
                .any(|e| e.slot == loc.slot);
            if exists != expected.is_some() {
                return refused(
                    "Preset read-back differs from the staged edits. Reload presets to see what is on the device.",
                );
            }
            if let Some(expected) = expected {
                let container = self.preset_data(&loc.path(), &|_| {})?;
                if decode_sound(model, &container).map_err(Fail::Refused)? != expected {
                    return refused(
                        "Preset read-back differs from the staged edits. Reload presets to see what is on the device.",
                    );
                }
            }
            done += 1;
        }
        progress("Presets pushed and verified", total, total);
        Ok(())
    }
}

/// Data uploads are permitted only to an exact preset slot; chunk/close
/// calls must belong to the writer that was opened by `write_preset`.
pub(in crate::device) fn allowed_write(cmd: u8, args: &[u8], job: Option<u32>) -> bool {
    match cmd {
        0x57 => job.is_none() && args.len() > 4 && be32(args, 0).is_some_and(|n| (43..=65536).contains(&n))
            && allowed_edit(0x5c, &args[4..]),
        0x58 => job.is_some() && be32(args, 0) == job && args.len() >= 16
            && be32(args, 12).is_some_and(|n| n as usize == args.len() - 16 && n as usize <= proto::CHUNK),
        0x59 => job.is_some() && be32(args, 0) == job && args.len() == 8,
        _ => false,
    }
}

pub fn translate_slot(model: Model, data: &mut [u8], slot: usize) {
    library::set_sound_slot(model, data, slot);
}

fn sample_id_at(model: Model, data: &[u8]) -> Option<usize> {
    Some(match (model, be32(data, 4)?) {
        (DT, 0..=3) => 132,
        (DT2, 0) => 279,
        (DT2, 2 | 3) => 297,
        _ => return None,
    })
}

pub fn sample_key(model: Model, data: &[u8]) -> Option<(u32, u32)> {
    let sound = library::parse_sound(model, 0, data);
    if !machines::plays_sample(model, sound.machine.as_ref().map_or(0, |(id, _)| *id)) { return None; }
    let at = sample_id_at(model, data)?;
    let (inode, hash, size) = (be32(data, at)?, be32(data, at + 4)?, be32(data, at + 8)?);
    (inode != u32::MAX && hash != 0 && size > 16).then_some((hash, size - 16))
}

/// CRC-32 with the Data API's initial value (zlib crc32(0xffffffff, data)).
fn preset_crc(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 { crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1)); }
    }
    !crc
}

fn preset_container(model: Model, loc: &PresetSlot, data: &[u8], template: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let old = decode_sound(model, template)?;
    let version = be32(data, 4).ok_or("Missing sound version")?;
    if !valid_bank(&loc.bank) || !(1..=256).contains(&loc.slot)
        || be32(template, 0) != Some(0xac11d303) || template.get(4) != Some(&2)
        || template.get(29..31) != Some(&[0, 12][..]) || template.get(36..).is_none_or(|d| old.as_ptr() != d.as_ptr())
        || old.len() != data.len() || be32(old, 4) != Some(version)
        || sound_sizeof(model, version) != Some(data.len()) || be32(data, 0) != Some(MAGIC)
        || template.len() != 36 + old.len() + 12
        || be32(template, 25) != Some((old.len() + 5) as u32)
        || be32(template, template.len() - 12) != Some(preset_crc(&template[31..template.len() - 12]))
        || be32(template, template.len() - 4) != Some(0xaaa1daaa) {
        return Err("Unsupported preset container; nothing written".into());
    }
    let mut body = template[..36].to_vec();
    let index = (loc.bank.as_bytes()[0] - b'A') as u32 * 256 + loc.slot - 1;
    body[21..25].copy_from_slice(&index.to_be_bytes());
    let size = (data.len() + 5) as u32;
    body[25..29].copy_from_slice(&size.to_be_bytes());
    body.extend_from_slice(data);
    let mut footer = Vec::new();
    footer.extend_from_slice(&preset_crc(&body[31..]).to_be_bytes());
    footer.extend_from_slice(&size.to_be_bytes());
    footer.extend_from_slice(&0xaaa1daaau32.to_be_bytes());
    Ok((body, footer))
}

fn valid_bank(bank: &str) -> bool {
    bank.len() == 1 && bank.as_bytes()[0].is_ascii_uppercase()
}

pub(super) fn decode_sound(model: Model, container: &[u8]) -> Result<&[u8], String> {
    // Never interpret an unknown layout as the current project's slots.
    for at in 0..container.len().min(256) {
        if be32(container, at) != Some(MAGIC) {
            continue;
        }
        if let Some(size) = be32(container, at + 4).and_then(|v| sound_sizeof(model, v)) {
            if let Some(sound) = container.get(at..at + size) {
                return Ok(sound);
            }
        }
    }
    Err("No supported sound structure in this preset".into())
}

impl ElekProject {
    pub fn export_preset(&self, cell: &SoundCell, fills: &std::collections::HashMap<u16, PoolEntry>) -> Result<Preset, String> {
        let mut data = self.copied_sound_data(cell).ok_or("The copied sound is empty")?;
        let sound = library::parse_sound(self.model, 0, &data);
        if machines::plays_sample(self.model, sound.machine.as_ref().map_or(0, |(id, _)| *id)) {
            let entry = fills.get(&(sound.slot as u16)).copied()
                .or_else(|| self.pool.get(sound.slot).copied()).unwrap_or(PoolEntry::EMPTY);
            let at = sample_id_at(self.model, &data).ok_or("Unsupported preset sample layout")?;
            data.get_mut(at..at + 16).ok_or("Short preset sound")?.copy_from_slice(&entry.bytes());
        }
        let sample = sample_key(self.model, &data).map(|key| (key, None));
        Ok(Preset { model: self.model, container: Vec::new(), sound, data, sample })
    }

}
