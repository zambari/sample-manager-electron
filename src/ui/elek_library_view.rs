//! The Sounds page (sibling of Patterns): a read-only look at what the
//! loaded project is made of, for deciding how to organise it.
//!
//! - **Sound pool** (first): the project's 128 pool sounds, read by itself
//!   the first time the tab shows with a device plugged in, with the sample
//!   each plays and the kit tracks that carry the same sound (name and
//!   sample), i.e. where it was probably loaded from the pool.
//! - **Kits**: every pattern's kit, grouped when the track sounds (name and
//!   sample) are the same, with the patterns using each and the tracks that
//!   differ from the most common kit. Digitakt II kits also show their MIDI
//!   track names.
//! - **Samples**: each pool sample and what plays it (kit tracks, sound pool,
//!   use count including sample locks), to spot unused ones.
//! - **+Drive data**: the device's data tree (`/soundbanks` for presets,
//!   `/projects`, …), folders listed when opened.
//!
//! The sound pool can be edited (with the whole project read): drag a sound
//! onto another to swap them (onto an empty one to move it), right-click to
//! rename or delete, drag a preset from the panel beside it onto an empty
//! sound. Edits sit in `LibraryView::sounds` and go out with the project
//! push (`ElekProject::build_push`), sound locks renumbered. The other tabs
//! are read-only. Data comes from `device::library` and the patterns as
//! read (`Patterns::info`).

use crate::device::library::{data_child, sounds_as_read, sounds_changed, DataItem, PoolSound, Preset, SoundCell, SoundSrc, SOUND_POOL, SRC_NAMES};
use crate::device::patterns::{self, PatState, PATTERNS};
use crate::device::ElekProject;
use crate::ui::elek_view::{self, ElekIntent, ElekState};
use crate::ui::theme::{self, OLED, OLED_DIM};
use egui::{Color32, RichText};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const ROW_H: f32 = 18.0;
const WARN: Color32 = Color32::from_rgb(230, 120, 90);
/// A kit track that differs from the most common kit.
const DIFFERS: Color32 = theme::CHANGED;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum LibTab {
    #[default]
    SoundPool,
    Kits,
    Samples,
    Drive,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum SampleFilter {
    #[default]
    All,
    Used,
    Unused,
}

#[derive(Default)]
pub struct LibraryView {
    pub tab: LibTab,
    filter: String,
    /// The sound pool as last read; cleared when another project loads.
    pub pool_sounds: Option<Result<Vec<PoolSound>, String>>,
    /// `(done, total)` while the sound pool is being read.
    pub sound_progress: Option<(usize, usize)>,
    hide_blank: bool,
    /// The sound pool as edited, by index (empty: as read).
    pub sounds: Vec<SoundCell>,
    /// Earlier `sounds`, for undo.
    sound_undo: Vec<Vec<SoundCell>>,
    /// Presets dropped on a pool sound before they were read: placed when
    /// the read lands, by path.
    pub place_after: HashMap<String, usize>,
    /// Sound-locked steps per pool sound (as read), once counted.
    lock_uses: Option<Vec<u32>>,
    /// An empty sound can be sent in place of a deleted one.
    blank_ok: Option<bool>,
    /// Renaming pool sound `.0`: the name typed so far, focus next frame.
    renaming: Option<(usize, String, bool)>,
    samples: SampleFilter,
    /// +Drive data listings by path, and those asked for.
    pub data: BTreeMap<String, Result<Vec<DataItem>, String>>,
    pub data_pending: BTreeSet<String>,
    /// Sound presets read from the +Drive, by path, and those asked for.
    pub presets: BTreeMap<String, Result<Preset, String>>,
    pub preset_pending: BTreeSet<String>,
    open_dirs: BTreeSet<String>,
    /// Kits shown with only the patterns that have something in them.
    used_kits: bool,
    /// The sound pool was read (or tried) by itself for this project.
    auto_read: bool,
}

impl LibraryView {
    /// Another project is loaded: what was read of the old one goes, and
    /// the sound pool read with it comes in.
    pub fn project_changed(&mut self, p: &ElekProject) {
        self.pool_sounds = p.pool_sounds().map(Ok);
        self.sound_progress = None;
        self.auto_read = false;
        self.sounds.clear();
        self.sound_undo.clear();
        self.place_after.clear();
        self.renaming = None;
        self.patterns_changed();
    }

    /// The patterns (or pool sounds) as read changed: recount.
    pub fn patterns_changed(&mut self) {
        self.lock_uses = None;
        self.blank_ok = None;
    }

    /// Pool sounds edited since the read.
    pub fn sound_changed(&self) -> usize {
        sounds_changed(&self.sounds)
    }

    fn cells_mut(&mut self) -> &mut Vec<SoundCell> {
        self.sound_undo.push(self.sounds.clone());
        if self.sounds.len() != SOUND_POOL {
            self.sounds = sounds_as_read();
        }
        &mut self.sounds
    }

    /// Put `cell` at pool sound `i`, as an undoable edit.
    pub fn set_sound(&mut self, i: usize, cell: SoundCell) {
        self.cells_mut()[i] = cell;
    }

    fn swap_sounds(&mut self, a: usize, b: usize) {
        self.cells_mut().swap(a, b);
    }

    fn rename_sound(&mut self, i: usize, name: String) {
        self.cells_mut()[i].name = Some(name);
    }

    fn delete_sound(&mut self, i: usize) {
        self.cells_mut()[i] = SoundCell { src: SoundSrc::Empty, name: None };
    }

    fn undo_sounds(&mut self) {
        if let Some(prev) = self.sound_undo.pop() {
            self.sounds = prev;
        }
    }
}

/// What is dragged on the sound pool: a pool sound (by index) or a preset
/// (`(data path to read it by, name)`).
#[derive(Clone)]
enum SoundDrag {
    Pool(usize),
    Preset(String, String),
}

/// Why the sound pool can't be edited now, if it can't.
fn sounds_locked(st: &ElekState) -> Option<&'static str> {
    let Some(p) = st.project.as_deref() else { return Some("no project loaded") };
    if p.read_only.is_some() {
        Some("this project is read-only")
    } else if p.partial || !p.sounds_read {
        Some("editing needs the whole project read (every pattern and the sound pool): reload it")
    } else if !p.locks_known() {
        Some("the sound locks in these patterns aren't understood (pattern version), so sounds can't be moved safely")
    } else if st.edit_locked() {
        Some("the device is busy")
    } else {
        None
    }
}

