//! The Sample Manager title button and About popup with build details and links.

pub fn show(ui: &mut egui::Ui) {
    let about = ui
        .add(
            egui::Button::new(egui::RichText::new("SAMPLE MANAGER").size(13.0).strong())
                .frame(false),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("About Sample Manager");
    egui::Popup::menu(&about)
        .width(480.0)
        .frame(egui::Frame::popup(ui.style()).inner_margin(16))
        .show(|ui| {
            // Enforce the content width: Popup::width only supplies an
            // initial size, and the popup can otherwise shrink to fit.
            ui.set_width(448.0);
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(egui::RichText::new("Sample Manager").strong());
            ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
            ui.label(format!("Build {}", env!("SAMPLE_MANAGER_BUILD")));
            ui.label(format!("Built {}", env!("SAMPLE_MANAGER_BUILD_DATE")));
            ui.separator();
            ui.hyperlink(env!("CARGO_PKG_REPOSITORY"));
            ui.hyperlink(env!("CARGO_PKG_HOMEPAGE"));
        });
}
