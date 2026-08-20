//! The Mongo I/O actor: a single async task that owns the driver client and
//! the live query cursor. The UI sends [`Command`]s and receives
//! [`CoreEvent`]s over channels, so no network call ever blocks a frame.

use std::time::{Duration, Instant};

use futures_util::TryStreamExt;
use mongodb::bson::{doc, Bson, Document};
use mongodb::options::ClientOptions;
use mongodb::{Client, Cursor, IndexModel};
use tokio::sync::mpsc;

use crate::types::{
    CollectionInfo, Command, CoreEvent, DatabaseInfo, FindSpec, IndexInfo, BATCH_SIZE,
};

const PING_INTERVAL: Duration = Duration::from_secs(10);
const SERVER_SELECTION_TIMEOUT: Duration = Duration::from_secs(5);
/// Server-side guardrails (FR-16).
const FIND_MAX_TIME: Duration = Duration::from_secs(30);
const COUNT_MAX_TIME: Duration = Duration::from_secs(15);

/// Spawn the actor on the current tokio runtime.
/// `read_only` rejects every write command at the I/O layer (FR-4).
pub fn spawn(read_only: bool) -> (mpsc::Sender<Command>, mpsc::Receiver<CoreEvent>) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>(64);
    let (evt_tx, evt_rx) = mpsc::channel::<CoreEvent>(64);
    tokio::spawn(run(cmd_rx, evt_tx, read_only));
    (cmd_tx, evt_rx)
}

struct Actor {
    client: Option<Client>,
    cursor: Option<Cursor<Document>>,
    generation: u64,
    read_only: bool,
    events: mpsc::Sender<CoreEvent>,
}

