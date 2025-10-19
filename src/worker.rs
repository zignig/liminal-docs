// --------------------------
// Worker
// --------------------------

use std::{str::FromStr, time::Duration};

use crate::comms::{Command, Config, Event, MessageOut};
use crate::notes::Notes;
use anyhow::Result;
use async_channel::{Receiver, Sender};
use iroh::protocol::Router;
// use iroh::protocol::Router;
use iroh::{Endpoint, SecretKey};
use iroh_blobs::BlobsProtocol;
use iroh_docs::{AuthorId, NamespaceId};
use iroh_docs::{DocTicket, engine::LiveEvent, protocol::Docs};
use iroh_gossip::net::Gossip;
use n0_future::{FuturesUnordered, Stream, StreamExt};
<<<<<<< HEAD
use tokio::{
    sync::Notify,
    time::{Instant, interval},
};
=======
use n0_watcher::Watcher;
use tokio::time::{Instant, interval};
>>>>>>> 714d4a60e63f724b12dd0b0e45b244126890d139
use tracing::{error, info, warn};

pub struct Worker {
    pub command_rx: Receiver<Command>,
    pub command_tx: Sender<Command>,
    pub mess: MessageOut,
    pub timer_out: Sender<TimerCommands>,
    pub blobs: BlobsProtocol,
    _endpoint: Endpoint,
    pub notes: Option<Notes>,
    _gossip: Gossip,
    pub docs: Docs,
    pub config: Config,
    _router: Router,
    pub tasks: FuturesUnordered<n0_future::boxed::BoxFuture<()>>,
    retry: u32,
}

pub struct WorkerHandle {
    pub command_tx: Sender<Command>,
    pub event_rx: Receiver<Event>,
}

