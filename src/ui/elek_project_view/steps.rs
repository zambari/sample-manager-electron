//! Pattern and track step visuals, timing summaries and hover descriptions.

use super::*;

/// One track's steps as dots, from `at` (left, vertical centre), groups of
/// 16 slightly apart. Steps past the track's length aren't drawn. Returns
/// the right edge.
/// Byte cells in a fixed grid; zero bytes dimmed when `raw` is given.
pub(super) fn hex_row(painter: &egui::Painter, at: egui::Pos2, cells: &[String], color: Color32, raw: Option<&[u8]>) {
    const PITCH: f32 = 20.0;
    for (k, c) in cells.iter().enumerate() {
        let x = at.x + k as f32 * PITCH + (k / 8) as f32 * 6.0;
        let col = if raw.is_some_and(|r| r.get(k) == Some(&0)) { DOT_BEAT } else { color };
        painter.text(egui::pos2(x, at.y), egui::Align2::LEFT_CENTER, c, oled_font(10.0), col);
    }
}

/// Step dot spacing: `(pitch, gap between 16-step groups)`.
fn dot_pitch(n: usize, max_w: f32) -> (f32, f32) {
    let gap = 5.0;
    let groups = n.div_ceil(16).saturating_sub(1) as f32;
    (((max_w - groups * gap) / n.max(1) as f32).clamp(3.0, 9.0), gap)
}

/// The step accepted by `want` nearest to `x` in a dot line drawn from
/// `x0`, within a reach wider than the dot itself so it's easy to hover.
/// Returns the step and its centre x.
pub(super) fn nearest_step(n: usize, x0: f32, max_w: f32, x: f32, want: impl Fn(usize) -> bool) -> Option<(usize, f32)> {
    let (pitch, gap) = dot_pitch(n, max_w);
    let reach = (pitch * 2.0).max(14.0);
    (0..n)
        .filter(|&k| want(k))
        .map(|k| (k, x0 + k as f32 * pitch + (k / 16) as f32 * gap + pitch / 2.0))
        .filter(|&(_, cx)| (x - cx).abs() <= reach)
        .min_by(|a, b| (x - a.1).abs().total_cmp(&(x - b.1).abs()))
}

/// What's on step `k`: note-ish bytes, flags, p-locks.
/// A track or pattern length: "2 bar" for whole bars past one, else steps.
pub(super) fn len_label(n: usize) -> String {
    if n > 16 && n % 16 == 0 { format!("{} bar", n / 16) } else { format!("{n} steps") }
}

/// The scales other than 1x in use (master and tracks), fastest first,
/// e.g. "2x 1/2x"; `None` when everything runs at 1x.
pub(super) fn pattern_scale(n: &patterns::PatternInfo) -> Option<String> {
    let used: BTreeSet<u8> = n.timing.map(|t| t.scale).into_iter().chain(n.seq.iter().filter_map(|t| t.scale)).filter(|&s| s != 2).collect();
    let labels: Vec<&str> = used.into_iter().filter_map(patterns::scale_label).collect();
    (!labels.is_empty()).then(|| labels.join(" "))
}

/// Hover text for the folded length / scale summary.
pub(super) fn timing_tip(label: &str, n: &patterns::PatternInfo, tr: &[&TrackSeq]) -> String {
    let mut out = vec![label.to_owned()];
    if let Some(t) = n.timing {
        let len = match t.master_len {
            1 => "INF".to_owned(),
            l => format!("{l} steps{}", if l % 16 == 0 && l > 16 { format!(" ({})", len_label(l as usize)) } else { String::new() }),
        };
        out.push(format!("scale mode: {}", if t.per_track { "per track" } else { "normal" }));
        out.push(format!("reset (master length): {len}"));
        out.push(format!("change: {}", if t.change <= 1 { "OFF".to_owned() } else { t.change.to_string() }));
        out.push(format!("master scale: {}", patterns::scale_label(t.scale).unwrap_or("?")));
        out.push(format!("tempo: {:.1} BPM", t.tempo_x120 as f32 / 120.0));
    } else {
        out.push("master length / change / scale: layout unknown for this version".to_owned());
    }
    // Per track, grouped when several share a length and scale.
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (k, t) in tr.iter().enumerate() {
        let len = if t.len_known { t.len.to_string() } else { format!("≈{}", t.len) };
        let sc = t.scale.and_then(patterns::scale_label).map_or(String::new(), |s| format!(" at {s}"));
        let key = format!("{len} steps{sc}");
        match groups.iter_mut().find(|g| g.0 == key) {
            Some(g) => g.1.push(k + 1),
            None => groups.push((key, vec![k + 1])),
        }
    }
    if groups.len() == 1 {
        out.push(format!("all tracks: {}", groups[0].0));
    } else {
        for (key, ts) in groups {
            let names: Vec<String> = ts.iter().map(|t| t.to_string()).collect();
            out.push(format!("track {}: {key}", names.join(", ")));
        }
    }
    out.join("\n")
}

