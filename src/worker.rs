// --------------------------
// Worker
// --------------------------

use std::{str::FromStr, sync::Arc, time::Duration};

use crate::comms::{Command, Config, Event, MessageOut};
use crate::notes::Notes;
use anyhow::Result;
use async_channel::{Receiver, Sender};
// use iroh::protocol::Router;
use iroh::{Endpoint, SecretKey};
use iroh_blobs::BlobsProtocol;
use iroh_docs::{AuthorId, NamespaceId};
use iroh_docs::{DocTicket, engine::LiveEvent, protocol::Docs};
use iroh_gossip::net::Gossip;
use n0_future::{FuturesUnordered, Stream, StreamExt};
use n0_watcher::Watcher;
use tokio::{
    sync::Notify,
    time::{Instant, interval},
};
use tracing::{error, info, warn};

pub struct Worker {
    pub command_rx: Receiver<Command>,
    pub mess: MessageOut,
    pub timer_out: Sender<TimerCommands>,
    pub blobs: BlobsProtocol,
    pub send_notify: Arc<Notify>,
    //pub endpoint: Endpoint,
    pub notes: Option<Notes>,
    //pub gossip: Gossip,
    pub docs: Docs,
    pub config: Config,
    //pub router: Router,
    pub tasks: FuturesUnordered<n0_future::boxed::BoxFuture<()>>,
}

pub struct WorkerHandle {
    pub command_tx: Sender<Command>,
    pub event_rx: Receiver<Event>,
}

impl Worker {
    pub fn spawn(config: Config) -> WorkerHandle {
        let (command_tx, command_rx) = async_channel::bounded(16);
        let (event_tx, event_rx) = async_channel::bounded(16);
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
                let mut worker = Worker::start(command_rx, event_tx, config)
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

        // Wait for the base node to stabilize
        let _ = endpoint.home_relay().initialized().await;
        let addr = endpoint.node_addr().initialized().await;
        warn!("{:#?}", addr);

        // Create the blob store
        let mut blob_path = config.store_path.clone();
        blob_path.push("blobs");
        let store = iroh_blobs::store::fs::FsStore::load(&blob_path)
            .await
            .unwrap();
        let blobs = iroh_blobs::BlobsProtocol::new(&store, endpoint.clone(), None);

        // Create the gossip
        let gossip = Gossip::builder().spawn(endpoint.clone());

        // Create the doc store

        let docs_path = config.store_path.clone();
        let docs = Docs::persistent(docs_path)
            .spawn(endpoint.clone(), (*blobs).clone(), gossip.clone())
            .await?;

        // the notify
        let send_notify = Arc::new(Notify::new());
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
            mess,
            timer_out,
            blobs,
            send_notify,
            //endpoint,
            //gossip,
            docs,
            config,
            notes,
            //router,
            tasks,
        })
    }

    async fn run(&mut self) -> Result<()> {
        // the actual runner for the worker
        info!("Starting  the worker");
        loop {
            tokio::select! {
                command = self.command_rx.recv() => {
                    let command = command?;
                    if let Err(err ) = self.handle_command(command).await{
                        self.mess.error(format!("{}",err).as_str()).await?;
                        warn!("command failed {err}");
                        self.mess.finished().await?;
                    }
                }
                // Run everything in the task pool
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
                self.run_sync(notes.clone()).await?;
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

                // Create a new author if none ( not using default )
                let author_id = self.author().await?;

                warn!("Create the notes object");
                // Grab the docs
                let notes = Notes::new(
                    Some(ticket),
                    author_id,
                    self.blobs.clone(),
                    self.docs.clone(),
                )
                .await?;
                warn!("Start Sync");
                // create the task to replicate
                self.run_sync(notes.clone()).await?;

                self.notes = Some(notes);
                self.save_config().await?;
                info!("exit new ticket");
                return Ok(());
            }
            Command::CancelSend => {
                info!("Finish the send runner!!");
                self.send_notify.notify_waiters();
                self.reset_timer().await?;
                return Ok(());
            }
            Command::ResetTimer => {
                self.reset_timer().await?;
                self.start_timer().await?;
                return Ok(());
            }
            Command::SendConfig(config) => {
                self.config = config;
                return Ok(());
            }
            Command::GetNotes => {
                if let Some(notes) = &self.notes {
                    let note_list = notes.get_note_vec().await;
                    self.mess.send_note_list(note_list).await?;
                }
                return Ok(());
            }
            Command::GetNote(id) => {
                if let Some(notes) = &self.notes {
                    let note = notes.get_note(id).await?;
                    self.mess.send_note(note).await?;
                }
                return Ok(());
            }
            Command::GetShareTicket => {
                if let Some(notes) = &self.notes {
                    let share_ticket = notes.ticket();
                    self.mess.share_ticket(share_ticket).await?;
                }
                return Ok(());
            }
        }
    }

    // Config save
    async fn save_config(&mut self) -> Result<()> {
        // move the config up to the gui and save.
        warn!("Save the config");
        self.mess.send_config(self.config.clone()).await?;
        // println!("{:#?}", &self.config);
        Ok(())
    }

    // Author maker
    async fn author(&mut self) -> Result<AuthorId> {
        let author = match &self.config.author {
            Some(author) => AuthorId::from_str(&author)?,
            None => {
                let author = self.docs.author_create().await?;
                self.config.author = Some(format!("{}", author));
                author
            }
        };
        self.save_config().await?;
        Ok(author)
    }

    // Async task attachment
    // TODO run up a document sync and add to the join set (self.tasks)
    // eg https://github.com/n0-computer/iroh-smol-kv/blob/main/src/lib.rs#L753
    async fn run_sync(&mut self, notes: Notes) -> Result<()> {
        warn!("Start the sync task");
        let events = notes.doc_subscribe().await?;
        let mess = self.mess.clone();
        self.tasks.push(Box::pin(subscription_events(events, mess)));
        warn!("Task should be attached");
        Ok(())
    }

    // -----
    // Timer functions
    //------

    async fn start_timer(&mut self) -> Result<()> {
        // info!("Start Timer");
        self.timer_out.send(TimerCommands::Start).await?;
        Ok(())
    }

    async fn reset_timer(&mut self) -> Result<()> {
        // info!("Stop timer");
        self.timer_out.send(TimerCommands::Reset).await?;
        Ok(())
    }
}

// Replica event runner
async fn subscription_events(mut events: impl Stream<Item = Result<LiveEvent>>, mess: MessageOut) {
    warn!("Starting Event Runner");
    let mut timer = interval(Duration::from_millis(5000));
    tokio::pin!(events);
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
                    LiveEvent::SyncFinished( ref sync_event) => {
                        match  &sync_event.result  {
                            Ok(_) => {},
                            Err(err) => {
                        mess.error(format!("{:#?}",err).as_str()).await.unwrap();
                        mess.error("TODO try again").await.unwrap();
                            },
                        };
                    },
                    _ => {}
                }
                mess.good(format!("{:#?}",event).as_str()).await.unwrap();
                warn!("{:#?}", &event);
            },
            _ = timer.tick() => {
                warn!("tick");
            }
        }
    }
    error!("Event runner exited (BAD)");
}

// ----------
// Timer runner
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
