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
        v.window_shadow = Shadow {
            offset: [0, 8],
            blur: 18,
            spread: 0,
            color: Color32::from_black_alpha(110),
        };
        v.popup_shadow = Shadow {
            offset: [0, 4],
            blur: 10,
            spread: 0,
            color: Color32::from_black_alpha(100),
        };

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

        for wv in [
            &mut w.noninteractive,
            &mut w.inactive,
            &mut w.hovered,
            &mut w.active,
            &mut w.open,
        ] {
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
    mode_title(label, focused, focused)
}

/// A title that doubles as a mode switch (Sample Editor / Elk Sample Order):
/// the active mode is always underlined so you can tell which one you're in;
/// it lights up orange only when its column also has keyboard focus.
pub fn mode_title(label: &str, active: bool, focused: bool) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        let text = egui::RichText::new(label.to_uppercase())
            .size(12.0)
            .strong();
        let text = if focused {
            text.color(ACCENT_BRIGHT)
        } else if active {
            text.color(ui.visuals().strong_text_color())
        } else {
            text.color(ui.visuals().weak_text_color())
        };
        let resp = ui.add(egui::Button::new(text).frame(false));
        if active {
            let r = resp.rect;
            ui.painter().hline(
                r.x_range().shrink(4.0),
                r.bottom(),
                Stroke::new(2.0, ACCENT),
            );
        }
        resp
    }
}

/// The glyph every close button uses. Plain "×" (U+00D7) is in the bundled
/// fonts; "✕" and friends are not and render as a missing-glyph box.
pub const CLOSE_GLYPH: &str = "×";

/// A close button: "×", optionally followed by a label, with one extra pixel
/// of padding on each side so the narrow glyph doesn't look cramped.
pub fn close_button(label: Option<&str>, small: bool) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        let text = match label {
            Some(l) => format!("{CLOSE_GLYPH} {l}"),
            None => CLOSE_GLYPH.to_owned(),
        };
        ui.scope(|ui| {
            ui.spacing_mut().button_padding.x += 1.0;
            let mut b = egui::Button::new(text);
            if small {
                b = b.small();
            }
            ui.add(b)
        })
        .inner
    }
}

/// Corner radius of a pane tab's top corners (the bottom is flat).
const TAB_RADIUS: f32 = 4.0;

/// A pane sub-tab header. Inactive tabs are a soft grey block; the active
/// one has no fill and an orange frame on top and sides, open at the bottom
/// so it reads as joined to the file list below.
pub fn pane_tab(label: &str, active: bool) -> impl egui::Widget + '_ {
    move |ui: &mut egui::Ui| {
        let pad = egui::vec2(10.0, 3.0);
        let font = egui::TextStyle::Button.resolve(ui.style());
        let color = if active {
            ACCENT_BRIGHT
        } else {
            ui.visuals().text_color()
        };
        let galley = ui.painter().layout_no_wrap(label.to_owned(), font, color);
        let size = galley.size() + pad * 2.0;
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
        if !ui.is_rect_visible(rect) {
            return resp;
        }

        let p = ui.painter();
        let rad = TAB_RADIUS as u8;
        let corners = CornerRadius {
            nw: rad,
            ne: rad,
            sw: 0,
            se: 0,
        };
        let w = &ui.visuals().widgets;
        if active {
            if resp.hovered() {
                p.rect_filled(rect, corners, w.inactive.weak_bg_fill.gamma_multiply(0.5));
            }
            let stroke = Stroke::new(1.0, ACCENT_BRIGHT.gamma_multiply(0.8));
            p.add(egui::Shape::line(
                open_tab_outline(rect.shrink(0.5)),
                stroke,
            ));
        } else {
            let fill = if resp.hovered() {
                w.hovered.weak_bg_fill
            } else {
                w.inactive.weak_bg_fill
            };
            p.rect_filled(rect, corners, fill);
        }
        p.galley(rect.min + pad, galley, color);
        resp
    }
}

/// Left side, rounded top, right side — no bottom edge.
fn open_tab_outline(r: egui::Rect) -> Vec<egui::Pos2> {
    use std::f32::consts::{FRAC_PI_2, PI};
    let rad = TAB_RADIUS.min(r.width() / 2.0).min(r.height());
    let arc = |c: egui::Pos2, from: f32, to: f32, out: &mut Vec<egui::Pos2>| {
        const STEPS: usize = 6;
        for i in 0..=STEPS {
            let a = from + (to - from) * i as f32 / STEPS as f32;
            out.push(c + rad * egui::vec2(a.cos(), a.sin()));
        }
    };
    let mut pts = vec![r.left_bottom()];
    arc(
        egui::pos2(r.left() + rad, r.top() + rad),
        PI,
        PI + FRAC_PI_2,
        &mut pts,
    );
    arc(
        egui::pos2(r.right() - rad, r.top() + rad),
        -FRAC_PI_2,
        0.0,
        &mut pts,
    );
    pts.push(r.right_bottom());
    pts
}