impl Worker {
    pub fn spawn(config: Config) -> WorkerHandle {
        let (command_tx, command_rx) = async_channel::bounded(16);
        let (event_tx, event_rx) = async_channel::bounded(16);
        // can send commands to itself
        let command_tx_self = command_tx.clone();
        // make the handle
        let handle = WorkerHandle {
            command_tx,
            event_rx,
        };

        // Spawn a new worker as a seperate thread.
        //  egui is sync the worker is async , comms are a channel of commands and events
        // events are wrapped in MessageOut for formatting and goodness

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to start tokio runtime");
            rt.block_on(async move {
                let mut worker = Worker::start(command_rx, command_tx_self, event_tx, config)
                    .await
                    .expect("Worker failed to start");
                if let Err(err) = worker.run().await {
                    warn!("worker stopped with error {err:?}");
                }
            })
        });
        handle
    }

    //
    async fn start(
        command_rx: async_channel::Receiver<Command>,
        // Send commands to myself
        command_tx: async_channel::Sender<Command>,
        event_tx: async_channel::Sender<Event>,
        config: Config,
    ) -> Result<Self> {
        let mess = MessageOut::new(event_tx.clone());
        // Channel for the timer
        let (timer_out, timer_in) = async_channel::bounded(16);
        // start the background timer ( 1 second interval if running )
        let timer = TimerTask::new(mess.clone());
        // Run the timer
        timer.run(timer_in);

        // Create the endpoint
        let secret_key = SecretKey::from_str(config.secret_key.as_str())?;
        let endpoint = Endpoint::builder()
            .secret_key(secret_key)
            .discovery_n0()
            .bind()
            .await?;

        // Create the blob store
        let mut blob_path = config.store_path.clone();
        blob_path.push("blobs");
        let store = iroh_blobs::store::fs::FsStore::load(&blob_path)
            .await
            .unwrap();
        let blobs = iroh_blobs::BlobsProtocol::new(&store, None);

        // Create the gossip
        let gossip = Gossip::builder().spawn(endpoint.clone());

        // Create the doc store

        let docs_path = config.store_path.clone();
        let docs = Docs::persistent(docs_path)
            .spawn(endpoint.clone(), (*blobs).clone(), gossip.clone())
            .await?;

        // The unbuilt notes
        let notes = None;

        // make the router
        let router = iroh::protocol::Router::builder(endpoint.clone())
            .accept(iroh_gossip::ALPN, gossip.clone())
            .accept(iroh_blobs::ALPN, blobs.clone())
            .accept(iroh_docs::ALPN, docs.clone())
            .spawn();

        // Create the task set
        let tasks = FuturesUnordered::<n0_future::boxed::BoxFuture<()>>::new();

        // Send a set ready, race condition on the create vs connect to docs
        mess.set_ready().await?;

        // Make the worker
        Ok(Self {
            command_rx,
            command_tx,
            mess,
            timer_out,
            blobs,
            _endpoint: endpoint,
            _gossip: gossip,
            docs,
            config,
            notes,
            _router: router,
            tasks,
            retry: 1,
        })
    }

    async fn run(&mut self) -> Result<()> {
        // the actual runner for the worker
        // this is where the events are processed.
        info!("Starting  the worker");
        loop {
            tokio::select! {
                command = self.command_rx.recv() => {
                    let command = command?;
                    // if the handle fails send a red message to the app.
                    if let Err(err ) = self.handle_command(command).await{
                        self.mess.error(format!("{}",err).as_str()).await?;
                        warn!("command failed {err}");
                        self.mess.finished().await?;
                    }
                }
                // Run everything in the task pool
                // this needs a bit more defn.
                // TODO move the timer into this task pool.
                _ = self.tasks.next(), if !self.tasks.is_empty() => {}
            }
        }
    }

    // handle the incoming commands from the egui
    // this is where the main actions for the worker happen
    async fn handle_command(&mut self, command: Command) -> Result<()> {
        match command {
            Command::Setup { callback } => {
                // lodge the redraw callback into the message updater
                let _ = self.mess.set_callback(callback).await?;
                // Say ready
                self.mess.good("Ready...").await?;
                return Ok(());
            }

            // Attach the Document to the mother ship
            // and increment the retry
            Command::Attach => {
                // this happens when the system trys to reattach
                // rather than a command from the egui
                // weird by OK
                if let Some(notes ) = &self.notes { 
                    self.run_sync(notes.clone(),self.command_tx.clone()).await?;
                }
                return Ok(());
            }
            // Already set up in config ( attach by id )
            Command::DocId(id) => {
                info!("Create doc from id {}", id);
                let id = NamespaceId::from_str(id.as_str())?;
                let author_id = self.author().await?;
                let notes =
                    Notes::from_id(id, author_id, self.blobs.clone(), self.docs.clone()).await?;
                // Subscribe and get synced
                warn!("Start sync");
                // Start the subscripion
                self.run_sync(notes.clone(), self.command_tx.clone())
                    .await?;
                warn!("Finish sync");
                self.notes = Some(notes);
                return Ok(());
            }

            // No doc in the config , use a doc share ticket.
            Command::DocTicket(ticket) => {
                // schnaffle the ticket into the config
                let doc_ticket = DocTicket::from_str(ticket.as_str())?;
                info!("{:#?}", &doc_ticket);
                self.config.doc_key = Some(doc_ticket.capability.id().to_string());

                // Create a new author if none ( not using default notes id )
                let author_id = self.author().await?;

                warn!("Create the notes object");

                // Make  new note set
                let notes = Notes::new(
                    Some(ticket),
                    author_id,
                    self.blobs.clone(),
                    self.docs.clone(),
                )
                .await?;

                warn!("Start Sync");
                // create the task to replicate, this needs a retry syste
                self.run_sync(notes.clone(), self.command_tx.clone())
                    .await?;

                // nice some notes
                self.notes = Some(notes);
                // if there is a new author , push it up to the app and config file
                self.save_config().await?;
                info!("exit new ticket");
                // looks good.
                return Ok(());
            }
<<<<<<< HEAD
=======

            // Clear the timer in egui
>>>>>>> 714d4a60e63f724b12dd0b0e45b244126890d139
            Command::ResetTimer => {
                self.reset_timer().await?;
                self.start_timer().await?;
                return Ok(());
            }

            // Confing from the egui application
            Command::SendConfig(config) => {
                self.config = config;
                return Ok(());
            }

            // Modified note
            Command::SaveNote(id, text) => {
                warn!("note info => \"{}\" \"{}\"", id, text);
                if let Some(notes) = &self.notes {
                    notes.update_note(id, text).await?;
                }
                return Ok(());
            }

            // Nice, a new note to create...
            Command::NewNote(id, text) => {
                warn!("create note => \"{}\" \"{}\"", id, text);
                if let Some(notes) = &self.notes {
                    notes.create(id, text).await?;
                }
                return Ok(());
            }

            // Get a list of existing notes
            // Not not ids but actual names
            Command::GetNotes => {
                if let Some(notes) = &self.notes {
                    let note_list = notes.get_note_vec().await;
                    self.mess.send_note_list(note_list).await?;
                }
                return Ok(());
            }

            // Grab a single note
            Command::GetNote(id) => {
                if let Some(notes) = &self.notes {
                    let note = notes.get_note(id).await?;
                    self.mess.send_note(note).await?;
                }
                return Ok(());
            }

            // Get the ticket , this is RW for now
            // dangerous mostly, but whatever
            Command::GetShareTicket => {
                if let Some(notes) = &self.notes {
                    let share_ticket = notes.ticket();
                    self.mess.share_ticket(share_ticket).await?;
                }
                return Ok(());
            }

            // Take the marked notes and actually delete the data.
            Command::DeleteHidden => {
                if let Some(notes) = &self.notes {
                    notes.delete_hidden().await?;
                    self.mess.info("delete hidden").await?;
                }
                return Ok(());
            }

            // Mark the note for deletion, does not actually make it go away.
            Command::HideNote(id) => {
                if let Some(notes) = &self.notes {
                    // notes.delete_note(id).await?;
                    notes.set_delete(id.clone()).await?;
                    let info = notes.get_note(id).await?;
                    println!("{:#?}", info);
                    self.mess.info("hide note").await?;
                }
                return Ok(());
            }
        }
    }

    // Config save, push the config up to app for file save
    async fn save_config(&mut self) -> Result<()> {
        // move the config up to the gui and save.
        warn!("Save the config {:#?}", &self.config);
        self.mess.send_config(self.config.clone()).await?;
        // println!("{:#?}", &self.config);
        Ok(())
    }

    // Author maker
    // If the author does not exist make a fresh one.
    async fn author(&mut self) -> Result<AuthorId> {
        let author = match &self.config.author {
            Some(author) => AuthorId::from_str(&author)?,
            None => {
                let author = self.docs.author_create().await?;
                self.config.author = Some(format!("{}", author));
                self.save_config().await?;
                author
            }
        };
        Ok(author)
    }

    // Async task attachment
    // TODO , this needs some love
    // should watch the retry count and BUG out if needed.
    async fn run_sync(
        &mut self,
        notes: Notes,
        command_tx: async_channel::Sender<Command>,
    ) -> Result<()> {
        warn!("Start the sync task");
        warn!("Retry {}",self.retry);
        notes.share().await?;
        let events = notes.doc_subscribe().await?;
        let mess = self.mess.clone();
        let attached = notes.attached().await;
        self.tasks.push(Box::pin(subscription_events(
            events,
            mess,
            command_tx,
            self.retry,
            attached,
        )));
        warn!("Task should be attached");
        self.retry += 1 ;
        Ok(())
    }

    // -----
    // Timer functions
    // worker interactions with the timer.
    //------

    async fn start_timer(&mut self) -> Result<()> {
        info!("Start Timer");
        self.timer_out.send(TimerCommands::Start).await?;
        Ok(())
    }

    async fn reset_timer(&mut self) -> Result<()> {
        info!("Stop timer");
        self.timer_out.send(TimerCommands::Reset).await?;
        Ok(())
    }
}