/// One kit, as found in one or more patterns.
struct KitGroup {
    pats: Vec<usize>,
    /// Kit names seen (one pattern's kit name may differ from another's
    /// with the same sounds).
    names: BTreeSet<String>,
    /// `(sound name, pool slot)` per audio track.
    tracks: Vec<(String, usize)>,
    midi: Vec<String>,
    /// Patterns in the group with trigs or other changes.
    used: usize,
}

fn kit_groups(p: &ElekProject, pats: &patterns::Patterns, used_only: bool) -> Vec<KitGroup> {
    let data = p.pattern_data();
    let mut groups: Vec<KitGroup> = Vec::new();
    let mut by_key: HashMap<(Vec<(String, usize)>, Vec<String>), usize> = HashMap::new();
    for i in 0..PATTERNS.min(pats.info.len()) {
        if !pats.loaded.get(i).copied().unwrap_or(false) {
            continue;
        }
        let info = &pats.info[i];
        let used = info.state != PatState::Blank;
        if used_only && !used {
            continue;
        }
        let tracks: Vec<(String, usize)> = info.tracks.iter().map(|t| (t.name.clone(), t.slot)).collect();
        let midi = data.get(i).copied().flatten().map(|d| crate::device::library::midi_names(p.model, d)).unwrap_or_default();
        let key = (tracks, midi);
        let gi = *by_key.entry(key.clone()).or_insert_with(|| {
            groups.push(KitGroup { pats: Vec::new(), names: BTreeSet::new(), tracks: key.0, midi: key.1, used: 0 });
            groups.len() - 1
        });
        let g = &mut groups[gi];
        g.pats.push(i);
        g.names.insert(info.kit_name.clone());
        g.used += used as usize;
    }
    groups
}

/// "A01–A16, B03" for a sorted list of pattern slots.
fn ranges(slots: &[usize]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < slots.len() {
        let mut j = i;
        while j + 1 < slots.len() && slots[j + 1] == slots[j] + 1 {
            j += 1;
        }
        out.push(if j == i {
            patterns::label(slots[i])
        } else {
            format!("{}–{}", patterns::label(slots[i]), patterns::label(slots[j]))
        });
        i = j + 1;
    }
    out.join(", ")
}

fn slot_label(slot: usize, pool_len: usize) -> String {
    if pool_len > 128 {
        format!("{}{:03}", (b'A' + (slot / 128) as u8) as char, slot % 128)
    } else {
        format!("{slot:03}")
    }
}

/// A cell of fixed width, text left-aligned and cut short to fit.
fn cell(ui: &mut egui::Ui, w: f32, text: impl Into<egui::WidgetText>) -> egui::Response {
    ui.allocate_ui_with_layout(egui::vec2(w, ROW_H), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_width(w);
        ui.add(egui::Label::new(text).truncate().sense(egui::Sense::click()))
    })
    .inner
}

fn matches(filter: &str, texts: &[&str]) -> bool {
    let f = filter.trim().to_lowercase();
    f.is_empty() || texts.iter().any(|t| t.to_lowercase().contains(&f))
}

pub(crate) fn view_tools(ui: &mut egui::Ui, st: &mut ElekState) {
    let lib = &mut st.lib;
    for (tab, label, tip) in [
        (LibTab::SoundPool, "Sound pool", "The project's 128 pool sounds, the sample each plays and the kit tracks carrying the same sound"),
        (LibTab::Kits, "Kits", "Each pattern's kit, grouped when their track sounds are the same"),
        (LibTab::Samples, "Samples", "Each pool sample and what plays it"),
        (LibTab::Drive, "+Drive data", "Sound presets (/soundbanks), saved projects and the rest of the device's data tree"),
    ] {
        ui.selectable_value(&mut lib.tab, tab, label).on_hover_text(tip);
    }
    ui.separator();
    ui.add(egui::TextEdit::singleline(&mut lib.filter).hint_text("filter").desired_width(140.0));
    if !lib.filter.is_empty() && ui.small_button(theme::CLOSE_GLYPH).clicked() {
        lib.filter.clear();
    }
    ui.separator();
    match lib.tab {
        LibTab::Kits => {
            ui.add(theme::soft_toggle(&mut lib.used_kits, "Used patterns only"))
                .on_hover_text("Leave out empty patterns, which all carry the default kit");
        }
        LibTab::Samples => {
            ui.selectable_value(&mut lib.samples, SampleFilter::All, "All");
            ui.selectable_value(&mut lib.samples, SampleFilter::Used, "Used");
            ui.selectable_value(&mut lib.samples, SampleFilter::Unused, "Unused")
                .on_hover_text("Not on any kit track, sample lock or pool sound: candidates for clearing");
        }
        LibTab::SoundPool => {
            ui.add(theme::soft_toggle(&mut lib.hide_blank, "Hide empty"));
        }
        LibTab::Drive => {}
    }
}

pub fn show(ui: &mut egui::Ui, st: &mut ElekState) -> Vec<ElekIntent> {
    let mut out = Vec::new();
    elek_view::top(ui, st, &mut out, |_, _| {});
    // Without a project the tabs still show: the +Drive tree doesn't need one.
    if st.project.is_none() {
        ui.horizontal_wrapped(|ui| view_tools(ui, st));
        if st.lib.tab != LibTab::Drive {
            elek_view::load_screen(ui, st, &mut out);
            elek_view::finish(ui, st, &mut out);
            return out;
        }
    }
    let hint = if st.lib.tab == LibTab::SoundPool && sounds_locked(st).is_none() {
        "Drag a sound onto another to swap them (onto an empty one to move it); right-click to rename or delete; drag presets from the right onto an empty sound. ⬆ Push to device sends the changes, with every pattern's sound locks following their sounds."
    } else {
        "Read-only: nothing on this tab changes the project or the device. Kits and samples are as read from the device."
    };
    ui.label(RichText::new(hint).weak().small());
    let mut ping = None;
    match st.lib.tab {
        LibTab::Kits => kits(ui, st, &mut ping, &mut out),
        LibTab::Samples => samples(ui, st, &mut ping, &mut out),
        LibTab::SoundPool => sound_pool(ui, st, &mut ping, &mut out),
        LibTab::Drive => drive(ui, st, &mut out),
    }
    if let Some(key) = ping {
        st.ping_key(|k| k == key);
        if st.preview {
            out.push(ElekIntent::Local(key, false));
        }
    }
    elek_view::finish(ui, st, &mut out);
    out
}

