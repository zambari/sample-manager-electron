//! Local and device file-cell painting, fonts and clipped text.

use super::*;

/// Pixel font size for device rows.
pub(super) const OLED_TEXT: f32 = 16.0;

/// Selected device row while the other pane has focus.
pub(super) const OLED_UNFOCUSED_SEL: egui::Color32 = egui::Color32::from_rgb(96, 106, 102);

/// Draws a row's icon, name and size straight onto the painter. These are
/// deliberately not widgets: a label would sit on top of the row's
/// interaction area and swallow clicks meant for the row.
pub(super) fn draw_cells(
    ui: &Ui,
    inner: egui::Rect,
    cx: &TableCtx<'_>,
    entry: &fs_model::Entry,
    shown: Option<&str>,
    device: bool,
) {
    let icon = if entry.is_dir {
        "📁"
    } else if !device && cx.preview.is_playing(&entry.path) {
        "🔊"
    } else if entry.is_audio || device {
        "🎵"
    } else {
        "·"
    };
    let gap = ui.spacing().item_spacing.x;

    // Right-aligned size first, so the name knows how much room it has.
    let size_text = if entry.is_dir && device {
        String::new()
    } else if entry.is_dir {
        match cx.sizes.get(&entry.path) {
            Some(Size::Known { bytes, files }) => {
                format!("{} · {files} files", fs_model::human_bytes(bytes))
            }
            Some(Size::Pending) => "calculating…".to_owned(),
            // Dash rather than 0: we genuinely don't know yet, and
            // claiming 0 would be a lie.
            None => "—".to_owned(),
        }
    } else {
        fs_model::human_bytes(entry.len)
    };
    let mono = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Monospace)
        .cloned()
        .unwrap_or_else(|| egui::FontId::monospace(12.0));
    let size_galley = ui
        .painter()
        .layout_no_wrap(size_text, mono, ui.visuals().weak_text_color());
    let size_pos = egui::pos2(
        inner.right() - size_galley.size().x,
        inner.center().y - size_galley.size().y / 2.0,
    );
    let size_left = size_pos.x;
    ui.painter()
        .galley(size_pos, size_galley, egui::Color32::PLACEHOLDER);

    let icon_color = if entry.empty {
        ui.visuals().text_color().gamma_multiply(0.5)
    } else {
        ui.visuals().text_color()
    };
    let mut x = paint_text(ui, inner, icon, body_font(ui, 0.0), icon_color);

    // Committed badge: a green check painted right after the icon. Drawn as
    // lines rather than a glyph so it never depends on font coverage.
    if !device && cx.committed.contains(&entry.path) {
        let h = 9.0;
        let left = x + gap;
        let cy = inner.center().y;
        let stroke = Stroke::new(2.0, egui::Color32::from_rgb(90, 210, 110));
        ui.painter().line_segment(
            [
                egui::pos2(left, cy),
                egui::pos2(left + h * 0.35, cy + h * 0.4),
            ],
            stroke,
        );
        ui.painter().line_segment(
            [
                egui::pos2(left + h * 0.35, cy + h * 0.4),
                egui::pos2(left + h, cy - h * 0.5),
            ],
            stroke,
        );
        x = left + h;
    }
    // Device file we have a local copy of: loads instantly, no fetch.
    if device && !entry.is_dir && cx.links.known(entry.hash, entry.len as u32).is_some() {
        let r = 3.0;
        let c = egui::pos2(x + gap + r, inner.center().y);
        ui.painter().circle_filled(c, r, crate::ui::theme::ACCENT_BRIGHT);
        x = c.x + r;
    }

    // Name: white (gray once opened in the editor), one point larger than
    // body text, cut off with "…" if it would run into the size column.
    let mut name_rect = inner;
    name_rect.min.x = x + gap;
    name_rect.max.x = (size_left - gap).max(name_rect.min.x);
    let name_color = if entry.empty {
        // Empty folder: nothing to see inside, so it recedes a little.
        egui::Color32::from_gray(185)
    } else if !device && cx.changed.contains(&entry.path) {
        crate::ui::theme::CHANGED
    } else if !device && cx.opened.contains(&entry.path) {
        egui::Color32::GRAY
    } else {
        egui::Color32::WHITE
    };
    // Cut items fade until they're pasted, like Explorer.
    let name_color = if cx.is_cut(&entry.path, device) {
        name_color.gamma_multiply(0.4)
    } else {
        name_color
    };
    paint_text(
        ui,
        name_rect,
        shown.unwrap_or(&entry.name),
        body_font(ui, 1.0),
        name_color,
    );
}