// Replica event runner
// Weidly this needs to be it's own function outside the struct.
// TODO , it needs more notify and bugout.
async fn subscription_events(
    events: impl Stream<Item = Result<LiveEvent>>,
    mess: MessageOut,
    command_tx: async_channel::Sender<Command>,
    retry: u32,
    attached: bool,
) {
    warn!("Starting Event Runner");
    let mut timer = interval(Duration::from_secs(30));

    // Retry logic.
    let base: u64 = 2;
    let retry_timer = tokio::time::sleep(Duration::from_secs(base.pow(retry)));

    tokio::pin!(events);
    tokio::pin!(retry_timer);
    loop {
        tokio::select! {
            Some(event) = events.next() => {
                let event = match event {
                    Ok(event) => event,
                    Err(err) => {
                        mess.error(format!("{:#?}",err).as_str()).await.unwrap();
                        break;
                    },
                };
                match event {
                    LiveEvent::InsertRemote{from: _, ref entry,content_status: _} => {
                        warn!("remote entry => {:#?}",entry);
                        // TODO push up into gui.
                        // command_tx.send(Command::)
                    }
                    LiveEvent::SyncFinished( ref sync_event) => {
                        match  &sync_event.result  {
                            Ok(_) => {},
                            Err(err) => {
                                mess.error(format!("{:#?}",err).as_str()).await.unwrap();
                                mess.error("TODO try again").await.unwrap();
                                // command_tx.send(Command::Attach).await.unwrap();
                                // break;
                                // exit the loop and try to attach again.
                            },
                        };
                    },
                    // Finshed sync (maybe) , update the notes...
                    LiveEvent::PendingContentReady => {
                        mess.good("Content Ready").await.unwrap();
                        command_tx.send(Command::GetNotes).await.unwrap();
                    },
                    // Unhandled event , janky
                    _ => {}
                }
                // mess.good(format!("{:#?}",event).as_str()).await.unwrap();
                warn!("{:#?}", &event);
            },
            _ = timer.tick() => {
                warn!("tick");
                // TODO check doc sync status and restart if needed.
            }
            _ = &mut retry_timer, if (!attached & (retry < 5)) => {
                warn!("retry");
                command_tx.send(Command::Attach).await.unwrap();
                break;
            }
        }
    }
    error!("Event runner exited (BAD)");
}

// ----------
// Timer runner
// TODO move this into the task pool
// ----------

#[derive(Debug)]
pub enum TimerCommands {
    Start,
    Reset,
}
pub struct TimerTask {
    mess: MessageOut,
}

// Runs as a seperate tokio task, boops every second
// Only sends a message time if its running
impl TimerTask {
    pub fn new(mess: MessageOut) -> Self {
        Self { mess }
    }

    pub fn run(self, incoming: Receiver<TimerCommands>) {
        let _ = tokio::spawn(async move {
            // every second , variables are local to the thread.
            let mut interval = interval(Duration::from_millis(1000));
            let mut running = true;
            let mess = self.mess.clone();
            let mut start_time = Instant::now();

            loop {
                tokio::select! {
                    command  = incoming.recv() => {
                       let command = command.unwrap() ;
                           info!("timer -- {:?}",command);
                       match command {
                        TimerCommands::Start => { start_time = Instant::now(); running = true;},
                        TimerCommands::Reset => { running = false ; let _ = mess.reset_timer().await; } ,
                      };
                    }
                    _ = interval.tick() => {
                    if running {
                        let since = start_time.elapsed().as_secs();
                        let _ = mess.tick(since).await;
                    }
                    }
                }
            }
        });
    }
}

// End of line.