/// Short file name of pool slot `slot` (as read), and its key.
fn sample(p: &ElekProject, names: &HashMap<(u32, u32), String>, slot: usize) -> (String, Option<(u32, u32)>) {
    if slot == 0 {
        return ("—".into(), None);
    }
    let Some(e) = p.pool.get(slot).filter(|e| !e.is_empty()) else {
        return (format!("{} (empty slot)", slot_label(slot, p.pool.len())), None);
    };
    let name = names
        .get(&e.key())
        .map(|n| {
            let f = n.rsplit('/').next().unwrap_or(n);
            f.rsplit_once('.').map_or(f, |(stem, _)| stem).to_owned()
        })
        .unwrap_or_else(|| "?".into());
    (format!("{} {name}", slot_label(slot, p.pool.len())), Some(e.key()))
}

fn kits(ui: &mut egui::Ui, st: &mut ElekState, ping: &mut Option<(u32, u32)>, _out: &mut [ElekIntent]) {
    let Some(p) = st.project.as_deref() else { return };
    let pats = match &p.patterns {
        Ok(pats) => pats,
        Err(e) => {
            ui.colored_label(WARN, format!("Kits unavailable: {e}"));
            return;
        }
    };
    let groups = kit_groups(p, pats, st.lib.used_kits);
    let n = pats.loaded_count();
    let mut summary = format!("{} distinct kit{} in {} pattern{}", groups.len(), if groups.len() == 1 { "" } else { "s" }, groups.iter().map(|g| g.pats.len()).sum::<usize>(), if n == 1 { "" } else { "s" });
    if n < PATTERNS {
        summary.push_str(&format!(" ({n} of {PATTERNS} patterns read)"));
    }
    ui.label(RichText::new(summary).color(OLED));
    // Tracks are compared with the kit most patterns use.
    let common = groups.iter().enumerate().max_by_key(|(_, g)| g.pats.len()).map(|(i, _)| i);
    let filter = st.lib.filter.clone();
    egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
        for (gi, g) in groups.iter().enumerate() {
            let names: Vec<&str> = g.names.iter().map(String::as_str).filter(|n| !n.is_empty()).collect();
            let mut texts: Vec<&str> = names.clone();
            texts.extend(g.tracks.iter().map(|t| t.0.as_str()));
            texts.extend(g.midi.iter().map(String::as_str));
            let pat_list = ranges(&g.pats);
            texts.push(&pat_list);
            if !matches(&filter, &texts) {
                continue;
            }
            let title = if names.is_empty() { "(no kit name)".to_owned() } else { names.join(" / ") };
            let header = format!(
                "{title}  ·  {} pattern{}{}",
                g.pats.len(),
                if g.pats.len() == 1 { "" } else { "s" },
                if Some(gi) == common { "  ·  most common" } else { "" }
            );
            egui::CollapsingHeader::new(RichText::new(header).color(OLED))
                .id_salt(("kit", gi))
                .default_open(groups.len() <= 6)
                .show(ui, |ui| {
                    ui.label(RichText::new(format!("Patterns: {pat_list}")).weak());
                    ui.label(RichText::new(format!("{} with trigs or changes", g.used)).weak().small());
                    let base = common.filter(|&c| c != gi).map(|c| &groups[c].tracks);
                    for (t, (name, slot)) in g.tracks.iter().enumerate() {
                        let differs = base.is_some_and(|b| b.get(t) != Some(&(name.clone(), *slot)));
                        let color = if differs { DIFFERS } else { OLED };
                        ui.horizontal(|ui| {
                            cell(ui, 40.0, RichText::new(format!("T{}", t + 1)).color(OLED_DIM));
                            cell(ui, 150.0, RichText::new(name).color(color));
                            let (s, key) = sample(p, &st.names, *slot);
                            let r = cell(ui, 320.0, RichText::new(s).color(if *slot == 0 { OLED_DIM } else { color }));
                            if differs {
                                r.clone().on_hover_text("Differs from the most common kit");
                            }
                            if let (true, Some(k)) = (r.clicked(), key) {
                                *ping = Some(k);
                            }
                        });
                    }
                    if g.midi.iter().any(|m| !m.is_empty()) {
                        ui.add_space(2.0);
                        let custom: Vec<String> = g
                            .midi
                            .iter()
                            .enumerate()
                            .filter(|(t, m)| **m != format!("MIDI {}", t + 1))
                            .map(|(t, m)| format!("{}: {m}", t + 1))
                            .collect();
                        let text = if custom.is_empty() { "MIDI tracks: default names".to_owned() } else { format!("MIDI tracks: {}", custom.join("  ")) };
                        ui.label(RichText::new(text).weak().small());
                    }
                });
        }
    });
}

struct SampleRow {
    slot: usize,
    name: String,
    key: (u32, u32),
    /// Patterns with a kit track on this sample, and how many tracks.
    kit_pats: Vec<usize>,
    kit_tracks: usize,
    sounds: Vec<usize>,
    uses: u32,
    size: u32,
}

