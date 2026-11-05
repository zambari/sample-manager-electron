//! The level/statistics table under the editor.

use super::*;

pub(super) fn stats_table(ui: &mut Ui, ed: &mut Editor) {
    let Some(s) = ed.sample().cloned() else { return };
    let gain = ed.gain();
    let r: Stats = ed.region_stats();
    let f: Stats = ed.file_stats();
    let dbv = |lin: f32| stats::fmt_db(stats::to_db(lin));
    let crest = |st: &Stats| {
        if st.rms > 0.0 { format!("{:.1} dB", stats::to_db(st.peak / st.rms)) } else { "—".to_owned() }
    };
    let len = |n: usize| format!("{:.3} s", s.seconds(n));

    egui::Grid::new("editor_stats").num_columns(3).spacing([18.0, 3.0]).striped(true).show(ui, |ui| {
        ui.label("");
        ui.label(RichText::new(if ed.mono_active() { "Region (mono, with gain)" } else { "Region (with gain)" }).weak());
        ui.label(RichText::new("File").weak());
        ui.end_row();

        ui.label("Length");
        ui.monospace(len(r.frames));
        ui.monospace(len(f.frames));
        ui.end_row();

        ui.label("Peak");
        let peak_after = r.peak * gain;
        let txt = RichText::new(dbv(peak_after)).monospace();
        ui.label(if peak_after >= 1.0 { txt.color(CLIP_COLOR) } else { txt });
        ui.monospace(dbv(f.peak));
        ui.end_row();

        ui.label("RMS");
        ui.monospace(dbv(r.rms * gain));
        ui.monospace(dbv(f.rms));
        ui.end_row();

        ui.label("Crest");
        ui.monospace(crest(&r));
        ui.monospace(crest(&f));
        ui.end_row();

        ui.label("DC offset");
        ui.monospace(format!("{:+.3}%", r.dc * gain * 100.0));
        ui.monospace(format!("{:+.3}%", f.dc * 100.0));
        ui.end_row();

        ui.label("Clipped");
        let txt = RichText::new(format!("{} samples", r.clipped)).monospace();
        ui.label(if r.clipped > 0 { txt.color(CLIP_COLOR) } else { txt });
        ui.label("");
        ui.end_row();
    });

    let ch = match s.channels.len() {
        1 => "mono".to_owned(),
        2 => "stereo".to_owned(),
        n => format!("{n} ch"),
    };
    let bits = s.bits.map(|b| format!(" · {b}-bit")).unwrap_or_default();
    ui.label(
        RichText::new(format!(
            "{} · {:.1} kHz{bits} · {ch} · {}",
            s.format,
            s.rate as f64 / 1000.0,
            crate::fs_model::human_bytes(s.file_len)
        ))
        .weak()
        .small(),
    );
}
