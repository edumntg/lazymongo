//! The Mongo I/O actor: a single async task that owns the driver client and
//! the live query cursor. The UI sends [`Command`]s and receives
//! [`CoreEvent`]s over channels, so no network call ever blocks a frame.

use std::time::{Duration, Instant};

use futures_util::TryStreamExt;
use mongodb::bson::{doc, Document};
use mongodb::options::ClientOptions;
use mongodb::{Client, Cursor};
use tokio::sync::mpsc;

use crate::types::{CollectionInfo, Command, CoreEvent, DatabaseInfo, BATCH_SIZE};

const PING_INTERVAL: Duration = Duration::from_secs(10);
const SERVER_SELECTION_TIMEOUT: Duration = Duration::from_secs(5);

/// Spawn the actor on the current tokio runtime.
/// Returns the command sender and event receiver for the UI side.
pub fn spawn() -> (mpsc::Sender<Command>, mpsc::Receiver<CoreEvent>) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>(64);
    let (evt_tx, evt_rx) = mpsc::channel::<CoreEvent>(64);
    tokio::spawn(run(cmd_rx, evt_tx));
    (cmd_tx, evt_rx)
}

struct Actor {
    client: Option<Client>,
    cursor: Option<Cursor<Document>>,
    generation: u64,
    events: mpsc::Sender<CoreEvent>,
}

async fn run(mut cmds: mpsc::Receiver<Command>, events: mpsc::Sender<CoreEvent>) {
    let mut actor = Actor {
        client: None,
        cursor: None,
        generation: 0,
        events,
    };
    // interval() fires immediately; delay the first health ping by one period.
    let mut ping =
        tokio::time::interval_at(tokio::time::Instant::now() + PING_INTERVAL, PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            cmd = cmds.recv() => match cmd {
                Some(cmd) => actor.handle(cmd).await,
                None => break, // UI dropped its sender: shut down
            },
            _ = ping.tick(), if actor.client.is_some() => actor.health_ping().await,
        }
    }
}

impl Actor {
    /// Free-standing emit so methods never hold `&Actor` across an await
    /// (the driver Cursor is !Sync, which would make the task future !Send).
    async fn emit(events: &mpsc::Sender<CoreEvent>, evt: CoreEvent) {
        // Receiver only disappears on shutdown; ignore the error.
        let _ = events.send(evt).await;
    }