fn samples(ui: &mut egui::Ui, st: &mut ElekState, ping: &mut Option<(u32, u32)>, _out: &mut [ElekIntent]) {
    let Some(p) = st.project.as_deref() else { return };
    let mut rows: Vec<SampleRow> = p
        .pool
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, e)| !e.is_empty())
        .map(|(slot, e)| SampleRow {
            slot,
            name: sample(p, &st.names, slot).0,
            key: e.key(),
            kit_pats: Vec::new(),
            kit_tracks: 0,
            sounds: Vec::new(),
            uses: p.uses.get(slot).copied().unwrap_or(0),
            size: e.size.saturating_sub(16),
        })
        .collect();
    let at: HashMap<usize, usize> = rows.iter().enumerate().map(|(i, r)| (r.slot, i)).collect();
    if let Ok(pats) = &p.patterns {
        for (i, info) in pats.info.iter().enumerate().take(PATTERNS) {
            if !pats.loaded.get(i).copied().unwrap_or(false) {
                continue;
            }
            for t in &info.tracks {
                if let Some(&ri) = at.get(&t.slot) {
                    let r = &mut rows[ri];
                    r.kit_tracks += 1;
                    if r.kit_pats.last() != Some(&i) {
                        r.kit_pats.push(i);
                    }
                }
            }
        }
    }
    if let Some(Ok(sounds)) = &st.lib.pool_sounds {
        for s in sounds.iter().filter(|s| !s.blank) {
            if let Some(&ri) = at.get(&s.slot) {
                rows[ri].sounds.push(s.index);
            }
        }
    }
    let total = rows.len();
    let unused = rows.iter().filter(|r| r.uses == 0 && r.sounds.is_empty()).count();
    let unused_bytes: u64 = rows.iter().filter(|r| r.uses == 0 && r.sounds.is_empty()).map(|r| r.size as u64).sum();
    let filter = st.lib.filter.clone();
    let want = st.lib.samples;
    rows.retain(|r| {
        let used = r.uses > 0 || !r.sounds.is_empty();
        let ok = match want {
            SampleFilter::All => true,
            SampleFilter::Used => used,
            SampleFilter::Unused => !used,
        };
        ok && matches(&filter, &[&r.name])
    });
    ui.label(RichText::new(format!(
        "{total} samples in the pool, {unused} not played by anything ({:.1} MB){}",
        unused_bytes as f64 / (1 << 20) as f64,
        if st.lib.pool_sounds.is_none() { " · sound pool not read yet: its samples count as unused here" } else { "" }
    ))
    .color(OLED));
    if p.partial {
        ui.colored_label(WARN, "Not every pattern was read: use counts are short.");
    }
    ui.horizontal(|ui| {
        cell(ui, 300.0, RichText::new("Sample").small().weak());
        cell(ui, 60.0, RichText::new("Uses").small().weak()).on_hover_text("Pattern tracks that play it (kit sound or sample locks), plus pool sounds");
        cell(ui, 80.0, RichText::new("Kit tracks").small().weak());
        cell(ui, 200.0, RichText::new("Kit patterns").small().weak());
        cell(ui, 90.0, RichText::new("Pool sounds").small().weak());
        cell(ui, 70.0, RichText::new("Size").small().weak());
    });
    egui::ScrollArea::vertical().auto_shrink([false; 2]).show_rows(ui, ROW_H, rows.len(), |ui, range| {
        for r in &rows[range] {
            ui.horizontal(|ui| {
                let used = r.uses > 0 || !r.sounds.is_empty();
                let color = if used { OLED } else { OLED_DIM };
                if cell(ui, 300.0, RichText::new(&r.name).color(color)).on_hover_text("Click: point it out in the docked pool (and play it with Preview on click)").clicked() {
                    *ping = Some(r.key);
                }
                cell(ui, 60.0, RichText::new(r.uses.to_string()).color(color));
                cell(ui, 80.0, RichText::new(if r.kit_tracks > 0 { r.kit_tracks.to_string() } else { "—".into() }).color(color));
                let pl = ranges(&r.kit_pats);
                cell(ui, 200.0, RichText::new(if pl.is_empty() { "—".into() } else { pl.clone() }).color(color)).on_hover_text(pl);
                let sl: Vec<String> = r.sounds.iter().map(|s| format!("{:03}", s + 1)).collect();
                cell(ui, 90.0, RichText::new(if sl.is_empty() { "—".into() } else { sl.join(" ") }).color(color));
                cell(ui, 70.0, RichText::new(format!("{:.0} KB", r.size as f64 / 1024.0)).color(color));
            });
        }
    });
}

/// Kit tracks carrying pool sound `s` (same name and sample), by pattern:
/// `(pattern, track)` pairs. Empty for a sound without a name.
fn kit_uses(p: &ElekProject, s: &PoolSound) -> Vec<(usize, usize)> {
    let Ok(pats) = &p.patterns else { return Vec::new() };
    if s.name.trim().is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (i, info) in pats.info.iter().enumerate().take(PATTERNS) {
        if !pats.loaded.get(i).copied().unwrap_or(false) {
            continue;
        }
        for (t, tr) in info.tracks.iter().enumerate() {
            if tr.slot == s.slot && tr.name == s.name {
                out.push((i, t));
            }
        }
    }
    out
}

/// SRC page columns shown (SMP is the sample column).
const SRC_SHOWN: [usize; 7] = [0, 1, 2, 4, 5, 6, 7];
const SRC_W: f32 = 40.0;

/// One SRC value as the device shows it: TUNE around 64, the rest raw.
fn src_text(i: usize, (v, _fine): (u8, u8)) -> String {
    if i == 0 {
        let t = v as i32 - 64;
        if t > 0 { format!("+{t}") } else { t.to_string() }
    } else {
        v.to_string()
    }
}

/// The whole SRC page, with fine bytes, for a tooltip.
fn src_detail(src: &[(u8, u8)]) -> String {
    if src.is_empty() {
        return "SRC page layout not known for this sound version".into();
    }
    let vals: Vec<String> = src.iter().zip(SRC_NAMES).map(|((v, f), n)| format!("{n} {v} (fine {f})")).collect();
    format!("SRC page as stored (read as a one-shot sound; other Digitakt II machines use these places for their own parameters):\n{}", vals.join("\n"))
}

/// Pool sounds with the same name and sample file as `s`.
fn same_in_pool(pool: Option<&Vec<PoolSound>>, s: &PoolSound) -> Vec<usize> {
    pool.map(|p| p.iter().filter(|q| !q.blank && q.name == s.name && q.file_hash == s.file_hash).map(|q| q.index).collect()).unwrap_or_default()
}

