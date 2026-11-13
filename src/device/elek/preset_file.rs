//! Importable +Drive preset backups. JSON retains complete containers as
//! base64, bank/slot locations and empty slots within every included bank.
//! Loading only prepares local operations; the ordinary preset push checks
//! current device contents again and verifies every write/clear afterward.

use super::*;
use super::presets::{Preset, PresetEntry, PresetOp, PresetSlot};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const FORMAT: &str = "sample_manager/elk-presets";
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
pub(super) struct PresetFile {
    format: String,
    version: u32,
    model: String,
    saved: String,
    /// A bank's omitted slots are empty. Banks omitted here are untouched.
    banks: Vec<String>,
    presets: Vec<SavedPreset>,
}

#[derive(Serialize, Deserialize)]
struct SavedPreset {
    bank: String,
    slot: u32,
    name: String,
    /// Original uncompressed Data API container, including unknown fields.
    container: String,
}

pub struct LoadedPresets {
    pub banks: Vec<String>,
    pub listings: HashMap<String, Vec<PresetEntry>>,
    pub sounds: Vec<(PresetSlot, Preset)>,
    pub ops: Vec<PresetOp>,
}

impl PresetFile {
    pub(super) fn new(model: Model, banks: Vec<String>) -> Self {
        Self { format: FORMAT.into(), version: VERSION, model: model.name().into(),
            saved: dump::stamp(), banks, presets: Vec::new() }
    }

    pub(super) fn add(&mut self, bank: &str, entry: &PresetEntry, container: &[u8]) {
        self.presets.push(SavedPreset { bank: bank.into(), slot: entry.slot,
            name: entry.name.clone(), container: file::b64::encode(container) });
    }

    pub(super) fn write(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| e.to_string())
    }

    fn read(path: &Path, model: Model) -> Result<(Vec<String>, BTreeMap<PresetSlot, Preset>), String> {
        // Bound reads before allocating or decoding a file selected by mistake.
        if std::fs::metadata(path).map_err(|e| e.to_string())?.len() > 64 * 1024 * 1024 {
            return Err("Preset file exceeds 64 MB".into());
        }
        let json = std::fs::read(path).map_err(|e| e.to_string())?;
        let f: Self = serde_json::from_slice(&json).map_err(|e| format!("Not a preset backup: {e}"))?;
        if f.format != FORMAT || f.version != VERSION {
            return Err(format!("Unsupported preset file format/version: {} v{}", f.format, f.version));
        }
        if f.model != model.name() {
            return Err(format!("This backup is for {}, but the device is {}", f.model, model.name()));
        }
        let mut banks = BTreeSet::new();
        for bank in &f.banks {
            if bank.len() != 1 || !bank.as_bytes()[0].is_ascii_uppercase() || !banks.insert(bank.clone()) {
                return Err("Invalid or duplicate bank in preset file".into());
            }
        }
        if banks.is_empty() { return Err("Preset file contains no banks".into()); }
        let mut presets = BTreeMap::new();
        for saved in f.presets {
            if !banks.contains(&saved.bank) || !(1..=256).contains(&saved.slot) {
                return Err("Invalid preset location in file".into());
            }
            let loc = PresetSlot::new(&saved.bank, saved.slot);
            let container = file::b64::decode(&saved.container).ok_or("Invalid base64 preset container")?;
            if container.len() > 65536 { return Err("Preset container exceeds 64 KB".into()); }
            let preset = Preset::from_container(model, &loc, container)
                .map_err(|e| format!("{}:{:03}: {e}", loc.bank, loc.slot))?;
            if presets.insert(loc, preset).is_some() { return Err("Duplicate preset slot in file".into()); }
        }
        Ok((f.banks, presets))
    }
}

impl Worker {
    fn preset_file_progress(&self, label: String, done: usize, total: usize) {
        self.em.send(Ev::Elek(ElekEv::PresetDumpProgress { label, done, total }));
    }

    pub(super) fn save_preset_file(&mut self, path: &Path) -> Result<(), String> {
        self.preset_file_progress("Listing saved presets".into(), 0, 0);
        let (model, banks) = self.with_conn(|c| Ok((c.model()?, c.preset_banks()?)))?;
        if banks.is_empty() { return Err("No preset banks found on device".into()); }
        let mut file = PresetFile::new(model, banks.clone());
        let mut entries = Vec::new();
        for bank in &banks {
            for entry in self.with_conn(|c| c.preset_entries(bank))? { entries.push((bank.clone(), entry)); }
            self.serve_pending();
        }
        for (i, (bank, entry)) in entries.iter().enumerate() {
            self.preset_file_progress(format!("Saving preset {bank}:{:03}", entry.slot), i, entries.len());
            let loc = PresetSlot::new(bank, entry.slot);
            let container = self.with_conn(|c| c.preset_data(&format!("/soundbanks/{bank}/{}", entry.slot), &|_| {}))?;
            // Importable exports must pass the same validation as imports.
            Preset::from_container(model, &loc, container.clone())
                .map_err(|e| format!("{bank}:{:03}: {e}. Use the debug dump for unsupported formats.", entry.slot))?;
            file.add(bank, entry, &container);
            self.serve_pending();
        }
        file.write(path)
    }

    pub(super) fn load_preset_file(&mut self, path: &Path) -> Result<LoadedPresets, String> {
        self.preset_file_progress("Reading preset backup".into(), 0, 0);
        let model = self.with_conn(|c| c.model())?;
        // Validate the entire file before reading destinations or staging anything.
        let (included, mut desired) = PresetFile::read(path, model)?;
        let banks = self.with_conn(|c| c.preset_banks())?;
        if included.iter().any(|b| !banks.contains(b)) {
            return Err("A bank in the backup is unavailable on this device".into());
        }
        let mut loaded = LoadedPresets { banks, listings: HashMap::new(), sounds: Vec::new(), ops: Vec::new() };
        let mut current = BTreeMap::new();
        let mut entries = Vec::new();
        for bank in &included {
            let listing = self.with_conn(|c| c.preset_entries(bank))?;
            for entry in &listing { entries.push(PresetSlot::new(bank, entry.slot)); }
            loaded.listings.insert(bank.clone(), listing);
            self.serve_pending();
        }
        for (i, loc) in entries.into_iter().enumerate() {
            self.preset_file_progress(format!("Checking preset {}:{:03}", loc.bank, loc.slot), i, loaded.listings.values().map(Vec::len).sum());
            let container = self.with_conn(|c| c.preset_data(&format!("/soundbanks/{}/{}", loc.bank, loc.slot), &|_| {}))?;
            let data = presets::decode_sound(model, &container)?.to_vec();
            // Current containers need only be readable; only uploaded ones
            // require the supported writer layout and checksum.
            let preset = Preset { model, sound: library::parse_sound(model, loc.slot as usize, &data),
                sample: presets::sample_key(model, &data).map(|key| (key, None)), container, data };
            current.insert(loc.clone(), preset.data.clone());
            loaded.sounds.push((loc, preset));
            self.serve_pending();
        }
        for bank in included {
            for slot in 1..=256 {
                let loc = PresetSlot::new(&bank, slot);
                match (current.get(&loc), desired.remove(&loc)) {
                    (Some(old), Some(new)) if *old == new.data => {}
                    (_, Some(new)) => loaded.ops.push(PresetOp::Write(loc, new)),
                    (Some(_), None) => loaded.ops.push(PresetOp::Clear(loc)),
                    (None, None) => {}
                }
            }
        }
        Ok(loaded)
    }
}
