//! The multi-rename panel: takes over a file pane while open, with Find /
//! Replace fields and a live `old → new` list of every selected item.

use crate::rename::{MultiRename, Row, Status};
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Key, RichText, Ui};

pub enum Outcome {
    Apply(Vec<Row>),
    Cancel,
}

const HIT_OLD: Color32 = Color32::from_rgb(230, 110, 100);
const HIT_NEW: Color32 = Color32::from_rgb(110, 220, 130);

pub fn show(ui: &mut Ui, mr: &mut MultiRename, pane_focused: bool) -> Option<Outcome> {
    let mut out = None;
    let rows = mr.preview();
    let n_change = rows.iter().filter(|r| r.status == Status::Ok).count();
    let n_bad = rows.iter().filter(|r| matches!(r.status, Status::Problem(_))).count();
    let can_apply = n_change > 0 && n_bad == 0;

    ui.add_space(4.0);
    let n = mr.items.len();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Multi-Rename Tool").strong());
        ui.label(RichText::new(format!("{n} item{}", if n == 1 { "" } else { "s" })).weak());
    });
    ui.add_space(4.0);

    let mut field_focused = false;
    egui::Grid::new("multi_rename_fields").num_columns(2).spacing([6.0, 4.0]).show(ui, |ui| {
        ui.label("Find");
        let find = ui.add(
            egui::TextEdit::singleline(&mut mr.find).desired_width(f32::INFINITY).hint_text("text to replace"),
        );
        if std::mem::take(&mut mr.focus_find) {
            find.request_focus();
        }
        field_focused |= find.has_focus() || find.lost_focus();
        ui.end_row();
        ui.label("Replace");
        let rep = ui.add(
            egui::TextEdit::singleline(&mut mr.replace).desired_width(f32::INFINITY).hint_text("replace with (empty removes)"),
        );
        field_focused |= rep.has_focus() || rep.lost_focus();
        ui.end_row();
    });
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut mr.match_case, "Match case");
        ui.checkbox(&mut mr.keep_ext, "Keep extensions")
            .on_hover_text("Only replace in the part of a file name before the extension");
    });

    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        if ui.button("Cancel").on_hover_text("Esc").clicked() {
            out = Some(Outcome::Cancel);
        }
        if ui.button("Clear").on_hover_text("Empty both fields").clicked() {
            mr.find.clear();
            mr.replace.clear();
            mr.focus_find = true;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let label = if n_change == 0 { "Apply".to_owned() } else { format!("Apply ({n_change})") };
            let tip = if n_bad > 0 { "Fix the problems marked in red first" } else { "↩" };
            if ui.add_enabled(can_apply, egui::Button::new(label)).on_hover_text(tip).on_disabled_hover_text(tip).clicked() {
                out = Some(Outcome::Apply(Vec::new()));
            }
        });
    });

    let summary = {
        let mut s = format!("{n_change} will change · {} unchanged", rows.len() - n_change - n_bad);
        if n_bad > 0 {
            s += &format!(" · {n_bad} problem{}", if n_bad == 1 { "" } else { "s" });
        }
        s
    };
    ui.label(RichText::new(summary).small().color(if n_bad > 0 { ui.visuals().error_fg_color } else { ui.visuals().weak_text_color() }));
    ui.separator();

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let font = FontId::monospace(12.0);
        let text = ui.visuals().text_color();
        let weak = ui.visuals().weak_text_color();
        let err = ui.visuals().error_fg_color;
        for r in &rows {
            match &r.status {
                Status::Unchanged => {
                    ui.label(job("", &r.old, &[], &font, weak, weak, false));
                }
                Status::Ok | Status::Problem(_) => {
                    let bad = matches!(r.status, Status::Problem(_));
                    ui.label(job("", &r.old, &r.old_hits, &font, weak, HIT_OLD, true));
                    let (color, hit) = if bad { (err, err) } else { (text, HIT_NEW) };
                    ui.label(job("→ ", &r.new, &r.new_hits, &font, color, hit, false));
                    if let Status::Problem(why) = &r.status {
                        ui.label(RichText::new(format!("  ⚠ {why}")).small().color(err));
                    }
                }
            }
            ui.add_space(3.0);
        }
    });

    // Enter applies, Esc cancels -- from the fields, or anywhere in this pane.
    if field_focused || pane_focused {
        // Consumed, so the app's own Enter (commit) doesn't also fire once
        // the panel has closed.
        let (enter, esc) = ui.input_mut(|i| {
            let m = egui::Modifiers::NONE;
            (i.consume_key(m, Key::Enter), i.consume_key(m, Key::Escape))
        });
        if esc {
            out = Some(Outcome::Cancel);
        } else if enter && can_apply {
            out = Some(Outcome::Apply(Vec::new()));
        }
    }

    match out {
        Some(Outcome::Apply(_)) => Some(Outcome::Apply(rows)),
        o => o,
    }
}

/// `prefix` then `text`, with `hits` drawn in `hit` (struck through with
/// `strike`) and the rest in `base`. Labels wrap, so long names stay
/// readable in a narrow pane.
fn job(prefix: &str, text: &str, hits: &[std::ops::Range<usize>], font: &FontId, base: Color32, hit: Color32, strike: bool) -> LayoutJob {
    let mut j = LayoutJob::default();
    let plain = TextFormat::simple(font.clone(), base);
    let mut marked = TextFormat::simple(font.clone(), hit);
    marked.background = hit.gamma_multiply(0.18);
    if strike {
        marked.strikethrough = egui::Stroke::new(1.0, hit);
    }
    if !prefix.is_empty() {
        j.append(prefix, 0.0, plain.clone());
    }
    let mut at = 0;
    for h in hits {
        if h.start > at {
            j.append(&text[at..h.start], 0.0, plain.clone());
        }
        j.append(&text[h.clone()], 0.0, marked.clone());
        at = h.end;
    }
    if at < text.len() || text.is_empty() {
        j.append(&text[at..], 0.0, plain);
    }
    j
}
