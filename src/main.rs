mod app;
mod audio;
mod device;
mod editor;
mod fs_model;
mod jobs;
mod keymap;
mod log;
mod ops;
mod pane;
mod preview;
mod sizes;
mod state;
mod tab;
mod ui;

fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1560.0, 880.0])
            .with_min_inner_size([960.0, 520.0])
            .with_title("Sample Manager"),
        ..Default::default()
    };

    eframe::run_native(
        "Sample Manager",
        native_options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc.egui_ctx.clone(), cc.storage)))),
    )
}
