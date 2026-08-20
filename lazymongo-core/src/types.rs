use mongodb::bson::{Bson, Document};

/// Number of documents fetched per batch.
pub const BATCH_SIZE: usize = 50;

#[derive(Debug, Clone)]
pub struct DatabaseInfo {
    pub name: String,
    pub size_on_disk: u64,
}

#[derive(Debug, Clone)]
pub struct CollectionInfo {
    pub name: String,
    /// Filled in asynchronously; `None` while still loading.
    pub estimated_count: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct IndexInfo {
    pub name: String,
    pub keys: Document,
    pub unique: bool,
}

/// Full find specification (FR-12).
#[derive(Debug, Clone, Default)]
pub struct FindSpec {
    pub filter: Document,
    pub projection: Option<Document>,
    pub sort: Option<Document>,
    pub limit: Option<i64>,
    pub skip: Option<u64>,
}

/// Commands the UI sends to the Mongo actor.
#[derive(Debug)]
pub enum Command {
    Connect {
        uri: String,
    },
    /// Toggle read-only enforcement (used by the connection picker).
    SetReadOnly(bool),
    ListDatabases,
    ListCollections {
        db: String,
    },
    /// Open a fresh cursor for this query. `generation` tags every batch the
    /// actor sends back so the UI can drop results from stale queries.
    StartFind {
        generation: u64,
        db: String,
        coll: String,
        spec: FindSpec,
    },
    /// Pull the next batch from the live cursor, if generation still matches.
    NextBatch {
        generation: u64,
    },
    /// Explain the given find with executionStats verbosity (FR-15).
    Explain {
        db: String,
        coll: String,
        spec: FindSpec,
    },
    /// countDocuments, used for update/delete dry runs (FR-29/FR-30).
    Count {
        req_id: u64,
        db: String,
        coll: String,
        filter: Document,
    },
    /// One-shot aggregation preview, capped at `limit` result docs (FR-18).
    Aggregate {
        generation: u64,
        db: String,
        coll: String,
        pipeline: Vec<Document>,
        limit: usize,
    },
    // ---- write operations (rejected when the actor is read-only) ----
    InsertOne {
        db: String,
        coll: String,
        doc: Document,
    },
    ReplaceOne {
        db: String,
        coll: String,
        id: Bson,
        doc: Document,
    },
    DeleteOne {
        db: String,
        coll: String,
        id: Bson,
    },
    DeleteMany {
        db: String,
        coll: String,
        filter: Document,
    },
    UpdateMany {
        db: String,
        coll: String,
        filter: Document,
        update: Document,
    },
    ListIndexes {
        db: String,
        coll: String,
    },
    CreateIndex {
        db: String,
        coll: String,
        keys: Document,
    },
    DropIndex {
        db: String,
        coll: String,
        name: String,
    },
    CreateCollection {
        db: String,
        name: String,
    },
    DropCollection {
        db: String,
        coll: String,
    },
}

impl Command {
    /// True for commands that mutate data (blocked in read-only mode).
    pub fn is_write(&self) -> bool {
        if let Command::Aggregate { pipeline, .. } = self {
            return pipeline_writes(pipeline);
        }
        matches!(
            self,
            Command::InsertOne { .. }
                | Command::ReplaceOne { .. }
                | Command::DeleteOne { .. }
                | Command::DeleteMany { .. }
                | Command::UpdateMany { .. }
                | Command::CreateIndex { .. }
                | Command::DropIndex { .. }
                | Command::CreateCollection { .. }
                | Command::DropCollection { .. }
        )
    }
}

/// Events the Mongo actor sends back to the UI.
#[derive(Debug)]
pub enum CoreEvent {
    Connected {
        server_version: String,
        ping_ms: u64,
    },
    ConnectFailed(String),
    Databases(Vec<DatabaseInfo>),
    Collections {
        db: String,
        colls: Vec<CollectionInfo>,
    },
    /// One page of query results.
    Batch {
        generation: u64,
        docs: Vec<Document>,
        /// True when the cursor is exhausted (no more batches).
        exhausted: bool,
        /// Estimated total (only for unfiltered finds), sent with batch 0.
        total_estimate: Option<u64>,
    },
    /// Aggregation preview results (one shot).
    AggBatch {
        generation: u64,
        docs: Vec<Document>,
    },
    ExplainResult(Document),
    CountResult {
        req_id: u64,
        n: u64,
    },
    /// A write operation completed. `refresh` hints the UI to re-run the
    /// active find; `namespace` is "db.coll" (or "db" for db-level ops).
    WriteDone {
        namespace: String,
        summary: String,
        refresh: bool,
    },
    Indexes {
        db: String,
        coll: String,
        indexes: Vec<IndexInfo>,
    },
    /// A find/aggregate was cancelled by the user before completing.
    Cancelled {
        generation: u64,
    },
    /// Periodic health ping.
    Ping {
        ms: u64,
    },
    Error(String),
}

/// True when an aggregation pipeline contains write stages ($out / $merge).
pub fn pipeline_writes(pipeline: &[Document]) -> bool {
    pipeline
        .iter()
        .any(|stage| stage.contains_key("$out") || stage.contains_key("$merge"))
}