fn sound_pool(ui: &mut egui::Ui, st: &mut ElekState, ping: &mut Option<(u32, u32)>, out: &mut Vec<ElekIntent>) {
    let present = crate::device::presence().available();
    let reading = st.lib.sound_progress.is_some();
    // First look at this project's sound pool: read it without being asked.
    if present && !reading && !st.lib.auto_read && st.lib.pool_sounds.is_none() && st.busy.is_none() && st.project.is_some() {
        st.lib.auto_read = true;
        out.push(ElekIntent::ReadSounds);
    }
    let locked = sounds_locked(st);
    let editable = locked.is_none();
    if editable && st.lib.lock_uses.is_none() {
        if let Some(p) = st.project.as_deref() {
            st.lib.lock_uses = Some(p.sound_lock_uses());
            st.lib.blank_ok = Some(p.blank_sound().is_some());
        }
    }
    ui.horizontal(|ui| {
        if editable {
            let lib = &mut st.lib;
            let changed = lib.sound_changed();
            if ui.add_enabled(!lib.sound_undo.is_empty(), egui::Button::new("↶ Undo")).on_hover_text("Undo the last sound pool edit").clicked() {
                lib.undo_sounds();
            }
            if ui.add_enabled(changed > 0, egui::Button::new("↺ Reset sounds")).on_hover_text("Back to the sound pool as read").clicked() {
                lib.sound_undo.push(lib.sounds.clone());
                lib.sounds.clear();
            }
            if changed > 0 {
                ui.label(RichText::new(format!("{changed} pool sound(s) changed: ⬆ Push to device sends them")).color(DIFFERS));
            }
        } else {
            let label = if st.lib.pool_sounds.is_some() { "⟳ Read sound pool again" } else { "⬇ Read sound pool" };
            if ui
                .add_enabled(present && !reading && st.busy.is_none(), egui::Button::new(label))
                .on_hover_text("Asks the device for each of the project's 128 pool sounds. Read-only")
                .clicked()
            {
                out.push(ElekIntent::ReadSounds);
            }
            if let Some(why) = locked {
                ui.label(RichText::new(format!("Not editable: {why}")).weak().small());
            }
        }
        if let Some((done, total)) = st.lib.sound_progress {
            ui.add(theme::progress_bar(done as f32 / total.max(1) as f32).desired_width(200.0).text(format!("{done}/{total}")));
        }
    });
    let Some(p) = st.project.as_deref() else { return };
    let sounds: Vec<PoolSound> = if editable {
        p.edited_sounds(&st.lib.sounds)
    } else {
        match &st.lib.pool_sounds {
            None if reading => {
                ui.label(RichText::new("Reading the sound pool…").weak());
                return;
            }
            None => {
                ui.label(RichText::new(if present { "Not read yet." } else { "Not read yet. Plug the device in to read it." }).weak());
                return;
            }
            Some(Err(e)) => {
                ui.colored_label(WARN, e);
                return;
            }
            Some(Ok(s)) => s.clone(),
        }
    };
    let filled = sounds.iter().filter(|s| !s.blank).count();
    let with_sample = sounds.iter().filter(|s| !s.blank && s.slot != 0).count();
    ui.label(RichText::new(format!("{filled} of {SOUND_POOL} sounds in use, {with_sample} with a sample")).color(OLED));
    if p.partial {
        ui.colored_label(WARN, "Not every pattern was read: the kit column is short.");
    }
    // The pool on the left, presets to drag in on the right.
    let avail = ui.available_rect_before_wrap();
    let side = if editable { 280.0f32.min(avail.width() * 0.4) } else { 0.0 };
    let left = egui::Rect::from_min_max(avail.min, egui::pos2(avail.max.x - side, avail.max.y));
    let mut lui = ui.new_child(egui::UiBuilder::new().max_rect(left).layout(egui::Layout::top_down(egui::Align::Min)));
    let drop = sound_rows(&mut lui, st, &sounds, editable, ping);
    if editable {
        let right = egui::Rect::from_min_max(egui::pos2(avail.max.x - side + 8.0, avail.min.y), avail.max);
        let mut rui = ui.new_child(egui::UiBuilder::new().max_rect(right).layout(egui::Layout::top_down(egui::Align::Min)));
        presets_panel(&mut rui, st, out);
    }
    ui.allocate_rect(avail, egui::Sense::hover());
    // The dragged thing follows the pointer.
    let ctx = ui.ctx().clone();
    if let (Some(d), Some(pos)) = (egui::DragAndDrop::payload::<SoundDrag>(&ctx), ctx.pointer_latest_pos()) {
        let label = match &*d {
            SoundDrag::Pool(i) => format!("{:03} {}", i + 1, sounds.get(*i).map(|s| s.name.as_str()).unwrap_or("")),
            SoundDrag::Preset(_, name) => format!("preset {name}"),
        };
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("sound_drag")));
        let font = egui::FontId::proportional(13.0);
        let galley = painter.layout_no_wrap(label, font, OLED);
        let r = egui::Rect::from_min_size(pos + egui::vec2(14.0, -8.0), galley.size() + egui::vec2(8.0, 4.0));
        painter.rect_filled(r, 2.0, theme::OLED_BG);
        painter.galley(r.min + egui::vec2(4.0, 2.0), galley, OLED);
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    if let Some((what, to)) = drop {
        drop_sound(st, what, to, &sounds, out);
    }
    rename_modal(ui.ctx(), st);
}

