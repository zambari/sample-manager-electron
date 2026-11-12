// Release builds on Windows are GUI apps: no console window behind them.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod device;
mod editor;
mod fs_model;
mod jobs;
mod keymap;
mod links;
mod prints;
mod log;
mod ops;
mod pane;
mod preview;
mod rename;
mod sizes;
mod state;
mod store;
mod tab;
mod ui;

fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1560.0, 880.0])
            .with_min_inner_size([960.0, 520.0])
            .with_title("Sample Manager")
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
                    .expect("bundled icon is a valid PNG"),
            ),
        ..Default::default()
    };

    // macOS delivers MIDI setup changes (devices plugged in or out) on the
    // run loop of the thread that first creates a MIDI client. Create one
    // here on the main thread, whose run loop the UI keeps turning; created
    // first on the device thread, the port list would never update and a
    // swapped device would keep being looked for under its old port.
    let _midi = midir::MidiInput::new("Sample Manager");

    eframe::run_native(
        "Sample Manager",
        native_options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc.egui_ctx.clone(), cc.storage)))),
    )
}