/// Height of a `section_header` row.
pub const SECTION_H: f32 = 13.0;

/// Tiny caps label with a hairline running to the right edge. Sits above
/// each editor section (Show / Transport / Gain …) to separate them.
pub fn section_header(ui: &mut egui::Ui, label: &str) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, SECTION_H), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let p = ui.painter();
    let color = ui.visuals().weak_text_color().gamma_multiply(0.8);
    let galley = p.layout_no_wrap(label.to_uppercase(), egui::FontId::proportional(8.5), color);
    let y = rect.center().y;
    let text_pos = egui::pos2(rect.left(), y - galley.size().y / 2.0);
    let line_x = rect.left() + galley.size().x + 5.0;
    p.galley(text_pos, galley, color);
    if line_x < rect.right() {
        p.hline(
            line_x..=rect.right(),
            y + 0.5,
            Stroke::new(1.0, Color32::from_gray(44)),
        );
    }
}

/// Show / hide switch for a panel: a flat, square-cornered pixel-font label
/// whose background is the indicator. On: low-alpha accent fill, normal text.
/// Off: no fill, dim text. Quieter than the solid orange chips used for
/// Loop / Snap etc., so the Show bar reads as its own thing.
pub fn vis_toggle<'a>(on: &'a mut bool, label: &'a str) -> impl egui::Widget + 'a {
    move |ui: &mut egui::Ui| {
        let font = oled_font(12.0);
        let pad = egui::vec2(6.0, 1.0);
        let text_color = if *on {
            ui.visuals().text_color()
        } else {
            ui.visuals().weak_text_color().gamma_multiply(0.7)
        };
        let galley = ui
            .painter()
            .layout_no_wrap(label.to_owned(), font, text_color);
        let size = galley.size() + pad * 2.0;
        let (rect, mut resp) = ui.allocate_exact_size(size, egui::Sense::click());
        if resp.clicked() {
            *on = !*on;
            resp.mark_changed();
        }
        if !ui.is_rect_visible(rect) {
            return resp;
        }
        let p = ui.painter();
        let hovered = resp.hovered();
        let fill = match (*on, hovered) {
            (true, false) => Some(ACCENT.gamma_multiply(0.22)),
            (true, true) => Some(ACCENT.gamma_multiply(0.32)),
            (false, true) => Some(Color32::from_gray(32)),
            (false, false) => None,
        };
        if let Some(f) = fill {
            p.rect_filled(rect, 0.0, f);
        }
        p.galley(rect.min + pad, galley, text_color);
        resp
    }
}

/// Font family for device-screen text (Dogica Pixel font).
pub const OLED_FAMILY: &str = "oled";

/// Dogica draws about 2pt larger than Silkscreen did at the same size, and
/// layouts were tuned for Silkscreen, so every pixel-font size is scaled
/// down by this factor.
const OLED_SIZE_SCALE: f32 = 0.6;

/// Device-screen text: the Dogica Pixel font.
pub fn oled_font(size: f32) -> egui::FontId {
    egui::FontId::new(
        size * OLED_SIZE_SCALE,
        egui::FontFamily::Name(OLED_FAMILY.into()),
    )
}

/// Readout green-white, like the device screen. Used for anything that lives
/// on the device (Elk Sample Order slots, +Drive file lists).
pub const OLED: Color32 = Color32::from_rgb(222, 236, 230);
pub const OLED_DIM: Color32 = Color32::from_rgb(110, 122, 118);
pub const OLED_BG: Color32 = Color32::from_rgb(12, 14, 14);
pub const OLED_HOVER: Color32 = Color32::from_rgb(30, 36, 36);

/// Registers the pixel font as `OLED_FAMILY`, falling back to the default
/// fonts for glyphs it lacks (accents, symbols).
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "dogica_pixel".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../../assets/dogicapixel.ttf"
        ))),
    );
    let mut fam = vec!["dogica_pixel".to_owned()];
    fam.extend(
        fonts
            .families
            .get(&egui::FontFamily::Proportional)
            .cloned()
            .unwrap_or_default(),
    );
    fonts
        .families
        .insert(egui::FontFamily::Name(OLED_FAMILY.into()), fam);
    ctx.set_fonts(fonts);
}
