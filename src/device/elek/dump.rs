//! Save project and +Drive preset dumps for format research: the raw `.syx`, each
//! message's decoded payload, the names we already know how to find, and
//! every printable run with its offset. Rename something on the device, save
//! again and diff the two folders to find where a new field lives.

use super::*;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Printable runs in raw preset containers and decoded sound structures.
fn preset_strings(out: &mut String, file: &str, data: &[u8]) {
    let mut start = 0;
    while start < data.len() {
        if !(0x20..0x7f).contains(&data[start]) {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < data.len() && (0x20..0x7f).contains(&data[end]) {
            end += 1;
        }
        if end - start >= 3 {
            let _ = writeln!(out, "{file}\t0x{start:06x}\t{:?}", String::from_utf8_lossy(&data[start..end]));
        }
        start = end;
    }
}

impl Worker {
    /// Fresh reads, independent of cached/staged UI data and project loading.
    /// Keep unsupported containers too: they are useful for format research.
    pub(super) fn save_preset_dump(&mut self, root: &Path) -> Result<PathBuf, String> {
        let em = self.em.clone();
        let progress = |label: String, done, total| {
            em.send(Ev::Elek(ElekEv::PresetDumpProgress { label, done, total }));
        };
        progress("Listing preset banks".into(), 0, 0);
        let (model, banks) = self.with_conn(|c| Ok((c.model()?, c.preset_banks()?)))?;
        let dir = root.join(format!("{}_{}_presets", stamp(), model.name().replace(' ', "")));
        let io_error = |e: std::io::Error| format!("{}: {e}", dir.display());
        std::fs::create_dir_all(root).map_err(io_error)?;
        // Never silently combine two dumps taken within the same second.
        std::fs::create_dir(&dir).map_err(io_error)?;
        let result = (|| -> Result<(), String> {
            std::fs::write(dir.join("INCOMPLETE.txt"), "This dump has not finished. Files already saved are retained if the read fails.\n").map_err(io_error)?;
            for sub in ["containers", "decoded"] {
                std::fs::create_dir(dir.join(sub)).map_err(io_error)?;
            }
            std::fs::write(dir.join("banks.txt"), banks.join("\n")).map_err(io_error)?;
            let mut backup = preset_file::PresetFile::new(model, banks.clone());
            let mut entries = Vec::new();
            for bank in &banks {
                progress(format!("Listing preset bank {bank}"), 0, 0);
                for entry in self.with_conn(|c| c.preset_entries(bank))? {
                    entries.push((bank.clone(), entry));
                }
                self.serve_pending();
            }
            let mut names = format!("{} · {} banks · {} saved presets\n\n", model.name(), banks.len(), entries.len());
            for (bank, entry) in &entries {
                let _ = writeln!(names, "{bank}:{:03}\t{:?}", entry.slot, entry.name);
            }
            std::fs::write(dir.join("names.txt"), names).map_err(io_error)?;
            let mut strings_file = std::fs::File::create(dir.join("strings.tsv")).map_err(io_error)?;
            let mut info_file = std::fs::File::create(dir.join("presets.tsv")).map_err(io_error)?;
            strings_file.write_all(b"file\toffset\ttext\n").map_err(io_error)?;
            info_file.write_all(b"bank\tslot\tcontainer_bytes\tsound_offset\tsound_bytes\tversion\tname_or_error\n").map_err(io_error)?;
            let mut unsupported = 0;
            for (i, (bank, entry)) in entries.iter().enumerate() {
                let mut strings = String::new();
                let mut info = String::new();
                progress(format!("Reading preset {bank}:{:03}", entry.slot), i, entries.len());
                let path = format!("/soundbanks/{bank}/{}", entry.slot);
                let container = self.with_conn(|c| c.preset_data(&path, &|_| {}))
                    .map_err(|e| format!("{path}: {e}"))?;
                let file = format!("containers/{bank}_{:03}.bin", entry.slot);
                std::fs::write(dir.join(&file), &container).map_err(io_error)?;
                backup.add(bank, entry, &container);
                preset_strings(&mut strings, &file, &container);
                match presets::decode_sound(model, &container) {
                    Ok(data) => {
                        let file = format!("decoded/{bank}_{:03}.bin", entry.slot);
                        std::fs::write(dir.join(&file), data).map_err(io_error)?;
                        preset_strings(&mut strings, &file, data);
                        let offset = data.as_ptr() as usize - container.as_ptr() as usize;
                        let _ = writeln!(info, "{bank}\t{}\t{}\t0x{offset:x}\t{}\t{}\t{:?}",
                            entry.slot, container.len(), data.len(), be32(data, 4).unwrap_or(0), name_at(data, 12));
                    }
                    Err(e) => {
                        unsupported += 1;
                        let _ = writeln!(info, "{bank}\t{}\t{}\t\t\t\t{e:?}", entry.slot, container.len());
                    }
                }
                // Persist the indexes as we go, so partial dumps remain useful.
                info_file.write_all(info.as_bytes()).map_err(io_error)?;
                strings_file.write_all(strings.as_bytes()).map_err(io_error)?;
                progress(format!("Saved preset {bank}:{:03}", entry.slot), i + 1, entries.len());
                self.serve_pending();
            }
            backup.write(&dir.join("presets.json"))?;
            std::fs::write(dir.join("README.txt"), format!(
                "{} +Drive preset debug dump\n{} saved presets; {unsupported} unsupported sound layouts (raw containers retained).\n\n\
                 containers/  uncompressed Data API containers as received; these are not project SysEx dumps\n\
                 decoded/     supported sound structures extracted from containers\n\
                 banks.txt    all banks, including empty ones\n\
                 names.txt    saved slot locations and listing names\n\
                 presets.tsv  container sizes, sound offsets, versions, names or decode errors\n\
                 strings.tsv  printable runs of 3+ characters with file-relative byte offsets\n\n\
                 presets.json importable backup for Presets > Load presets (supported container layouts only)\n\n\
                 Fresh reads from /soundbanks; staged app edits and sample audio are not included.\n\
                 Avoid editing presets on the device while this read runs.\n\
                 Change one setting, dump again, then compare matching .bin files.\n",
                model.name(), entries.len()
            )).map_err(io_error)?;
            std::fs::remove_file(dir.join("INCOMPLETE.txt")).map_err(io_error)?;
            Ok(())
        })();
        result.map_err(|e| format!("{e} (partial dump: {})", dir.display()))?;
        Ok(dir)
    }
}

/// Where the parts of one pattern+kit message sit, for the names we know.
struct Layout {
    /// Pattern name: right after the 80 p-lock rows.
    pattern_name: usize,
    kit: usize,
    kit_name: usize,
    /// Track sounds: first one, and the stride.
    sounds: usize,
    sound_size: usize,
}

fn layout(m: Model, d: &[u8]) -> Option<Layout> {
    let pv = be32(d, 0)?;
    let kit = pattern_sizeof(m, pv)?;
    let pattern_name = pattern_plocks(m, pv)? + PLOCK_ROWS * (2 + m.steps() * 2);
    let kv_at = if m == DT2 && be32(d, kit) == Some(MAGIC) { kit + 4 } else { kit };
    let kv = be32(d, kv_at)?;
    Some(Layout {
        pattern_name,
        kit,
        kit_name: kv_at + 4,
        sounds: kit + kit_sounds(m, kv)?,
        sound_size: sound_sizeof(m, kit_sound_version(m, kv)?)?,
    })
}

/// Up to 16 chars, stopping at the first NUL.
fn name_at(d: &[u8], at: usize) -> String {
    d.get(at..at + 16)
        .map(|b| {
            b.iter()
                .take_while(|&&c| c != 0)
                .map(|&c| if (0x20..0x7F).contains(&c) { c as char } else { '·' })
                .collect()
        })
        .unwrap_or_default()
}

/// "A01".."H16".
fn pattern_label(i: u8) -> String {
    format!("{}{:02}", (b'A' + i / 16) as char, i % 16 + 1)
}

fn file_name(m: &DumpMsg) -> String {
    match m.kind {
        PATTERN_KIT => format!("pk_{:03}_{}.bin", m.index, pattern_label(m.index)),
        SOUND => format!("sound_{:03}.bin", m.index),
        SETTINGS => "settings.bin".into(),
        k => format!("msg_{k:02x}_{:03}.bin", m.index),
    }
}

/// Which part of a pattern+kit message offset `at` falls in.
fn region(l: &Layout, tracks: usize, at: usize) -> String {
    if at < l.kit {
        return format!("pattern+0x{at:x}");
    }
    let end = l.sounds + tracks * l.sound_size;
    if at >= l.sounds && at < end {
        let t = (at - l.sounds) / l.sound_size;
        let o = (at - l.sounds) % l.sound_size;
        return format!("kit track {} sound+0x{o:x}", t + 1);
    }
    format!("kit+0x{:x}", at - l.kit)
}

/// `YYYY-MM-DD_HHMMSS` in UTC.
pub(super) fn stamp() -> String {
    let s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (s.div_euclid(86400), s.rem_euclid(86400));
    // Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    format!(
        "{y:04}-{mo:02}-{d:02}_{:02}{:02}{:02}",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

impl ElekProject {
    /// Write the dump into a new timestamped folder under `root`; returns it.
    pub fn save_dump(&self, root: &Path) -> std::io::Result<PathBuf> {
        let dir = root.join(format!("{}_{}", stamp(), self.model.name().replace(' ', "")));
        let bins = dir.join("decoded");
        std::fs::create_dir_all(&bins)?;

        // Empty patterns (no note trigs; kits may be set up) are left out
        // everywhere, .syx included: they're most of a project and say
        // nothing. Unknown when the patterns didn't parse: keep them all.
        let blank = self.blank_patterns();
        let keep: Vec<&DumpMsg> = self.msgs[..self.base]
            .iter()
            .filter(|m| !(m.kind == PATTERN_KIT && blank.contains(&(m.index as usize))))
            .collect();
        let syx: Vec<u8> = keep.iter().flat_map(|m| m.raw.iter().copied()).collect();
        std::fs::write(dir.join("project.syx"), &syx)?;

        let tracks = self.model.kit_tracks();
        let mut names = String::new();
        let mut strings = String::new();
        let _ = writeln!(
            names,
            "{} · {} · {} messages\n",
            self.model.name(),
            self.versions,
            keep.len()
        );
        if !blank.is_empty() {
            let labels: Vec<String> = blank.iter().map(|&i| pattern_label(i as u8)).collect();
            let _ = writeln!(names, "{} empty patterns left out: {}\n", blank.len(), labels.join(" "));
        }
        for m in keep {
            let file = file_name(m);
            std::fs::write(bins.join(&file), &m.data)?;
            let lay = (m.kind == PATTERN_KIT).then(|| layout(self.model, &m.data)).flatten();
            match (m.kind, &lay) {
                (PATTERN_KIT, Some(l)) => {
                    let d = &m.data;
                    let _ = writeln!(
                        names,
                        "{}  pattern \"{}\"  kit \"{}\"",
                        pattern_label(m.index),
                        name_at(d, l.pattern_name),
                        name_at(d, l.kit_name)
                    );
                    let snd: Vec<String> = (0..tracks)
                        .map(|t| format!("{}:\"{}\"", t + 1, name_at(d, l.sounds + t * l.sound_size + 12)))
                        .collect();
                    let _ = writeln!(names, "     track sounds  {}", snd.join("  "));
                }
                (PATTERN_KIT, None) => {
                    let _ = writeln!(names, "{}  (unknown layout)", pattern_label(m.index));
                }
                (SOUND, _) => {
                    let _ = writeln!(names, "pool sound {:3}  \"{}\"", m.index, name_at(&m.data, 12));
                }
                _ => {}
            }
            // Printable runs of 3+ chars.
            let d = &m.data;
            let mut i = 0;
            while i < d.len() {
                if !(0x20..0x7F).contains(&d[i]) {
                    i += 1;
                    continue;
                }
                let start = i;
                while i < d.len() && (0x20..0x7F).contains(&d[i]) {
                    i += 1;
                }
                if i - start >= 3 {
                    let s = String::from_utf8_lossy(&d[start..i]);
                    let where_ = lay.as_ref().map(|l| region(l, tracks, start)).unwrap_or_default();
                    let _ = writeln!(strings, "{file}\t0x{start:06x}\t{where_}\t\"{s}\"");
                }
            }
        }
        std::fs::write(dir.join("names.txt"), names)?;
        std::fs::write(dir.join("strings.tsv"), strings)?;
        std::fs::write(
            dir.join("README.txt"),
            "project.syx   the dump as received (the device's own SysEx Dump format), empty patterns left out\n\
             decoded/      each message's payload after 7-bit decoding: offsets match elk-herd's CppStructs\n\
             names.txt     pattern, kit and per-track sound names at the offsets we know\n\
             strings.tsv   every printable run of 3+ chars: file, offset, region, text\n\n\
             To find an unknown field: change one thing on the device, save again,\n\
             then: cmp -l old/decoded/pk_000_A01.bin new/decoded/pk_000_A01.bin\n",
        )?;
        Ok(dir)
    }
}
