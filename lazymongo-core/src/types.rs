use mongodb::bson::Document;

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

/// Commands the UI sends to the Mongo actor.
#[derive(Debug)]
pub enum Command {
    Connect {
        uri: String,
    },
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
        filter: Document,
    },
    /// Pull the next batch from the live cursor, if generation still matches.
    NextBatch {
        generation: u64,
    },
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
    /// Periodic health ping.
    Ping {
        ms: u64,
    },
    Error(String),
}
