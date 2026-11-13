//! Machines: what the 8 SRC page places of a sound mean, per machine.
//!
//! Every sound struct (kit track sound or pool sound, same layout) keeps its
//! SRC page in 8 fixed places, 2 bytes each (value, fine): see
//! `library::src_offsets`. The track's **machine byte** says what each place
//! holds. Everything here was found by setting one machine per track on the
//! device, dumping and diffing (Save debug dump). Names ending in `?` and
//! the `[unverified]` notes are guesses still to be checked on the device.
//!
//! # Digitakt (DT1), sound v3 — machine byte at sound+124
//! Dump 2026-11-13_091146_Digitakt, pattern B03 (+ pool sounds 0–7, the
//! same 8 sounds). SRC places at sound+62, 64 … 76. The byte after the
//! machine byte read 01 on every sound.
//!
//! | id | machine  | +62  | +64  | +66  | +68  | +70  | +72  | +74  | +76 |
//! |----|----------|------|------|------|------|------|------|------|-----|
//! | 0  | ONE SHOT | TUNE | PLAY | BIT  | SMP  | STRT | LEN  | LOOP | LEV |
//! | 1  | WERP     | TUNE?| –    | BIT? | SMP  | SEG  | PLAY | BARS | LEV |
//! | 2  | REPITCH  | –    | –    | BIT? | SMP  | ?    | ?    | BARS | LEV |
//! | 3  | SLICE    | TUNE?| –    | BIT? | SMP  | SLICE| SLEN | GRID | LEV |
//! | 6  | POLY     | custom machine, defaults 64 0 16 0 0 120 0 100 |
//! | 9  | DT-FM    | custom machine, defaults 64 0 16 0 127 0 127 64 |
//! | 28 | CHORD    | custom machine, defaults 64 32 100 0 0 120 0 100 |
//!
//! - SEG: 0 = 1/32, 1 = 1/16, 2 = 1/8 (seen 1 and 2).
//! - PLAY: 2 = FWD LOOP, 3 = REV LOOP seen; [unverified] 0 FWD, 1 REV.
//!   Machines that don't use +64 keep the one-shot value the sound had.
//! - BARS: index into 1, 2, 4, 8 … (seen 1 → 0, 2 → 1, 4 → 2).
//! - SLICE: 0 = slices mapped to pitch, n = slice n (seen 6). Parameter 5
//!   on the device (confirmed).
//! - SLEN: stored 2 for a slice length of 3: the device counts from 0
//!   (confirmed), so stored = shown − 1 here (DT II SLICE: see below).
//! - GRID: 0 = 4, 1 = 8, 2 = 16, 3 = 32, 4 = 64 (seen 4).
//! - DT1's SLICE (id 3) has the grid parameter, so it matches the DT II's
//!   GRID machine, not the DT II's SLICE.
//! - Custom machine ids (6, 9, 28): [unverified] whether they hold across
//!   firmware builds. Ids 4, 5, 7, 8, 10–27 not seen.
//! - **+68 is a pool sample slot only for ids 0–3.** On the synth machines
//!   it's a parameter (0 here). Sample slot renumbering must check this.
//! - Old A-bank sounds (made before choosing machines) mostly read id 3,
//!   some 1 / 2.
//! - [open] B03 T1 was reported as TUNE +24, STRT 14.04, LEN 65.85,
//!   LOOP 13.31, LEV 80.86, but the dump holds the defaults with only
//!   PLAY = REV LOOP.
//!
//! # Digitakt II, sound v3 — machine byte at sound+210
//! Dump 2026-11-13_092130_DigitaktII, pattern B01. SRC places at sound+94,
//! 96, 98, 100 (u16 sample slot), 106, 108, 110, 112. Factory machines on
//! the device's list: ONE SHOT, WERP, STRETCH, REPITCH, SLICE, GRID (+ MIDI).
//!
//! | id | machine  | +94  | +96  | +98  | +100 | +106 | +108 | +110 | +112 |
//! |----|----------|------|------|------|------|------|------|------|------|
//! | 0  | ONE SHOT | TUNE | PLAY | BIT  | SMP  | STRT | LEN  | LOOP | LEV  |
//! | 2  | ?        | –    | –    | BIT? | SMP  | ?    | ?    | BARS | LEV  |
//! | 3  | ?        | –    | –    | BIT? | SMP  | ?    | ?    | BARS | LEV  |
//! | 4  | GRID     | TUNE?| –    | –    | SMP  | SLICE| SLEN | GRID | LEV  |
//! | 5  | MIDI     | parameters not in the sound: see below |
//! | 6  | SLICE    | TUNE?| –    | –    | SMP  | SLICE| SLEN?| –    | LEV  |
//! | 11 | PLAITS   | TUNE | MODE | HARM | –    | TIMB | MORPH| MIX  | LEV? |
//!
//! - Machine ids aren't in menu order, nor shared between models: new
//!   machines were slotted into the menu before old ones, so ids keep the
//!   order machines were added in (expected for compatibility).
//! - Ids 2 and 3: both were reported as REPITCH (T1 bars 2 → stored 1,
//!   T2 bars 8 → stored 3). [open] One of them is likely STRETCH.
//! - Id 1 (WERP) not dumped yet; shown with the DT1 werp layout.
//! - SLICE: 0 = keyboard (pitch-mapped), n = slice n (GRID T4: 3 → 3).
//! - SLEN: GRID stored 1 for length 2; SLICE stored 0 for length 0. The
//!   device counts slice length from 0 (confirmed), shown as stored on
//!   SLICE; GRID's 2 → 1 is still [open].
//! - GRID: 0 = 4, 1 = 8, 2 = 16 (seen 2).
//! - PLAITS (custom, id 11) as set: HARM 73, TIMB 65, MORPH 66, MIX 10.
//! - **MIDI (id 5)** keeps its parameters in the kit's per-track MIDI
//!   struct (268 bytes, the track name at kit+0x4638 + track × 0x10C, see
//!   `library::midi_names`), offsets from that name: +82 CHAN (0-based,
//!   seen 4 = channel 5), +84 BANK?, +86 SBNK?, +88 ? (02), +90 u16 BE
//!   PITCH BEND (0x2000 = centre, display = (v − 8192) / 64: 0x1BD7 →
//!   −16.64), +92 ? (03), +94 MOD WHEEL (10), +96 BREATH (18). Also the
//!   kit MIDI mask (`kit_midi_mask`, kit+0x56F4 u16) gets the track's bit
//!   (0x0010 = track 5). BANK, SBNK and PROG showed [x] (unset) on the
//!   device; +84 and +86 read 0. [open] what +88 = 2 and +92 = 3 are.

