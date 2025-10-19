// Comms between the gui and  the worker in it's own module.
// Some of this lives on both sides ( be careful )

use std::{path::PathBuf, sync::Arc};

use anyhow::Result;
use async_channel::Sender;
use eframe::egui::{self};

use egui::{Color32, Ui};
use serde_derive::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::notes::Note;

// Application Configuration
// Application saved config
// Default impl in app.rs for visibilibly
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    pub dark_mode: bool,
    pub download_path: PathBuf,
    pub store_path: PathBuf,
    pub secret_key: String,
    pub doc_key: Option<String>,
    pub author: Option<String>,
    pub mothership: Option<String>,
}

// Update Callback
type UpdateCallback = Box<dyn Fn() + Send + 'static>;

// Outgoing event to the application
// TODO , some of the naming convention semaitics are wrong
// Chose a side , (probably the egui , send == from , get == to)
// rework
pub enum Event {
    Message(MessageDisplay),
    SendConfig(Config),
    SendShareTicket(String),
    NoteList(Vec<String>),
    SendNote(Note),
    Tick(u64),
    StopTick,
    Finished,
    SetReady,
}

// Incoming commands from the egui interface
// and the actor loop on  replication events.
pub enum Command {
    Setup { callback: UpdateCallback },
    DocTicket(String),
    DocId(String),
    GetShareTicket,
    GetNotes,
    GetNote(String),
    SendConfig(Config),
    SaveNote(String,String),
    NewNote(String,String),
    ResetTimer,
    DeleteHidden,
    HideNote(String),
    Attach,
}

// Message types
// display messsages in the egui
#[derive(Clone)]
pub enum MessageType {
    Good,
    Info,
    Error,
}

// egui display struct.
#[derive(Clone)]
pub struct MessageDisplay {
    pub text: String,
    pub mtype: MessageType,
}

// Messaging, displayed messages in the egui.
// this wraps the communications from worker to gui
// it's a little boilerplatey but it bridges nicely
#[derive(Clone)]
pub struct MessageOut(Arc<Mutex<MessageInner>>);

// Arc for cloneability.
pub struct MessageInner {
    event_tx: Sender<Event>,
    callback: Option<UpdateCallback>,
}

impl MessageOut {
    // Make the original message machine
    pub fn new(event_tx: Sender<Event>) -> Self {
        Self(Arc::new(Mutex::new(MessageInner {
            event_tx,
            callback: None,
        })))
    }

    // For  the gui to be interactive a callback 
    // needs to be run when a message is sent.
    // for interior mutability this needs to be wrapped in a mutex
    pub async fn set_callback(&self, callback: UpdateCallback) -> Result<()> {
        let mut value = self.0.lock().await;
        value.callback = Some(callback);
        Ok(())
    }

    // Function for sending a message ( Extracted for the mutex)
    async fn emit(&self, event: Event) -> Result<()> {
        let binding = self.0.lock().await;
        if let Some(callback) = &binding.callback {
            callback();
        }
        binding.event_tx.send(event).await.unwrap();
        Ok(())
    }

    // Yellow text
    pub async fn info(&self, message: &str) -> Result<()> {
        self.emit(Event::Message(MessageDisplay {
            text: message.to_string(),
            mtype: MessageType::Info,
        }))
        .await?;
        Ok(())
    }

    // Green text 
    pub async fn good(&self, message: &str) -> Result<()> {
        self.emit(Event::Message(MessageDisplay {
            text: message.to_string(),
            mtype: MessageType::Good,
        }))
        .await?;
        Ok(())
    }

    pub async fn error(&self, message: &str) -> Result<()> {
        self.emit(Event::Message(MessageDisplay {
            text: message.to_string(),
            mtype: MessageType::Error,
        }))
        .await?;
        Ok(())
    }

    // Say finished to the gui (hangove from sendme)
    pub async fn finished(&self) -> Result<()> {
        self.emit(Event::Message(MessageDisplay {
            text: "Finished...".to_string(),
            mtype: MessageType::Good,
        }))
        .await?;
        self.emit(Event::Finished).await?;
        Ok(())
    }

    // Send a clock update , show the clock in the gui
    pub async fn tick(&self, since: u64) -> Result<()> {
        self.emit(Event::Tick(since)).await?;
        Ok(())
    }

    // Stop showing clock in the gui.
    pub async fn reset_timer(&self) -> Result<()> {
        self.emit(Event::StopTick).await?;
        Ok(())
    }

    // Send the config up to the gui and save to file in app
    pub async fn send_config(&self, config: Config) -> Result<()> {
        self.emit(Event::SendConfig(config)).await?;
        Ok(())
    }

    // Send note list up to the gui
    pub async fn send_note_list(&self, note_list: Vec<String>) -> Result<()> {
        self.emit(Event::NoteList(note_list)).await?;
        Ok(())
    }

    // Send note up to the gui
    pub async fn send_note(&self, note: Note) -> Result<()> {
        self.emit(Event::SendNote(note)).await?;
        Ok(())
    }

    // Send the share ticket up to the gui
    pub async fn share_ticket(&self, share_ticket: String) -> Result<()> {
        self.emit(Event::SendShareTicket(share_ticket)).await?;
        Ok(())
    }

    // Send set ready.
    pub async fn set_ready(&self) -> Result<()> {
        self.emit(Event::SetReady).await?;
        Ok(())
    }

}

// Message formatting
impl MessageDisplay {
    pub fn show(&self, ui: &mut Ui) {
        match self.mtype {
            MessageType::Good => {
                let m = egui::RichText::new(&self.text)
                    .color(Color32::LIGHT_GREEN)
                    .family(egui::FontFamily::Monospace);
                ui.label(m);
            }
            MessageType::Info => {
                let m = egui::RichText::new(&self.text).family(egui::FontFamily::Monospace);
                ui.label(m);
            }
            MessageType::Error => {
                let m = egui::RichText::new(&self.text)
                    .color(Color32::LIGHT_RED)
                    .family(egui::FontFamily::Monospace);
                ui.label(m);
            }
        }
    }
}