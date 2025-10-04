// The application egui front end

use core::f32;
use std::collections::BTreeMap;
use std::fmt::Display;
use std::path::PathBuf;

use crate::about::ABOUT;
use crate::comms::{Command, Config, Event, MessageDisplay, MessageType, ProgressList};
use crate::notes::Note;
use crate::worker::{Worker, WorkerHandle};
use crate::markdown;

use anyhow::Result;
use directories::{BaseDirs, UserDirs};
use eframe::NativeOptions;
use eframe::egui::{self, FontId, RichText, Visuals};
use egui::Ui;
use iroh::SecretKey;
use rfd::FileDialog;


use tracing::{info, warn};

const APP_NAME: &str = "liminal-docs";

impl Default for Config {
    fn default() -> Self {
        let download_path = match UserDirs::new() {
            Some(user_dirs) => user_dirs.download_dir().unwrap().to_owned().join(APP_NAME),
            None => std::process::exit(1),
        };
        let store_path = match BaseDirs::new() {
            Some(base_dirs) => base_dirs.data_dir().to_owned().join(APP_NAME),
            None => std::process::exit(1),
        };
        let secret_key = SecretKey::generate(rand::rngs::OsRng);
        let secret_key = data_encoding::HEXLOWER.encode(&secret_key.to_bytes());
        Self {
            dark_mode: true,
            download_path,
            store_path,
            secret_key,
            doc_key: None,
            author: None,
        }
    }
}

// Message list max
const MESSAGE_MAX: usize = 50;

// The application
pub struct App {
    is_first_update: bool,
    state: AppState,
}

// The application mode
#[derive(PartialEq)]
enum AppMode {
    Init,
    Idle,
    Edit,
    GetDocTicket,
    ShareTicket,
    Finished,
    Config,
    About,
}

impl Display for AppMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let val = match self {
            AppMode::Init => "Init",
            AppMode::Idle => "Idle",
            AppMode::Edit => "Editint ...",
            AppMode::Finished => "Finished",
            AppMode::Config => "Config",
            AppMode::About => "About...",
            AppMode::GetDocTicket => "Get Doc Ticket...",
            AppMode::ShareTicket => "Share Ticket...",
        };
        write!(f, "{}", val)
    }
}

// Internal state for the application
struct AppState {
    notes: NotesUi,
    worker: WorkerHandle,
    mode: AppMode,
    receiver_ticket: String,
    current_text: String,
    progress: ProgressList,
    messages: Vec<MessageDisplay>,
    config: Config,
    elapsed: Option<u64>,
    share_ticket: Option<String>,
}

// Make the egui impl for display
impl eframe::App for App {
    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        if self.is_first_update {
            self.is_first_update = false;
            ctx.set_zoom_factor(1.);
            if self.state.config.dark_mode {
                ctx.set_visuals(Visuals::dark());
            } else {
                ctx.set_visuals(Visuals::light());
            };
            // Push the redraw function into the worker.
            // This is janky and has a mutex for borrowing reasons
            let ctx = ctx.clone();
            let callback = Box::new(move || ctx.request_repaint());
            self.state.cmd(Command::Setup { callback });
        }
        self.state.update(ctx);
    }
}

// The application runner start,draw, etc...
// Spawns the worker as a subthread
impl App {
    pub fn run(options: NativeOptions) -> Result<(), eframe::Error> {
        // Load the config
        let config: Config = confy::load(APP_NAME, None).unwrap_or_default();
        // let _ = confy::store(APP_NAME, None, &config);

        // Start up the worker , separate thread , async runner
        let handle = Worker::spawn(config.clone());

        // Create a fresh application
        let state = AppState {
            notes: NotesUi::new(),
            worker: handle,
            mode: AppMode::Init,
            receiver_ticket: String::new(),
            current_text: String::new(),
            progress: ProgressList::new(),
            messages: Vec::new(),
            config: config,
            elapsed: None,
            share_ticket: None,
        };

        // New App
        let app = App {
            is_first_update: true,
            state,
        };

        // Run the egui in the foreground, worker as  a subthread (async)
        eframe::run_native(APP_NAME, options, Box::new(|_cc| Ok(Box::new(app))))
    }
}