/// The pool sound rows. Returns what was dropped on which row.
fn sound_rows(ui: &mut egui::Ui, st: &mut ElekState, sounds: &[PoolSound], editable: bool, ping: &mut Option<(u32, u32)>) -> Option<(SoundDrag, usize)> {
    let p = st.project.as_deref()?;
    let filter = st.lib.filter.clone();
    let lock_uses = st.lib.lock_uses.clone().unwrap_or_default();
    let blank_ok = st.lib.blank_ok.unwrap_or(false);
    let cells = st.lib.sounds.clone();
    let cell_of = |i: usize| cells.get(i).cloned().unwrap_or_else(|| SoundCell::as_read(i));
    // Locks follow the sound: count them by where it was read.
    let locks_of = |i: usize| match cell_of(i).src {
        SoundSrc::Read(o) if editable => lock_uses.get(o as usize).copied().unwrap_or(0),
        _ => 0,
    };
    let shown: Vec<(&PoolSound, Vec<(usize, usize)>)> = sounds
        .iter()
        .filter(|s| !st.lib.hide_blank || !s.blank)
        .filter(|s| {
            let n = sample(p, &st.names, s.slot).0;
            matches(&filter, &[&s.name, &n])
        })
        .map(|s| (s, kit_uses(p, s)))
        .collect();
    ui.horizontal(|ui| {
        cell(ui, 50.0, RichText::new("#").small().weak());
        cell(ui, 160.0, RichText::new("Sound").small().weak());
        cell(ui, 220.0, RichText::new("Sample").small().weak());
        for i in SRC_SHOWN {
            cell(ui, SRC_W, RichText::new(SRC_NAMES[i]).small().weak())
                .on_hover_text("SRC page, as stored. Read as a one-shot sound: on the Digitakt II other machines keep their own parameters here");
        }
        cell(ui, 60.0, RichText::new("Kit tracks").small().weak())
            .on_hover_text("Kit tracks with the same sound name and sample: probably loaded from this pool sound (or saved into it)");
        cell(ui, 140.0, RichText::new("In patterns").small().weak());
        if editable {
            cell(ui, 50.0, RichText::new("Locks").small().weak()).on_hover_text("Steps sound-locked to this pool sound, over all patterns. They follow it when it moves");
        }
    });
    let mut dropped = None;
    let mut menu: Option<(usize, MenuPick)> = None;
    egui::ScrollArea::vertical().id_salt("sound_pool_rows").auto_shrink([false; 2]).show_rows(ui, ROW_H, shown.len(), |ui, range| {
        for (s, uses) in &shown[range] {
            let edited = editable && cell_of(s.index) != SoundCell::as_read(s.index);
            let row = ui.horizontal(|ui| {
                let color = if s.blank { OLED_DIM } else if edited { DIFFERS } else { OLED };
                cell(ui, 50.0, RichText::new(format!("{:03}", s.index + 1)).color(OLED_DIM));
                cell(ui, 160.0, RichText::new(if s.name.is_empty() { "—" } else { &s.name }).color(color)).on_hover_text(src_detail(&s.src));
                let (n, key) = sample(p, &st.names, s.slot);
                let r = cell(ui, 220.0, RichText::new(n).color(if s.slot == 0 { OLED_DIM } else { color }));
                if let (true, Some(k)) = (r.on_hover_text("Click: point it out in the docked pool").clicked(), key) {
                    *ping = Some(k);
                }
                for i in SRC_SHOWN {
                    let t = s.src.get(i).map(|&v| src_text(i, v)).unwrap_or_default();
                    cell(ui, SRC_W, RichText::new(t).color(color));
                }
                cell(ui, 60.0, RichText::new(if uses.is_empty() { "—".into() } else { uses.len().to_string() }).color(color));
                let mut pats: Vec<usize> = uses.iter().map(|u| u.0).collect();
                pats.dedup();
                let pl = ranges(&pats);
                let detail: Vec<String> = uses.iter().map(|(i, t)| format!("{} T{}", patterns::label(*i), t + 1)).collect();
                let r = cell(ui, 140.0, RichText::new(if pl.is_empty() { "—".into() } else { pl }).color(color));
                if !detail.is_empty() {
                    r.on_hover_text(detail.join(", "));
                }
                if editable {
                    let n = locks_of(s.index);
                    cell(ui, 50.0, RichText::new(if n == 0 { "—".into() } else { n.to_string() }).color(color));
                }
            });
            if !editable {
                continue;
            }
            // The row as a whole: dragged by its number and name, dropped
            // on anywhere, right-clicked anywhere.
            let rect = row.response.rect;
            let grip = egui::Rect::from_min_max(rect.min, egui::pos2(rect.min.x + 210.0, rect.max.y));
            let id = ui.id().with(("pool_sound", s.index));
            let r = ui.interact(grip, id, egui::Sense::click_and_drag());
            if r.drag_started() && !s.blank {
                egui::DragAndDrop::set_payload(ui.ctx(), SoundDrag::Pool(s.index));
            }
            let over = ui.interact(rect, id.with("drop"), egui::Sense::hover());
            if let Some(d) = over.dnd_hover_payload::<SoundDrag>() {
                let ok = match &*d {
                    SoundDrag::Pool(from) => *from != s.index,
                    SoundDrag::Preset(..) => s.blank,
                };
                let c = if ok { theme::DEVICE_ACCENT_BRIGHT } else { WARN };
                ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(1.0, c), egui::StrokeKind::Inside);
            }
            if let Some(d) = over.dnd_release_payload::<SoundDrag>() {
                dropped = Some(((*d).clone(), s.index));
            }
            if !s.blank {
                r.on_hover_text("Drag onto another sound to swap them, or onto an empty one to move it. Right-click to rename or delete").context_menu(|ui| {
                    ui.label(RichText::new(format!("Pool sound {:03} “{}”", s.index + 1, s.name)).weak());
                    if ui.button("Rename…").clicked() {
                        menu = Some((s.index, MenuPick::Rename));
                        ui.close();
                    }
                    let locks = locks_of(s.index);
                    let tip = if !blank_ok {
                        "No empty sound to send in its place: this project has no empty pattern or empty pool sound to copy one from".to_owned()
                    } else if locks > 0 {
                        format!("{locks} sound-locked step(s) in the patterns point at it; those locks are removed")
                    } else {
                        "Empties this pool sound. Kit tracks keep their own copies".to_owned()
                    };
                    let label = if locks > 0 { format!("Delete ({locks} locks removed)") } else { "Delete".to_owned() };
                    if ui.add_enabled(blank_ok, egui::Button::new(label)).on_hover_text(tip.clone()).on_disabled_hover_text(tip).clicked() {
                        menu = Some((s.index, MenuPick::Delete));
                        ui.close();
                    }
                });
            }
        }
    });
    match menu {
        Some((i, MenuPick::Rename)) => {
            let name = sounds.get(i).map(|s| s.name.clone()).unwrap_or_default();
            st.lib.renaming = Some((i, name, true));
        }
        Some((i, MenuPick::Delete)) => {
            let name = sounds.get(i).map(|s| s.name.clone()).unwrap_or_default();
            st.lib.delete_sound(i);
            st.status = format!("Pool sound {:03} “{name}” deleted. Push to send it", i + 1);
        }
        None => {}
    }
    dropped
}

enum MenuPick {
    Rename,
    Delete,
}