/// A device row, in the device-screen style of the Elek Sample Order view:
/// pixel font, green-white on black, inverted when selected. Folders get a
/// small painted folder (filled, or hollow when empty), since the pixel font
/// has no symbol glyphs.
pub(super) fn draw_device_cells(
    ui: &Ui,
    inner: egui::Rect,
    cx: &TableCtx<'_>,
    entry: &fs_model::Entry,
    shown: Option<&str>,
    selected: bool,
) {
    let (fg, dim) = if selected {
        (OLED_BG, egui::Color32::from_rgb(60, 70, 66))
    } else {
        (OLED, OLED_DIM)
    };
    let font = oled_font(OLED_TEXT);
    let cy = inner.center().y.round();

    let size_text = if entry.is_dir {
        String::new()
    } else {
        fs_model::human_bytes(entry.len)
    };
    let size_galley = ui.painter().layout_no_wrap(size_text, font.clone(), dim);
    let size_left = inner.right() - size_galley.size().x;
    ui.painter().galley(
        egui::pos2(size_left, cy - size_galley.size().y / 2.0),
        size_galley,
        dim,
    );

    // Folder icon, 13×9, pixel-aligned.
    const ICON_W: f32 = 13.0;
    let x = inner.left().round();
    if entry.is_dir {
        let color = if entry.empty { dim } else { fg };
        let tab = egui::Rect::from_min_size(egui::pos2(x, cy - 5.0), egui::vec2(6.0, 2.0));
        let body = egui::Rect::from_min_size(egui::pos2(x, cy - 3.0), egui::vec2(ICON_W, 8.0));
        let p = ui.painter();
        p.rect_filled(tab, 0.0, color);
        if entry.empty {
            p.rect_stroke(body, 0.0, Stroke::new(1.0, color), StrokeKind::Inside);
        } else {
            p.rect_filled(body, 0.0, color);
        }
    }

    let mut name_rect = inner;
    name_rect.min.x = x + ICON_W + 7.0;
    name_rect.max.x = (size_left - 8.0).max(name_rect.min.x);
    let color = if entry.is_dir && entry.empty { dim } else { fg };
    // Cut items fade until they're pasted.
    let color = if cx.is_cut(&entry.path, true) {
        color.gamma_multiply(0.4)
    } else {
        color
    };
    paint_text(ui, name_rect, shown.unwrap_or(&entry.name), font, color);
}

/// The body font, `extra` points larger.
pub(super) fn body_font(ui: &Ui, extra: f32) -> egui::FontId {
    let mut f = egui::TextStyle::Body.resolve(ui.style());
    f.size += extra;
    f
}

/// Paints one line of text left-aligned and vertically centred in `rect`,
/// eliding with "…" if it doesn't fit. Returns the x where the text ends.
pub(super) fn paint_text(
    ui: &Ui,
    rect: egui::Rect,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
) -> f32 {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width().max(0.0));
    let galley = ui.painter().layout_job(job);
    let pos = egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0);
    let end = pos.x + galley.size().x;
    ui.painter()
        .with_clip_rect(rect.intersect(ui.clip_rect()))
        .galley(pos, galley, color);
    end
}