use super::*;

/// How a stored SRC value is shown.
#[derive(Clone, Copy)]
enum Fmt {
    Raw,
    /// Centred on 64, signed.
    Tune,
    /// Option index.
    List(&'static [&'static str]),
    /// 1, 2, 4, 8 …
    Pow2,
    /// Stored 0-based, shown 1-based.
    Plus1,
    /// 0 = keyboard / pitch-mapped, else the slice number.
    Slice,
}

const PLAY: Fmt = Fmt::List(&["FWD?", "REV?", "FWD LP", "REV LP"]);
const SEG: Fmt = Fmt::List(&["1/32", "1/16", "1/8", "1/4?", "1/2?"]);
const GRID: Fmt = Fmt::List(&["4", "8", "16", "32", "64"]);

type P = Option<(&'static str, Fmt)>;
const fn p(n: &'static str, f: Fmt) -> P {
    Some((n, f))
}
const RAW: Fmt = Fmt::Raw;

/// The machine-independent one-shot page.
const ONE_SHOT: [P; 8] = [p("TUNE", Fmt::Tune), p("PLAY", PLAY), p("BIT", RAW), p("SMP", RAW), p("STRT", RAW), p("LEN", RAW), p("LOOP", RAW), p("LEV", RAW)];
const WERP: [P; 8] = [p("TUNE?", Fmt::Tune), None, p("BIT?", RAW), p("SMP", RAW), p("SEG", SEG), p("PLAY", PLAY), p("BARS", Fmt::Pow2), p("LEV", RAW)];
const BARS_ONLY: [P; 8] = [None, None, p("BIT?", RAW), p("SMP", RAW), p("P5?", RAW), p("P6?", RAW), p("BARS", Fmt::Pow2), p("LEV", RAW)];
const GRID_PAGE: [P; 8] = [p("TUNE?", Fmt::Tune), None, None, p("SMP", RAW), p("SLICE", Fmt::Slice), p("SLEN", Fmt::Plus1), p("GRID", GRID), p("LEV", RAW)];
const DT1_SLICE: [P; 8] = [p("TUNE?", Fmt::Tune), None, p("BIT?", RAW), p("SMP", RAW), p("SLICE", Fmt::Slice), p("SLEN", Fmt::Plus1), p("GRID", GRID), p("LEV", RAW)];
const DT2_SLICE: [P; 8] = [p("TUNE?", Fmt::Tune), None, None, p("SMP", RAW), p("SLICE", Fmt::Slice), p("SLEN", RAW), None, p("LEV", RAW)];
const PLAITS: [P; 8] = [p("TUNE", Fmt::Tune), p("MODE", RAW), p("HARM", RAW), None, p("TIMB", RAW), p("MORPH", RAW), p("MIX", RAW), p("LEV?", RAW)];
const UNKNOWN: [P; 8] = [p("P1?", RAW), p("P2?", RAW), p("P3?", RAW), p("P4?", RAW), p("P5?", RAW), p("P6?", RAW), p("P7?", RAW), p("P8?", RAW)];
const NONE: [P; 8] = [None; 8];

/// One machine as far as it's known.
struct Machine {
    name: &'static str,
    /// Plays a pool sample (the SMP place is a sample slot).
    sample: bool,
    page: &'static [P; 8],
}

const fn mach(name: &'static str, sample: bool, page: &'static [P; 8]) -> Machine {
    Machine { name, sample, page }
}

fn machine(m: Model, id: u8) -> Machine {
    match (m, id) {
        (_, 0) => mach("ONE SHOT", true, &ONE_SHOT),
        (DT, 1) | (DT2, 1) => mach("WERP", true, &WERP),
        (DT, 2) => mach("REPITCH", true, &BARS_ONLY),
        (DT, 3) => mach("SLICE", true, &DT1_SLICE),
        (DT, 6) => mach("POLY", false, &UNKNOWN),
        (DT, 9) => mach("DT-FM", false, &UNKNOWN),
        (DT, 28) => mach("CHORD", false, &UNKNOWN),
        (DT2, 2) => mach("REPITCH/STRETCH? (2)", true, &BARS_ONLY),
        (DT2, 3) => mach("REPITCH/STRETCH? (3)", true, &BARS_ONLY),
        (DT2, 4) => mach("GRID", true, &GRID_PAGE),
        (DT2, 5) => mach("MIDI", false, &NONE),
        (DT2, 6) => mach("SLICE", true, &DT2_SLICE),
        (DT2, 11) => mach("PLAITS", false, &PLAITS),
        _ => mach("", false, &UNKNOWN),
    }
}

/// Where the machine byte sits in a sound struct of version `sv`.
fn machine_at(m: Model, sv: u32) -> Option<usize> {
    Some(match (m, sv) {
        (DT, 3) => 124,
        (DT2, 3) => 210,
        _ => return None,
    })
}

/// The machine byte of a sound struct, where the layout is known.
pub(super) fn machine_id(m: Model, sv: u32, d: &[u8]) -> Option<u8> {
    d.get(machine_at(m, sv)?).copied()
}

/// The machine's name ("#7?" for ids not seen yet).
pub fn machine_name(m: Model, id: u8) -> String {
    let n = machine(m, id).name;
    if n.is_empty() { format!("#{id}?") } else { n.to_owned() }
}

/// Does this machine play a pool sample? Unknown ids count as not.
pub fn plays_sample(m: Model, id: u8) -> bool {
    machine(m, id).sample
}

/// Should the sample-slot place of this sound struct (`sound` starts at
/// its `beefbace`) be renumbered when the sample pool moves? Yes unless its
/// machine is one we know doesn't play a sample (there the place is a synth
/// parameter). Unknown machine ids keep the old behaviour: renumbered.
pub(super) fn slot_is_sample(m: Model, sv: u32, sound: &[u8]) -> bool {
    match machine_id(m, sv, sound) {
        Some(id) => {
            let mc = machine(m, id);
            mc.sample || mc.name.is_empty()
        }
        None => true,
    }
}

/// One SRC parameter as the machine shows it.
#[derive(Clone, Debug)]
pub struct SrcParam {
    /// Place on the page, 0..8 (3 = the sample place).
    pub place: usize,
    pub name: &'static str,
    pub text: String,
    pub raw: (u8, u8),
}

/// The SRC places the machine uses, named and formatted. `src` is
/// `PoolSound::src` (value, fine per place).
pub fn src_params(m: Model, id: u8, src: &[(u8, u8)]) -> Vec<SrcParam> {
    let page = machine(m, id).page;
    page.iter()
        .enumerate()
        .filter_map(|(place, p)| {
            let (name, f) = (*p)?;
            let raw = *src.get(place)?;
            let v = raw.0;
            let text = match f {
                Fmt::Raw => v.to_string(),
                Fmt::Tune => {
                    let t = v as i32 - 64;
                    if t > 0 { format!("+{t}") } else { t.to_string() }
                }
                Fmt::List(l) => l.get(v as usize).map(|s| s.to_string()).unwrap_or_else(|| format!("#{v}?")),
                Fmt::Pow2 => if v < 8 { (1u32 << v).to_string() } else { format!("#{v}?") },
                Fmt::Plus1 => (v as u32 + 1).to_string(),
                Fmt::Slice => if v == 0 { "KEYS".into() } else { v.to_string() },
            };
            Some(SrcParam { place, name, text, raw })
        })
        .collect()
}