/// A sound or preset let go over pool sound `to`.
fn drop_sound(st: &mut ElekState, what: SoundDrag, to: usize, sounds: &[PoolSound], out: &mut Vec<ElekIntent>) {
    let blank = sounds.get(to).is_none_or(|s| s.blank);
    match what {
        SoundDrag::Pool(from) if from == to => {}
        SoundDrag::Pool(from) => {
            st.lib.swap_sounds(from, to);
            let name = sounds.get(from).map(|s| s.name.clone()).unwrap_or_default();
            st.status = if blank {
                format!("“{name}” moved {:03} → {:03}. Push to send it", from + 1, to + 1)
            } else {
                format!("Swapped {:03} and {:03}. Push to send it", from + 1, to + 1)
            };
        }
        SoundDrag::Preset(_, name) if !blank => {
            st.status = format!("Drop “{name}” on an empty sound (delete this one first to replace it)");
        }
        SoundDrag::Preset(path, name) => match st.lib.presets.get(&path).cloned() {
            Some(Ok(pr)) => st.place_preset(to, &path, &pr),
            Some(Err(e)) => st.status = format!("Couldn't read “{name}”: {e}"),
            None => {
                st.lib.place_after.insert(path.clone(), to);
                if st.lib.preset_pending.insert(path.clone()) {
                    out.push(ElekIntent::ReadPreset(path));
                }
                st.status = format!("Reading “{name}” from the +Drive…");
            }
        },
    }
}

fn rename_modal(ctx: &egui::Context, st: &mut ElekState) {
    let Some((i, text, focus)) = st.lib.renaming.as_mut() else { return };
    let i = *i;
    let mut done: Option<bool> = None;
    egui::Modal::new(egui::Id::new("pool_sound_rename")).show(ctx, |ui| {
        ui.heading(format!("Rename pool sound {:03}", i + 1));
        ui.add_space(4.0);
        let r = ui.add(egui::TextEdit::singleline(text).char_limit(patterns::NAME_MAX).desired_width(220.0));
        if std::mem::take(focus) {
            r.request_focus();
        }
        if r.lost_focus() && ui.input(|x| x.key_pressed(egui::Key::Enter)) {
            done = Some(true);
        }
        ui.label(RichText::new("Up to 15 characters, shown in capitals as on the device. Kit tracks loaded from it keep their old name.").weak().small());
        ui.horizontal(|ui| {
            if ui.button("Rename").clicked() {
                done = Some(true);
            }
            if ui.button("Cancel").clicked() || ui.input(|x| x.key_pressed(egui::Key::Escape)) {
                done = Some(false);
            }
        });
    });
    match done {
        Some(true) => {
            let name = patterns::clean_name(text);
            st.lib.renaming = None;
            st.lib.rename_sound(i, name.clone());
            st.status = format!("Pool sound {:03} renamed “{name}”. Push to send it", i + 1);
        }
        Some(false) => st.lib.renaming = None,
        None => {}
    }
}

/// The presets on the +Drive (`/soundbanks`), to drag onto an empty sound.
fn presets_panel(ui: &mut egui::Ui, st: &mut ElekState, out: &mut Vec<ElekIntent>) {
    const ROOT: &str = "/soundbanks";
    let lib = &mut st.lib;
    let present = crate::device::presence().available();
    if present && !lib.data.contains_key(ROOT) && lib.data_pending.insert(ROOT.into()) {
        out.push(ElekIntent::ListData(ROOT.into()));
    }
    ui.label(RichText::new("Presets").color(OLED));
    ui.label(RichText::new("Drag one onto an empty sound. Its sample is found in the sample pool, or goes into the first empty slot.").weak().small());
    let mut rows: Vec<DataRow> = Vec::new();
    walk(lib, ROOT, 0, &mut rows);
    let mut toggle = None;
    let mut read = None;
    egui::ScrollArea::vertical().id_salt("sound_presets").auto_shrink([false; 2]).show_rows(ui, ROW_H, rows.len(), |ui, range| {
        for (depth, path, item, note) in &rows[range] {
            ui.horizontal(|ui| {
                ui.add_space(*depth as f32 * 14.0);
                match (item, note) {
                    (_, Some(n)) => {
                        cell(ui, 200.0, RichText::new(n).weak().italics());
                    }
                    (Some(it), None) if it.dir => {
                        let open = lib.open_dirs.contains(path);
                        if cell(ui, 220.0, RichText::new(format!("{} {}", if open { "▾" } else { "▸" }, it.name)).color(theme::DEVICE_ACCENT_BRIGHT)).clicked() {
                            toggle = Some(path.clone());
                        }
                    }
                    (Some(it), None) => {
                        let at = item_path(path, it);
                        let state = if lib.preset_pending.contains(&at) {
                            "…"
                        } else {
                            match lib.presets.get(&at) {
                                Some(Ok(p)) if p.sound.is_some() => "✓",
                                Some(_) => "!",
                                None => "",
                            }
                        };
                        let r = ui.add(egui::Label::new(RichText::new(format!("{:03} {}", it.slot, it.name)).color(OLED)).truncate().selectable(false).sense(egui::Sense::click_and_drag()));
                        ui.label(RichText::new(state).weak());
                        if r.drag_started() {
                            egui::DragAndDrop::set_payload(ui.ctx(), SoundDrag::Preset(at.clone(), it.name.clone()));
                        }
                        let tip = match lib.presets.get(&at) {
                            Some(Ok(p)) => match &p.sound {
                                Some(s) => format!("“{}”: {}", s.name, src_detail(&s.src)),
                                None => format!("{} bytes, not a sound struct this app knows", p.bytes),
                            },
                            Some(Err(e)) => e.clone(),
                            None => "Drag onto an empty pool sound. Click to read it first".into(),
                        };
                        if r.on_hover_text(tip).clicked() && present && !lib.preset_pending.contains(&at) {
                            read = Some(at);
                        }
                    }
                    (None, None) => {}
                }
            });
        }
    });
    if let Some(path) = read {
        lib.preset_pending.insert(path.clone());
        out.push(ElekIntent::ReadPreset(path));
    }
    if let Some(path) = toggle {
        if !lib.open_dirs.remove(&path) {
            lib.open_dirs.insert(path.clone());
            if !lib.data.contains_key(&path) && lib.data_pending.insert(path.clone()) {
                out.push(ElekIntent::ListData(path));
            }
        }
    }
}

/// A row of the +Drive data tree: depth, path, the item, or a note
/// (listing…, empty, error) in place of one.
type DataRow = (usize, String, Option<DataItem>, Option<String>);