pub(super) fn step_tip(seq: &TrackSeq, k: usize, track: &str, sample_name: &dyn Fn(usize) -> String) -> String {
    let mut s = format!("{track} step {} (bar {}, step {})", k + 1, k / 16 + 1, k % 16 + 1);
    s += &format!("\ntrig flags {:04x}", seq.flags.get(k).copied().unwrap_or(0));
    let locks: Vec<String> = seq
        .locks
        .iter()
        .filter_map(|(p, v)| v.get(k).copied().flatten().map(|v| (*p, v)))
        .map(|(p, v)| {
            if Some(p) == seq.sample_param {
                // DT1 (64 steps) keeps the slot in the high byte.
                let slot = if seq.flags.len() == 64 { v >> 8 } else { v } as usize;
                format!("  sample → {slot:03} {}", sample_name(slot))
            } else {
                format!("  param {p:#04x} ({p}) = {} (raw {v:04x})", v >> 8)
            }
        })
        .collect();
    if locks.is_empty() {
        s += "\nno p-locks";
    } else {
        s += "\np-locks:\n";
        s += &locks.join("\n");
    }
    // Raw per-step bytes, for finding the note: offset in track = byte.
    let raw: Vec<String> = seq.arrays.iter().map(|(o, a)| format!("@{o}:{:02x}", a.get(k).copied().unwrap_or(0))).collect();
    if !raw.is_empty() {
        s += "\nper-step bytes (track offset:value):\n  ";
        s += &raw.chunks(4).map(|c| c.join("  ")).collect::<Vec<_>>().join("\n  ");
    }
    s
}

pub(super) fn dots(painter: &egui::Painter, at: egui::Pos2, max_w: f32, seq: &TrackSeq) -> f32 {
    let n = seq.steps.len();
    let (pitch, gap) = dot_pitch(n, max_w);
    let groups = n.div_ceil(16).saturating_sub(1) as f32;
    let r = (pitch * 0.32).clamp(1.2, 2.8);
    let x_of = |k: usize| at.x + k as f32 * pitch + (k / 16) as f32 * gap + pitch / 2.0;
    for (k, s) in seq.steps.iter().enumerate().take(seq.len) {
        let c = egui::pos2(x_of(k), at.y);
        match s {
            Step::Note => {
                painter.circle_filled(c, r, OLED);
            }
            Step::Lock => {
                painter.circle_stroke(c, r - 0.3, Stroke::new(1.0, OLED_DIM));
            }
            Step::None => {
                painter.circle_filled(c, 1.0, if k % 4 == 0 { DOT_BEAT } else { DOT_OFF });
            }
        }
    }
    at.x + n as f32 * pitch + groups * gap
}

/// The audio tracks squeezed into a small picture: 2 px per step, one thin
/// line per track. More than 8 tracks (DT2) share lines two at a time, so
/// each line stays tall enough to read.
pub(super) fn mini(painter: &egui::Painter, at: egui::Pos2, h: f32, tracks: &[&TrackSeq]) {
    const BG: Color32 = Color32::from_rgb(92, 42, 6);
    const LEN: Color32 = Color32::from_rgb(122, 60, 16);
    const LOCK: Color32 = Color32::from_rgb(196, 128, 70);
    let Some(first) = tracks.first() else { return };
    let n = first.steps.len();
    let per_line = if tracks.len() > 8 { 2 } else { 1 };
    let lines = tracks.len().div_ceil(per_line).max(1);
    let line = h / lines as f32;
    let bg = Rect::from_min_size(at, egui::vec2(n as f32 * 2.0, h));
    painter.rect_filled(bg.expand(1.0), 1.0, BG);
    for (l, group) in tracks.chunks(per_line).enumerate() {
        let y = at.y + l as f32 * line;
        // The longest track's length, faintly.
        let len = group.iter().map(|t| t.len).max().unwrap_or(0);
        let lr = Rect::from_min_size(egui::pos2(at.x, y + line * 0.35), egui::vec2(len as f32 * 2.0, (line * 0.3).max(0.5)));
        painter.rect_filled(lr, 0.0, LEN);
        for k in 0..len {
            // A note on either track wins over a lock.
            let step = |t: &&TrackSeq| if k < t.len { t.steps.get(k).copied().unwrap_or(Step::None) } else { Step::None };
            let c = if group.iter().any(|t| step(t) == Step::Note) {
                OLED
            } else if group.iter().any(|t| step(t) == Step::Lock) {
                LOCK
            } else {
                continue;
            };
            painter.rect_filled(Rect::from_min_size(egui::pos2(at.x + k as f32 * 2.0, y + 0.3), egui::vec2(1.6, (line - 0.6).max(1.0))), 0.0, c);
        }
    }
}

/// A fold arrow centred on `c`: pointing down when open.
pub(super) fn triangle(painter: &egui::Painter, c: egui::Pos2, open: bool, color: Color32) {
    let pts = if open {
        vec![c + egui::vec2(-4.0, -2.5), c + egui::vec2(4.0, -2.5), c + egui::vec2(0.0, 3.5)]
    } else {
        vec![c + egui::vec2(-2.5, -4.0), c + egui::vec2(3.5, 0.0), c + egui::vec2(-2.5, 4.0)]
    };
    painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}
