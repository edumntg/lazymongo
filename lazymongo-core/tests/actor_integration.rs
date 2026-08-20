//! Integration test for the Mongo actor against a real server.
//! Skipped unless LAZYMONGO_TEST_URI is set, e.g.:
//!   LAZYMONGO_TEST_URI=mongodb://localhost:27099 cargo test -p lazymongo-core

use std::time::Duration;

use lazymongo_core::actor;
use lazymongo_core::query::parse_filter;
use lazymongo_core::types::{Command, CoreEvent, BATCH_SIZE};
use tokio::time::timeout;

/// Receive the next non-Ping event (health pings can arrive at any time).
async fn recv(rx: &mut tokio::sync::mpsc::Receiver<CoreEvent>) -> CoreEvent {
    loop {
        let ev = timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("timed out waiting for core event")
            .expect("actor channel closed");
        if !matches!(ev, CoreEvent::Ping { .. }) {
            return ev;
        }
    }
}

#[tokio::test]
async fn browse_and_query_flow() {
    let Ok(uri) = std::env::var("LAZYMONGO_TEST_URI") else {
        eprintln!("LAZYMONGO_TEST_URI not set; skipping integration test");
        return;
    };

    let (cmd, mut evt) = actor::spawn();
    cmd.send(Command::Connect { uri }).await.unwrap();
    match recv(&mut evt).await {
        CoreEvent::Connected { server_version, .. } => {
            assert!(!server_version.is_empty())
        }
        other => panic!("expected Connected, got {other:?}"),
    }

    // Databases include the seeded app_db.
    cmd.send(Command::ListDatabases).await.unwrap();
    let dbs = match recv(&mut evt).await {
        CoreEvent::Databases(dbs) => dbs,
        other => panic!("expected Databases, got {other:?}"),
    };
    assert!(
        dbs.iter().any(|d| d.name == "app_db"),
        "app_db missing: {dbs:?}"
    );

    // Collections of app_db.
    cmd.send(Command::ListCollections {
        db: "app_db".into(),
    })
    .await
    .unwrap();
    let colls = match recv(&mut evt).await {
        CoreEvent::Collections { db, colls } => {
            assert_eq!(db, "app_db");
            colls
        }
        other => panic!("expected Collections, got {other:?}"),
    };
    let names: Vec<&str> = colls.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"users") && names.contains(&"orders"),
        "{names:?}"
    );

    // Unfiltered find: first batch + estimate, then page to exhaustion.
    cmd.send(Command::StartFind {
        generation: 1,
        db: "app_db".into(),
        coll: "users".into(),
        filter: parse_filter("").unwrap(),
    })
    .await
    .unwrap();

    let mut total = 0usize;
    let mut batches = 0usize;
    loop {
        match recv(&mut evt).await {
            CoreEvent::Batch {
                generation,
                docs,
                exhausted,
                total_estimate,
            } => {
                assert_eq!(generation, 1);
                if batches == 0 {
                    assert_eq!(total_estimate, Some(500));
                    assert_eq!(docs.len(), BATCH_SIZE);
                }
                total += docs.len();
                batches += 1;
                if exhausted {
                    break;
                }
                cmd.send(Command::NextBatch { generation: 1 })
                    .await
                    .unwrap();
            }
            CoreEvent::Ping { .. } => continue,
            other => panic!("expected Batch, got {other:?}"),
        }
    }
    assert_eq!(total, 500);
    assert!(batches >= 500 / BATCH_SIZE);

    // Stale NextBatch (wrong generation) must be silently ignored.
    cmd.send(Command::NextBatch { generation: 99 })
        .await
        .unwrap();

    // Filtered find with relaxed mongosh syntax.
    cmd.send(Command::StartFind {
        generation: 2,
        db: "app_db".into(),
        coll: "users".into(),
        filter: parse_filter("{ status: 'active', age: { $gt: 40 } }").unwrap(),
    })
    .await
    .unwrap();
    let mut filtered = 0usize;
    loop {
        match recv(&mut evt).await {
            CoreEvent::Batch {
                generation,
                docs,
                exhausted,
                total_estimate,
            } => {
                assert_eq!(generation, 2, "stale batch leaked through");
                assert_eq!(total_estimate, None, "filtered find has no estimate");
                for d in &docs {
                    assert_eq!(d.get_str("status").unwrap(), "active");
                    assert!(d.get_i32("age").unwrap() > 40);
                }
                filtered += docs.len();
                if exhausted {
                    break;
                }
                cmd.send(Command::NextBatch { generation: 2 })
                    .await
                    .unwrap();
            }
            CoreEvent::Ping { .. } => continue,
            other => panic!("expected Batch, got {other:?}"),
        }
    }
    assert!(
        filtered > 0 && filtered < 500,
        "unexpected filtered count {filtered}"
    );
}