// Actual gui code (the interface)
impl AppState {
    fn update(&mut self, ctx: &egui::Context) {
        // Events from the worker
        while let Ok(event) = self.worker.event_rx.try_recv() {
            match event {
                Event::Message(m) => {
                    if self.messages.len() > MESSAGE_MAX {
                        let _ = self.messages.remove(0);
                    }
                    self.messages.push(m);
                }
                Event::Progress((name, current, total)) => {
                    self.progress.insert(name, current, total);
                }
                Event::Finished => {
                    self.cmd(Command::ResetTimer);
                    self.mode = AppMode::Finished;
                }
                Event::ProgressFinished(name) => self.progress.finish(name),
                Event::ProgressComplete(name) => self.progress.complete(name),
                Event::Tick(seconds) => {
                    self.elapsed = Some(seconds);
                }
                Event::StopTick => {
                    self.elapsed = None;
                }
                Event::SendConfig(config) => {
                    self.config = config;
                    let _ = confy::store(APP_NAME, None, &self.config);
                }
                Event::NoteList(list) => {
                    self.notes.update(list);
                }
                Event::SendNote(note) => {
                    self.notes.set(note.clone());
                    self.current_text = note.text;
                }
                Event::SendShareTicket(share_ticket) => {
                    self.share_ticket = Some(share_ticket);
                }
            }
        }

        // active flags
        let mut change_enabled: bool = true;

        // Use the mode to enable and disable
        match self.mode {
            AppMode::Init => {
                if let Some(doc_id) = &self.config.doc_key {
                    self.cmd(Command::DocId(doc_id.clone()));
                    self.cmd(Command::GetNotes);
                    self.mode = AppMode::Idle;
                } else {
                    self.mode = AppMode::GetDocTicket;
                }
            }
            AppMode::Finished => {
                self.cmd(Command::ResetTimer);
                self.mode = AppMode::Idle;
            }
            AppMode::Config => {
                change_enabled = false;
            }
            _ => {}
        }

        // The actual gui
        // the lower panel
        self.footer(ctx);
        // the side panel
        self.side_panel(ctx);

        // Main panel
        egui::CentralPanel::default().show(ctx, |ui| {
            // Main buttons
            ui.add_space(5.);
            self.button_header(change_enabled, ui);
            // gap
            ui.separator();
            // Modal Display
            self.modal_display_above(ui);
            // Show the current progress bars
            self.show_progress(ui);
            // Show the current messages
            self.show_messages(ui);
        });
    }