async fn run(mut cmds: mpsc::Receiver<Command>, events: mpsc::Sender<CoreEvent>, read_only: bool) {
    let mut actor = Actor {
        client: None,
        cursor: None,
        generation: 0,
        read_only,
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

    async fn emit_err(events: &mpsc::Sender<CoreEvent>, msg: String) {
        Self::emit(events, CoreEvent::Error(msg)).await;
    }

    async fn handle(&mut self, cmd: Command) {
        if self.read_only && cmd.is_write() {
            Self::emit_err(
                &self.events,
                "read-only mode: write operations are disabled".into(),
            )
            .await;
            return;
        }
        match cmd {
            Command::Connect { uri } => self.connect(uri).await,
            Command::SetReadOnly(ro) => self.read_only = ro,
            Command::ListDatabases => self.list_databases().await,
            Command::ListCollections { db } => self.list_collections(db).await,
            Command::StartFind {
                generation,
                db,
                coll,
                spec,
            } => self.start_find(generation, db, coll, spec).await,
            Command::NextBatch { generation } => self.next_batch(generation).await,
            Command::Explain { db, coll, spec } => self.explain(db, coll, spec).await,
            Command::Count {
                req_id,
                db,
                coll,
                filter,
            } => self.count(req_id, db, coll, filter).await,
            Command::Aggregate {
                generation,
                db,
                coll,
                pipeline,
                limit,
            } => self.aggregate(generation, db, coll, pipeline, limit).await,
            Command::InsertOne { db, coll, doc } => self.insert_one(db, coll, doc).await,
            Command::ReplaceOne { db, coll, id, doc } => self.replace_one(db, coll, id, doc).await,
            Command::DeleteOne { db, coll, id } => self.delete_one(db, coll, id).await,
            Command::DeleteMany { db, coll, filter } => self.delete_many(db, coll, filter).await,
            Command::UpdateMany {
                db,
                coll,
                filter,
                update,
            } => self.update_many(db, coll, filter, update).await,
            Command::ListIndexes { db, coll } => self.list_indexes(db, coll).await,
            Command::CreateIndex { db, coll, keys } => self.create_index(db, coll, keys).await,
            Command::DropIndex { db, coll, name } => self.drop_index(db, coll, name).await,
            Command::CreateCollection { db, name } => self.create_collection(db, name).await,
            Command::DropCollection { db, coll } => self.drop_collection(db, coll).await,
        }
    }

    fn coll(&self, db: &str, coll: &str) -> mongodb::Collection<Document> {
        self.client
            .as_ref()
            .expect("collection op before connect")
            .database(db)
            .collection::<Document>(coll)
    }

    fn connected(&self) -> bool {
        self.client.is_some()
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
            Err(e) => Self::emit_err(&self.events, format!("ping failed: {e}")).await,
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
            Err(e) => Self::emit_err(&self.events, format!("listDatabases: {e}")).await,
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
            Err(e) => Self::emit_err(&self.events, format!("listCollections({db}): {e}")).await,
        }
    }

    async fn start_find(&mut self, generation: u64, db: String, coll: String, spec: FindSpec) {
        if !self.connected() {
            return;
        }
        self.generation = generation;
        self.cursor = None;

        let collection = self.coll(&db, &coll);
        let total_estimate = if spec.filter.is_empty() && spec.limit.is_none() {
            collection.estimated_document_count().await.ok()
        } else {
            None
        };

        let mut find = collection
            .find(spec.filter)
            .batch_size(BATCH_SIZE as u32)
            .max_time(FIND_MAX_TIME);
        if let Some(p) = spec.projection {
            find = find.projection(p);
        }
        if let Some(s) = spec.sort {
            find = find.sort(s);
        }
        if let Some(l) = spec.limit {
            find = find.limit(l);
        }
        if let Some(s) = spec.skip {
            find = find.skip(s);
        }

        match find.await {
            Ok(cursor) => {
                self.cursor = Some(cursor);
                self.pull_batch(generation, total_estimate).await;
            }
            Err(e) => Self::emit_err(&self.events, format!("find: {e}")).await,
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
                    Self::emit_err(&self.events, format!("cursor: {e}")).await;
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

    async fn explain(&mut self, db: String, coll: String, spec: FindSpec) {
        let Some(client) = &self.client else { return };
        let mut find_body = doc! { "find": &coll, "filter": spec.filter };
        if let Some(p) = spec.projection {
            find_body.insert("projection", p);
        }
        if let Some(s) = spec.sort {
            find_body.insert("sort", s);
        }
        if let Some(l) = spec.limit {
            find_body.insert("limit", l);
        }
        if let Some(s) = spec.skip {
            find_body.insert("skip", s as i64);
        }
        let cmd = doc! { "explain": find_body, "verbosity": "executionStats" };
        match client.database(&db).run_command(cmd).await {
            Ok(result) => Self::emit(&self.events, CoreEvent::ExplainResult(result)).await,
            Err(e) => Self::emit_err(&self.events, format!("explain: {e}")).await,
        }
    }

    async fn count(&mut self, req_id: u64, db: String, coll: String, filter: Document) {
        if !self.connected() {
            return;
        }
        match self
            .coll(&db, &coll)
            .count_documents(filter)
            .max_time(COUNT_MAX_TIME)
            .await
        {
            Ok(n) => Self::emit(&self.events, CoreEvent::CountResult { req_id, n }).await,
            Err(e) => Self::emit_err(&self.events, format!("count: {e}")).await,
        }
    }

    async fn aggregate(
        &mut self,
        generation: u64,
        db: String,
        coll: String,
        pipeline: Vec<Document>,
        limit: usize,
    ) {
        if !self.connected() {
            return;
        }
        match self.coll(&db, &coll).aggregate(pipeline).await {
            Ok(mut cursor) => {
                let mut docs = Vec::new();
                loop {
                    match cursor.try_next().await {
                        Ok(Some(d)) => {
                            docs.push(d);
                            if docs.len() >= limit {
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            Self::emit_err(&self.events, format!("aggregate cursor: {e}")).await;
                            break;
                        }
                    }
                }
                Self::emit(&self.events, CoreEvent::AggBatch { generation, docs }).await;
            }
            Err(e) => Self::emit_err(&self.events, format!("aggregate: {e}")).await,
        }
    }

    // ---------- writes ----------

    async fn insert_one(&mut self, db: String, coll: String, doc: Document) {
        if !self.connected() {
            return;
        }
        match self.coll(&db, &coll).insert_one(doc).await {
            Ok(r) => {
                let id = display_id(&r.inserted_id);
                Self::emit(
                    &self.events,
                    CoreEvent::WriteDone {
                        namespace: format!("{db}.{coll}"),
                        summary: format!("inserted _id={id}"),
                        refresh: true,
                    },
                )
                .await;
            }
            Err(e) => Self::emit_err(&self.events, format!("insert: {e}")).await,
        }
    }

    async fn replace_one(&mut self, db: String, coll: String, id: Bson, doc: Document) {
        if !self.connected() {
            return;
        }
        match self
            .coll(&db, &coll)
            .replace_one(doc! { "_id": id.clone() }, doc)
            .await
        {
            Ok(r) => {
                Self::emit(
                    &self.events,
                    CoreEvent::WriteDone {
                        namespace: format!("{db}.{coll}"),
                        summary: format!(
                            "replaced _id={} (matched {}, modified {})",
                            display_id(&id),
                            r.matched_count,
                            r.modified_count
                        ),
                        refresh: true,
                    },
                )
                .await;
            }
            Err(e) => Self::emit_err(&self.events, format!("replace: {e}")).await,
        }
    }

    async fn delete_one(&mut self, db: String, coll: String, id: Bson) {
        if !self.connected() {
            return;
        }
        match self
            .coll(&db, &coll)
            .delete_one(doc! { "_id": id.clone() })
            .await
        {
            Ok(r) => {
                Self::emit(
                    &self.events,
                    CoreEvent::WriteDone {
                        namespace: format!("{db}.{coll}"),
                        summary: format!(
                            "deleted _id={} ({} doc)",
                            display_id(&id),
                            r.deleted_count
                        ),
                        refresh: true,
                    },
                )
                .await;
            }
            Err(e) => Self::emit_err(&self.events, format!("delete: {e}")).await,
        }
    }

    async fn delete_many(&mut self, db: String, coll: String, filter: Document) {
        if !self.connected() {
            return;
        }
        match self.coll(&db, &coll).delete_many(filter).await {
            Ok(r) => {
                Self::emit(
                    &self.events,
                    CoreEvent::WriteDone {
                        namespace: format!("{db}.{coll}"),
                        summary: format!("deleted {} docs by filter", r.deleted_count),
                        refresh: true,
                    },
                )
                .await;
            }
            Err(e) => Self::emit_err(&self.events, format!("deleteMany: {e}")).await,
        }
    }

    async fn update_many(&mut self, db: String, coll: String, filter: Document, update: Document) {
        if !self.connected() {
            return;
        }
        match self.coll(&db, &coll).update_many(filter, update).await {
            Ok(r) => {
                Self::emit(
                    &self.events,
                    CoreEvent::WriteDone {
                        namespace: format!("{db}.{coll}"),
                        summary: format!(
                            "updated {} docs (matched {})",
                            r.modified_count, r.matched_count
                        ),
                        refresh: true,
                    },
                )
                .await;
            }
            Err(e) => Self::emit_err(&self.events, format!("updateMany: {e}")).await,
        }
    }

    // ---------- indexes & collections ----------

    async fn list_indexes(&mut self, db: String, coll: String) {
        if !self.connected() {
            return;
        }
        match self.coll(&db, &coll).list_indexes().await {
            Ok(cursor) => {
                let models: Vec<IndexModel> = match cursor.try_collect().await {
                    Ok(m) => m,
                    Err(e) => {
                        Self::emit_err(&self.events, format!("listIndexes: {e}")).await;
                        return;
                    }
                };
                let indexes = models
                    .into_iter()
                    .map(|m| {
                        let opts = m.options.as_ref();
                        IndexInfo {
                            name: opts
                                .and_then(|o| o.name.clone())
                                .unwrap_or_else(|| "(unnamed)".into()),
                            keys: m.keys.clone(),
                            unique: opts.and_then(|o| o.unique).unwrap_or(false),
                        }
                    })
                    .collect();
                Self::emit(&self.events, CoreEvent::Indexes { db, coll, indexes }).await;
            }
            Err(e) => Self::emit_err(&self.events, format!("listIndexes: {e}")).await,
        }
    }

    async fn create_index(&mut self, db: String, coll: String, keys: Document) {
        if !self.connected() {
            return;
        }
        let model = IndexModel::builder().keys(keys).build();
        match self.coll(&db, &coll).create_index(model).await {
            Ok(r) => {
                Self::emit(
                    &self.events,
                    CoreEvent::WriteDone {
                        namespace: format!("{db}.{coll}"),
                        summary: format!("created index {}", r.index_name),
                        refresh: false,
                    },
                )
                .await;
                // Refresh the index list for any open indexes view.
                self.list_indexes(db, coll).await;
            }
            Err(e) => Self::emit_err(&self.events, format!("createIndex: {e}")).await,
        }
    }

    async fn drop_index(&mut self, db: String, coll: String, name: String) {
        if !self.connected() {
            return;
        }
        match self.coll(&db, &coll).drop_index(&name).await {
            Ok(()) => {
                Self::emit(
                    &self.events,
                    CoreEvent::WriteDone {
                        namespace: format!("{db}.{coll}"),
                        summary: format!("dropped index {name}"),
                        refresh: false,
                    },
                )
                .await;
                self.list_indexes(db, coll).await;
            }
            Err(e) => Self::emit_err(&self.events, format!("dropIndex: {e}")).await,
        }
    }

    async fn create_collection(&mut self, db: String, name: String) {
        let Some(client) = &self.client else { return };
        match client.database(&db).create_collection(&name).await {
            Ok(()) => {
                Self::emit(
                    &self.events,
                    CoreEvent::WriteDone {
                        namespace: db.clone(),
                        summary: format!("created collection {db}.{name}"),
                        refresh: false,
                    },
                )
                .await;
                self.list_collections(db).await;
            }
            Err(e) => Self::emit_err(&self.events, format!("createCollection: {e}")).await,
        }
    }

    async fn drop_collection(&mut self, db: String, coll: String) {
        if !self.connected() {
            return;
        }
        match self.coll(&db, &coll).drop().await {
            Ok(()) => {
                Self::emit(
                    &self.events,
                    CoreEvent::WriteDone {
                        namespace: db.clone(),
                        summary: format!("dropped collection {db}.{coll}"),
                        refresh: false,
                    },
                )
                .await;
                self.list_collections(db).await;
            }
            Err(e) => Self::emit_err(&self.events, format!("dropCollection: {e}")).await,
        }
    }
}

fn display_id(id: &Bson) -> String {
    match id {
        Bson::ObjectId(oid) => oid.to_string(),
        other => other.to_string(),
    }
}
