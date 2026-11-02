//! The app's look: a neutral dark grey base with a dark orange accent and
//! nearly square corners.

use egui::{Color32, CornerRadius, Shadow, Stroke};

/// Fill for selected / toggled-on things (chips, rows, tabs, sliders).
pub const ACCENT: Color32 = Color32::from_rgb(184, 88, 18);
/// Brighter orange for text, outlines and markers on dark backgrounds.
pub const ACCENT_BRIGHT: Color32 = Color32::from_rgb(236, 132, 44);
/// Text drawn on top of an `ACCENT` fill.
pub const ON_ACCENT: Color32 = Color32::from_rgb(255, 238, 222);
/// Main waveform colour inside the in/out region.
pub const WAVE: Color32 = Color32::from_rgb(178, 200, 214);

/// Faint frame around a pane showing the Digitakt's +Drive.
pub fn device_frame() -> Stroke {
    Stroke::new(1.5, ACCENT_BRIGHT.gamma_multiply(0.45))
}

/// Barely-there wash behind a +Drive pane.
pub fn device_wash() -> Color32 {
    ACCENT.gamma_multiply(0.05)
}

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        let r = CornerRadius::same(2);
        let v = &mut style.visuals;

        v.panel_fill = Color32::from_gray(24);
        v.window_fill = Color32::from_gray(30);
        v.extreme_bg_color = Color32::from_gray(13);
        v.faint_bg_color = Color32::from_gray(32);
        v.code_bg_color = Color32::from_gray(44);
        v.window_stroke = Stroke::new(1.0, Color32::from_gray(56));
        v.window_corner_radius = CornerRadius::same(3);
        v.menu_corner_radius = CornerRadius::same(3);
        v.window_shadow = Shadow { offset: [0, 8], blur: 18, spread: 0, color: Color32::from_black_alpha(110) };
        v.popup_shadow = Shadow { offset: [0, 4], blur: 10, spread: 0, color: Color32::from_black_alpha(100) };

        v.selection.bg_fill = ACCENT;
        v.selection.stroke = Stroke::new(1.0, ON_ACCENT);
        v.hyperlink_color = ACCENT_BRIGHT;
        v.warn_fg_color = Color32::from_rgb(240, 180, 60);
        v.text_cursor.stroke = Stroke::new(2.0, ACCENT_BRIGHT);
        v.slider_trailing_fill = true;

        let w = &mut v.widgets;
        w.noninteractive.bg_fill = Color32::from_gray(24);
        w.noninteractive.weak_bg_fill = Color32::from_gray(24);
        w.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_gray(48));

        w.inactive.bg_fill = Color32::from_gray(46);
        w.inactive.weak_bg_fill = Color32::from_gray(38);
        w.inactive.bg_stroke = Stroke::NONE;

        w.hovered.bg_fill = Color32::from_gray(58);
        w.hovered.weak_bg_fill = Color32::from_gray(52);
        w.hovered.bg_stroke = Stroke::new(1.0, ACCENT_BRIGHT.gamma_multiply(0.55));

        w.active.bg_fill = Color32::from_gray(66);
        w.active.weak_bg_fill = Color32::from_gray(62);
        w.active.bg_stroke = Stroke::new(1.0, ACCENT_BRIGHT);

        w.open.bg_fill = Color32::from_gray(46);
        w.open.weak_bg_fill = Color32::from_gray(46);
        w.open.bg_stroke = Stroke::new(1.0, Color32::from_gray(70));

        for wv in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
            wv.corner_radius = r;
            wv.expansion = 0.0;
        }

        style.spacing.button_padding = egui::vec2(6.0, 2.0);
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    });
}

/// A column's title (Source / Sample Editor / Destination): plain text that
/// lights up orange with an underline when that column has keyboard focus.
pub fn pane_title(label: &str, focused: bool) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        let text = egui::RichText::new(label.to_uppercase()).size(12.0).strong();
        let text = if focused { text.color(ACCENT_BRIGHT) } else { text.color(ui.visuals().weak_text_color()) };
        let resp = ui.add(egui::Button::new(text).frame(false));
        if focused {
            let r = resp.rect;
            ui.painter().hline(r.x_range().shrink(4.0), r.bottom(), Stroke::new(2.0, ACCENT));
        }
        resp
    }
}
