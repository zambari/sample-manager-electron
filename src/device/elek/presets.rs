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
    pub sound: PoolSound,
    pub data: Vec<u8>,
    pub sample: Option<((u32, u32), Option<String>)>,
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
}

impl PresetOp {
    pub fn slots(&self) -> Vec<PresetSlot> {
        match self {
            Self::Move(a, b) | Self::Swap(a, b) => vec![a.clone(), b.clone()],
            Self::Clear(a) => vec![a.clone()],
        }
    }
}

/// Snapshots of every touched slot, plus the staged native operations.
pub struct PresetPush {
    pub ops: Vec<PresetOp>,
    pub before: Vec<(PresetSlot, Option<Vec<u8>>)>,
    pub after: Vec<(PresetSlot, Option<Vec<u8>>)>,
}

/// The only exceptions to the Data API mutation block are native preset
/// move/clear/swap with exact, validated soundbank slot paths. Saved project
/// paths and all Data API upload commands remain blocked at the wire boundary.
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
    fn preset_data(&mut self, path: &str, progress: &impl Fn(f32)) -> R<Vec<u8>> {
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
        let at = match (model, version) {
            (DT, 0..=3) => 132,
            (DT2, 0) => 279,
            (DT2, 2 | 3) => 297,
            _ => return refused("Unknown preset sample layout"),
        };
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
            sound,
            data,
            sample,
        })
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
        for op in &plan.ops {
            progress("Pushing presets to +Drive", done, total);
            let (cmd, args) = match op {
                PresetOp::Move(a, b) => (0x5a, Args::default().str(&a.path()).str(&b.path())),
                PresetOp::Swap(a, b) => (0x5d, Args::default().str(&a.path()).str(&b.path())),
                PresetOp::Clear(a) => (0x5c, Args::default().str(&a.path())),
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

fn valid_bank(bank: &str) -> bool {
    bank.len() == 1 && bank.as_bytes()[0].is_ascii_uppercase()
}

fn decode_sound(model: Model, container: &[u8]) -> Result<&[u8], String> {
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
    /// Exact byte matches against the sound pool as currently edited.
    pub fn preset_pool_matches(&self, preset: &Preset, cells: &[SoundCell]) -> Vec<usize> {
        if !self.sounds_read {
            return Vec::new();
        }
        (0..library::SOUND_POOL)
            .filter(|&i| {
                self.sound_want(cells, i, &[])
                    .is_some_and(|d| d == preset.data)
            })
            .collect()
    }
}
