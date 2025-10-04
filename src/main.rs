// Egui interface for sendme.

// hide console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] 

mod app;
mod comms;
mod worker;
mod about;
mod notes;

use app::App;
use eframe::NativeOptions;

fn main() -> eframe::Result {
    tracing_subscriber::fmt::init();
    let mut options = NativeOptions::default();
    options.viewport = options
        .viewport
        .with_title("Liminal Docs")
        .with_resizable(true)
        .with_inner_size([640., 480.])
        .with_drag_and_drop(true); // So cool !!
    App::run(options)
}