    async fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::Connect { uri } => self.connect(uri).await,
            Command::ListDatabases => self.list_databases().await,
            Command::ListCollections { db } => self.list_collections(db).await,
            Command::StartFind {
                generation,
                db,
                coll,
                filter,
            } => self.start_find(generation, db, coll, filter).await,
            Command::NextBatch { generation } => self.next_batch(generation).await,
        }
    }

    async fn connect(&mut self, uri: String) {
        let result: anyhow::Result<(Client, String, u64)> = async {
            let mut opts = ClientOptions::parse(&uri).await?;
            opts.app_name = Some("lazymongo".into());
            opts.server_selection_timeout = Some(SERVER_SELECTION_TIMEOUT);
            let client = Client::with_options(opts)?;

            let admin = client.database("admin");
            let started = Instant::now();
            admin.run_command(doc! { "ping": 1 }).await?;
            let ping_ms = started.elapsed().as_millis() as u64;

            let version = match admin.run_command(doc! { "buildInfo": 1 }).await {
                Ok(info) => info.get_str("version").unwrap_or("?").to_string(),
                Err(_) => "?".to_string(), // buildInfo may be restricted; not fatal
            };
            Ok((client, version, ping_ms))
        }
        .await;

        match result {
            Ok((client, server_version, ping_ms)) => {
                self.client = Some(client);
                Self::emit(
                    &self.events,
                    CoreEvent::Connected {
                        server_version,
                        ping_ms,
                    },
                )
                .await;
            }
            Err(e) => Self::emit(&self.events, CoreEvent::ConnectFailed(e.to_string())).await,
        }
    }

    async fn health_ping(&mut self) {
        let Some(client) = &self.client else { return };
        let started = Instant::now();
        match client
            .database("admin")
            .run_command(doc! { "ping": 1 })
            .await
        {
            Ok(_) => {
                Self::emit(
                    &self.events,
                    CoreEvent::Ping {
                        ms: started.elapsed().as_millis() as u64,
                    },
                )
                .await
            }
            Err(e) => Self::emit(&self.events, CoreEvent::Error(format!("ping failed: {e}"))).await,
        }
    }

    async fn list_databases(&mut self) {
        let Some(client) = &self.client else { return };
        match client.list_databases().await {
            Ok(specs) => {
                let mut dbs: Vec<DatabaseInfo> = specs
                    .into_iter()
                    .map(|s| DatabaseInfo {
                        name: s.name,
                        size_on_disk: s.size_on_disk,
                    })
                    .collect();
                dbs.sort_by(|a, b| a.name.cmp(&b.name));
                Self::emit(&self.events, CoreEvent::Databases(dbs)).await;
            }
            Err(e) => {
                Self::emit(
                    &self.events,
                    CoreEvent::Error(format!("listDatabases: {e}")),
                )
                .await
            }
        }
    }

    async fn list_collections(&mut self, db: String) {
        let Some(client) = &self.client else { return };
        let database = client.database(&db);
        match database.list_collection_names().await {
            Ok(mut names) => {
                names.sort();
                let mut colls = Vec::with_capacity(names.len());
                for name in names {
                    let count = database
                        .collection::<Document>(&name)
                        .estimated_document_count()
                        .await
                        .ok();
                    colls.push(CollectionInfo {
                        name,
                        estimated_count: count,
                    });
                }
                Self::emit(&self.events, CoreEvent::Collections { db, colls }).await;
            }
            Err(e) => {
                Self::emit(
                    &self.events,
                    CoreEvent::Error(format!("listCollections({db}): {e}")),
                )
                .await
            }
        }
    }

    async fn start_find(&mut self, generation: u64, db: String, coll: String, filter: Document) {
        let Some(client) = &self.client else { return };
        self.generation = generation;
        self.cursor = None;

        let collection = client.database(&db).collection::<Document>(&coll);

        let total_estimate = if filter.is_empty() {
            collection.estimated_document_count().await.ok()
        } else {
            None
        };

        match collection.find(filter).batch_size(BATCH_SIZE as u32).await {
            Ok(cursor) => {
                self.cursor = Some(cursor);
                self.pull_batch(generation, total_estimate).await;
            }
            Err(e) => Self::emit(&self.events, CoreEvent::Error(format!("find: {e}"))).await,
        }
    }

    async fn next_batch(&mut self, generation: u64) {
        if generation != self.generation {
            return; // stale request from a superseded query
        }
        self.pull_batch(generation, None).await;
    }

    async fn pull_batch(&mut self, generation: u64, total_estimate: Option<u64>) {
        let Some(cursor) = &mut self.cursor else {
            return;
        };
        let mut docs = Vec::with_capacity(BATCH_SIZE);
        let mut exhausted = false;
        loop {
            match cursor.try_next().await {
                Ok(Some(doc)) => {
                    docs.push(doc);
                    if docs.len() >= BATCH_SIZE {
                        break;
                    }
                }
                Ok(None) => {
                    exhausted = true;
                    self.cursor = None;
                    break;
                }
                Err(e) => {
                    self.cursor = None;
                    Self::emit(&self.events, CoreEvent::Error(format!("cursor: {e}"))).await;
                    exhausted = true;
                    break;
                }
            }
        }
        Self::emit(
            &self.events,
            CoreEvent::Batch {
                generation,
                docs,
                exhausted,
                total_estimate,
            },
        )
        .await;
    }
}