/// The open part of the data tree under `path`, flattened.
fn walk(lib: &LibraryView, path: &str, depth: usize, rows: &mut Vec<DataRow>) {
    match lib.data.get(path) {
        None => {
            let msg = if lib.data_pending.contains(path) { "listing…" } else { "not listed" };
            rows.push((depth, path.to_owned(), None, Some(msg.into())));
        }
        Some(Err(e)) => rows.push((depth, path.to_owned(), None, Some(e.clone()))),
        Some(Ok(items)) if items.is_empty() => rows.push((depth, path.to_owned(), None, Some("empty".into()))),
        Some(Ok(items)) => {
            for it in items {
                let child = data_child(path, &it.name);
                rows.push((depth, child.clone(), Some(it.clone()), None));
                if it.dir && lib.open_dirs.contains(&child) {
                    walk(lib, &child, depth + 1, rows);
                }
            }
        }
    }
}

/// The path a data item is read by: its folder and slot number (items
/// are read by slot, not name).
fn item_path(path: &str, it: &DataItem) -> String {
    data_child(path.rsplit_once('/').map_or("/", |(d, _)| if d.is_empty() { "/" } else { d }), &it.slot.to_string())
}

fn drive(ui: &mut egui::Ui, st: &mut ElekState, out: &mut Vec<ElekIntent>) {
    let lib = &mut st.lib;
    let present = crate::device::presence().available();
    if present && !lib.data.contains_key("/") && !lib.data_pending.contains("/") {
        out.push(ElekIntent::ListData("/".into()));
        lib.data_pending.insert("/".into());
    }
    ui.horizontal(|ui| {
        if ui.add_enabled(present, egui::Button::new("⟳ Refresh")).on_hover_text("List the open folders again").clicked() {
            let open: Vec<String> = std::iter::once("/".to_owned()).chain(lib.open_dirs.iter().cloned()).collect();
            lib.data.clear();
            for p in open {
                if lib.data_pending.insert(p.clone()) {
                    out.push(ElekIntent::ListData(p));
                }
            }
        }
        ui.label(RichText::new("The device's data storage: sound presets (/soundbanks), saved projects and whatever else it lists. Click a folder to open it.").weak().small());
    });
    if !present && lib.data.is_empty() {
        ui.label(RichText::new("Plug the device in to list its data.").weak());
        return;
    }
    // Flatten the open part of the tree, then draw it.
    let mut rows: Vec<DataRow> = Vec::new();
    walk(lib, "/", 0, &mut rows);
    let filter = lib.filter.clone();
    let mut toggle = None;
    let mut read: Option<String> = None;
    let pool = match &lib.pool_sounds {
        Some(Ok(p)) => Some(p.clone()),
        _ => None,
    };
    let pool = pool.as_ref();
    egui::ScrollArea::vertical().auto_shrink([false; 2]).show_rows(ui, ROW_H, rows.len(), |ui, range| {
        for (depth, path, item, note) in &rows[range] {
            ui.horizontal(|ui| {
                ui.add_space(*depth as f32 * 18.0);
                match (item, note) {
                    (_, Some(n)) => {
                        cell(ui, 400.0, RichText::new(n).weak().italics());
                    }
                    (Some(it), None) if it.dir => {
                        let open = lib.open_dirs.contains(path);
                        let label = format!("{} {}", if open { "▾" } else { "▸" }, it.name);
                        if cell(ui, 400.0, RichText::new(label).color(theme::DEVICE_ACCENT_BRIGHT)).clicked() {
                            toggle = Some(path.clone());
                        }
                    }
                    (Some(it), None) => {
                        let color = if matches(&filter, &[&it.name]) { OLED } else { OLED_DIM };
                        cell(ui, 50.0, RichText::new(format!("{:03}", it.slot)).color(OLED_DIM));
                        let preset = path.starts_with("/soundbanks/");
                        let r = cell(ui, 250.0, RichText::new(&it.name).color(color));
                        cell(ui, 90.0, RichText::new(format!("{:.1} KB", it.size as f64 / 1024.0)).color(OLED_DIM));
                        if preset {
                            let item = item_path(path, it);
                            if r.on_hover_text("Click: read this preset (read-only) and compare it with the sound pool").clicked() && present && !lib.preset_pending.contains(&item) {
                                read = Some(item.clone());
                            }
                            preset_cell(ui, lib, &item, pool);
                        }
                    }
                    (None, None) => {}
                }
            });
        }
    });
    if let Some(path) = read {
        lib.preset_pending.insert(path.clone());
        out.push(ElekIntent::ReadPreset(path));
    }
    if let Some(path) = toggle {
        if !lib.open_dirs.remove(&path) {
            lib.open_dirs.insert(path.clone());
            if !lib.data.contains_key(&path) && lib.data_pending.insert(path.clone()) {
                out.push(ElekIntent::ListData(path));
            }
        }
    }
}

/// What a read preset holds, beside its row: name, SRC page, and the pool
/// sounds it matches (same name and sample file).
fn preset_cell(ui: &mut egui::Ui, lib: &LibraryView, item: &str, pool: Option<&Vec<PoolSound>>) {
    if lib.preset_pending.contains(item) {
        ui.label(RichText::new("reading…").weak().italics());
        return;
    }
    match lib.presets.get(item) {
        None => {}
        Some(Err(e)) => {
            ui.colored_label(WARN, e);
        }
        Some(Ok(p)) => {
            let saved = p.saved.as_ref().map(|f| format!("\nSaved to {}", f.display())).unwrap_or_default();
            let tip = format!("{} bytes, sound struct at {:?}. Starts: {}{saved}", p.bytes, p.at, p.head);
            let Some(s) = &p.sound else {
                ui.label(RichText::new(format!("{} bytes, not a sound struct we know", p.bytes)).color(WARN)).on_hover_text(tip);
                return;
            };
            let vals: Vec<String> = SRC_SHOWN.iter().filter_map(|&i| Some(format!("{} {}", SRC_NAMES[i], src_text(i, *s.src.get(i)?)))).collect();
            let same = same_in_pool(pool, s);
            let pool_txt = match (pool, same.is_empty()) {
                (None, _) => "pool not read".to_owned(),
                (Some(_), true) => "not in the sound pool".to_owned(),
                (Some(_), false) => format!("= pool {}", same.iter().map(|i| format!("{:03}", i + 1)).collect::<Vec<_>>().join(", ")),
            };
            let text = format!("“{}”  {}  ·  {pool_txt}", s.name, vals.join(" "));
            ui.label(RichText::new(text).color(OLED)).on_hover_text(format!("{tip}\n\n{}", src_detail(&s.src)));
        }
    }
}