    fn side_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("doc list")
            .resizable(false)
            .default_width(160.0)
            .min_width(160.0)
            .show(ctx, |ui| {
                if let Some(name) = self.notes.show(ui) {
                    self.cmd(Command::GetNote(name));
                }
            });
    }

    fn footer(&mut self, ctx: &egui::Context) {
        // Status bar at the bottom
        // egui needs outer things done first
        // the status bar at the bottom.
        egui::TopBottomPanel::bottom("status bar").show(ctx, |ui| {
            ui.add_space(5.);
            ui.horizontal(|ui| {
                if ui.button("Clear").clicked() {
                    // Reset the timer for good measure
                    self.cmd(Command::ResetTimer);
                    self.reset();
                }
                ui.add_space(5.);

                // mode and timer
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(elapsed_seconds) = self.elapsed {
                        ui.label(RichText::new(format_seconds_as_hms(elapsed_seconds)).strong());
                    }
                    ui.label(format!(" {} ", self.mode));
                });
            });
            ui.add_space(5.);
        });
    }

    // The buttons at the top
    fn button_header(&mut self, send_enabled: bool, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.add_space(2.);
            ui.add_enabled_ui(send_enabled, |ui| {
                if ui.button("List Notes").clicked() {
                    self.cmd(Command::GetNotes);
                };
                if ui.button("Share...").clicked() {
                    self.cmd(Command::GetShareTicket);
                    self.mode = AppMode::ShareTicket;
                }
                ui.add_space(20.);
                if ui.button("About").clicked() {
                    self.mode = AppMode::About;
                }
                if ui.button("Config").clicked() {
                    self.mode = AppMode::Config;
                }
            });
            ui.add_space(5.);
        });
    }

    // modal display above progress and messages
    fn modal_display_above(&mut self, ui: &mut Ui) {
        // Show mode based widgets
        match self.mode {
            AppMode::Init => {}
            AppMode::Idle => {
                markdown::show(ui,&self.current_text);
            }
            AppMode::Edit => {
                let _current_doc = egui::TextEdit::multiline(&mut self.current_text)
                    .desired_width(f32::INFINITY)
                    .show(ui);
            }
            AppMode::Finished => {}
            AppMode::Config => {
                self.show_config(ui);
            }
            AppMode::About => self.about(ui),
            AppMode::GetDocTicket => {
                self.ticket_box(ui);
            }
            AppMode::ShareTicket => {
                if let Some(ticket) = &self.share_ticket {
                    ui.add_space(10.);
                    ui.label("Doc Share Ticket...");
                    ui.add_space(5.);
                    ui.separator();
                    ui.add_space(10.);
                    ui.label(RichText::new(ticket).strong().font(FontId::monospace(15.)));
                    ui.add_space(10.);
                    ui.separator();
                    if ui.button("Done").clicked() { 
                        self.mode = AppMode::Idle;
                    }
                }
            }
        }
    }

    // Show the config editor ,  needs a restart to work
    fn show_config(&mut self, ui: &mut Ui) {
        // config editor
        // LATER need a fall back config on cancel
        ui.label("Configuration");
        ui.add_space(5.);
        ui.separator();
        ui.small("Display Mode");
        ui.checkbox(&mut self.config.dark_mode, "Darkmode");
        ui.add_space(5.);
        ui.small("Download Path");
        ui.horizontal(|ui| {
            ui.label(self.config.download_path.display().to_string());
            if ui.button("Change").clicked() {
                let mut new_path = FileDialog::new();
                new_path = new_path.set_directory(self.config.download_path.as_path());
                if let Some(path) = new_path.pick_folder() {
                    info!("new export path {}", path.display().to_string());
                    self.config.download_path = path;
                }
            }
        });
        ui.separator();

        if ui.button("Save Config").clicked() {
            let message = MessageDisplay {
                text: "Config updated".to_string(),
                mtype: MessageType::Good,
            };
            self.messages.push(message);
            // Save the config to file
            let _ = confy::store(APP_NAME, None, &self.config);
            // Push the config down to the worker
            self.cmd(Command::SendConfig(self.config.clone()));
            // Set idle
            self.mode = AppMode::Idle;
        }
    }

    // About panel
    fn about(&mut self, ui: &mut Ui) {
        ui.label(ABOUT);
        ui.add_space(10.);
        let _ = ui.hyperlink("https://github.com/zignig/liminal-docs");
        ui.add_space(10.);
        ui.separator();
        if ui.button("Awesome!").clicked() {
            self.mode = AppMode::Idle;
        }
    }

    // Show the blob ticket fetch box
    fn ticket_box(&mut self, ui: &mut Ui) {
        ui.label("Docs share ticket");
        ui.add_space(8.);
        let _ticket_edit = egui::TextEdit::multiline(&mut self.receiver_ticket)
            .desired_width(f32::INFINITY)
            .show(ui);
        ui.add_space(5.);
        ui.horizontal(|ui| {
            if ui.button("Get Doc").clicked() {
                // Fetch to the default path
                self.cmd(Command::DocTicket(self.receiver_ticket.clone()));
                self.mode = AppMode::Idle;
            };
        });
    }

    // Reset the application
    fn reset(&mut self) {
        self.mode = AppMode::Idle;
        self.receiver_ticket = "".to_string();
        self.messages = Vec::new();
        self.progress.clear();
    }

    // Show the list of progress bars
    fn show_progress(&mut self, ui: &mut Ui) {
        ui.add_space(4.);
        self.progress.show(ui);
    }

    // Show the list of messages
    fn show_messages(&mut self, ui: &mut Ui) {
        ui.add_space(4.);
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .max_width(f32::INFINITY)
            .show(ui, |ui| {
                let ui_builder = egui::UiBuilder::new();
                ui.scope_builder(ui_builder, |ui| {
                    egui::Grid::new("message_grid")
                        .num_columns(1)
                        .spacing([40.0, 4.0])
                        .striped(true)
                        .show(ui, |ui| {
                            for message in self.messages.iter() {
                                message.show(ui);
                                ui.end_row();
                            }
                        });
                });
            });
    }

    // Send command to the worker.
    fn cmd(&self, command: Command) {
        self.worker
            .command_tx
            .send_blocking(command)
            .expect("Worker is not responding");
    }
}

fn format_seconds_as_hms(total_seconds: u64) -> String {
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    format!("[{:02}:{:02}:{:02}]", hours, minutes, seconds)
}

// ------
// Note User Interface side
// ------
pub struct NotesUi {
    notes: BTreeMap<String, (bool, Option<Note>)>,
}

impl NotesUi {
    pub fn new() -> Self {
        Self {
            notes: BTreeMap::new(),
        }
    }
}

impl NotesUi {
    fn update(&mut self, names: Vec<String>) {
        for note in names {
            self.notes.insert(note, (false, None));
        }
    }

    fn set(&mut self, note: Note) {
        self.notes.insert(note.id.clone(), (true, Some(note)));
    }

    fn show(&mut self, ui: &mut Ui) -> Option<String> {
        ui.add_space(10.);
        let mut val = None;
        ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
            let mut active_pos = usize::MAX;

            for (pos, (name, (active, _note))) in self.notes.iter_mut().enumerate() {
                if ui.toggle_value(active, name).clicked() {
                    active_pos = pos;
                    val = Some(name.clone());
                }
            }
            // Make sure only one is active
            if active_pos != usize::MAX {
                for (pos, (name, (active, _note))) in self.notes.iter_mut().enumerate() {
                    if active_pos != pos {
                        *active = false;
                    }
                }
            }
        });
        return val;
    }
}
