//! Integration test for the Mongo actor against a real server.
//! Skipped unless LAZYMONGO_TEST_URI is set, e.g.:
//!   LAZYMONGO_TEST_URI=mongodb://localhost:27099 cargo test -p lazymongo-core

use std::time::Duration;

use lazymongo_core::actor;
use lazymongo_core::bson::{doc, Bson};
use lazymongo_core::query::{parse_filter, parse_pipeline};
use lazymongo_core::types::{Command, CoreEvent, FindSpec, BATCH_SIZE};
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

fn test_uri() -> Option<String> {
    std::env::var("LAZYMONGO_TEST_URI").ok()
}

/// Seed app_db exactly once per test run (tests run in parallel), so the
/// suite is self-contained: any empty MongoDB works, including CI service
/// containers.
static SEEDED: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

async fn ensure_seeded(uri: &str) {
    SEEDED
        .get_or_init(|| async {
            let client = mongodb::Client::with_uri_str(uri)
                .await
                .expect("seed: connect");
            let db = client.database("app_db");
            db.drop().await.expect("seed: drop app_db");
            let users: Vec<lazymongo_core::bson::Document> = (0..500)
                .map(|i: i32| {
                    let tags = ["a", "b", "c"][..(i as usize % 3) + 1].to_vec();
                    doc! {
                        "name": format!("User {i}"),
                        "email": format!("user{i}@example.com"),
                        "age": 18 + (i % 50),
                        "status": if i % 3 == 0 { "active" } else { "inactive" },
                        "address": {
                            "city": format!("City {}", i % 10),
                            "geo": { "lat": f64::from(i) * 0.1, "lng": f64::from(i) * -0.2 },
                        },
                        "tags": tags,
                    }
                })
                .collect();
            db.collection("users")
                .insert_many(users)
                .await
                .expect("seed: users");
            let orders: Vec<lazymongo_core::bson::Document> = (0..75)
                .map(|i: i32| {
                    doc! {
                        "total": f64::from(i) * 9.99,
                        "items": [{ "sku": format!("X{i}"), "qty": i % 5 }],
                        "paid": i % 2 == 0,
                    }
                })
                .collect();
            db.collection("orders")
                .insert_many(orders)
                .await
                .expect("seed: orders");
        })
        .await;
}

fn spec(filter: &str) -> FindSpec {
    FindSpec {
        filter: parse_filter(filter).unwrap(),
        ..Default::default()
    }
}

#[tokio::test]
async fn browse_and_query_flow() {
    let Some(uri) = test_uri() else {
        eprintln!("LAZYMONGO_TEST_URI not set; skipping integration test");
        return;
    };
    ensure_seeded(&uri).await;

    let (cmd, mut evt, _cancel) = actor::spawn(false);
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
        spec: spec(""),
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
        spec: spec("{ status: 'active', age: { $gt: 40 } }"),
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
            other => panic!("expected Batch, got {other:?}"),
        }
    }
    assert!(
        filtered > 0 && filtered < 500,
        "unexpected filtered count {filtered}"
    );

    // Full spec: projection + sort + limit + skip.
    cmd.send(Command::StartFind {
        generation: 3,
        db: "app_db".into(),
        coll: "users".into(),
        spec: FindSpec {
            filter: doc! {},
            projection: Some(doc! { "name": 1, "age": 1 }),
            sort: Some(doc! { "age": -1 }),
            limit: Some(5),
            skip: Some(2),
        },
    })
    .await
    .unwrap();
    match recv(&mut evt).await {
        CoreEvent::Batch {
            generation: 3,
            docs,
            exhausted,
            ..
        } => {
            assert_eq!(docs.len(), 5);
            assert!(exhausted);
            let ages: Vec<i32> = docs.iter().map(|d| d.get_i32("age").unwrap()).collect();
            assert!(
                ages.windows(2).all(|w| w[0] >= w[1]),
                "not sorted desc: {ages:?}"
            );
            assert!(docs[0].get("status").is_none(), "projection not applied");
        }
        other => panic!("expected Batch gen 3, got {other:?}"),
    }

    // Explain returns a query plan.
    cmd.send(Command::Explain {
        db: "app_db".into(),
        coll: "users".into(),
        spec: spec("{ age: { $gt: 30 } }"),
    })
    .await
    .unwrap();
    match recv(&mut evt).await {
        CoreEvent::ExplainResult(plan) => {
            assert!(plan.get_document("queryPlanner").is_ok(), "{plan:?}");
        }
        other => panic!("expected ExplainResult, got {other:?}"),
    }

    // Count for dry runs.
    cmd.send(Command::Count {
        req_id: 7,
        db: "app_db".into(),
        coll: "users".into(),
        filter: parse_filter("{ status: 'active' }").unwrap(),
    })
    .await
    .unwrap();
    match recv(&mut evt).await {
        CoreEvent::CountResult { req_id: 7, n } => assert!(n > 0 && n < 500),
        other => panic!("expected CountResult, got {other:?}"),
    }

    // Aggregation preview, capped at limit.
    cmd.send(Command::Aggregate {
        generation: 4,
        db: "app_db".into(),
        coll: "users".into(),
        pipeline: parse_pipeline(
            "[{ $match: { status: 'active' } }, { $group: { _id: '$address.city', n: { $sum: 1 } } }, { $sort: { n: -1 } }]",
        )
        .unwrap(),
        limit: 5,
    })
    .await
    .unwrap();
    match recv(&mut evt).await {
        CoreEvent::AggBatch {
            generation: 4,
            docs,
        } => {
            assert!(!docs.is_empty() && docs.len() <= 5, "{docs:?}");
            assert!(docs[0].get("n").is_some());
        }
        other => panic!("expected AggBatch, got {other:?}"),
    }
}

#[tokio::test]
async fn write_operations_roundtrip() {
    let Some(uri) = test_uri() else {
        return;
    };
    ensure_seeded(&uri).await;
    let (cmd, mut evt, _cancel) = actor::spawn(false);
    cmd.send(Command::Connect { uri }).await.unwrap();
    assert!(matches!(recv(&mut evt).await, CoreEvent::Connected { .. }));

    let db = "lazymongo_write_test".to_string();
    let coll = "scratch".to_string();

    // Start clean.
    cmd.send(Command::DropCollection {
        db: db.clone(),
        coll: coll.clone(),
    })
    .await
    .unwrap();
    loop {
        match recv(&mut evt).await {
            CoreEvent::WriteDone { .. } => {}
            CoreEvent::Collections { .. } => break,
            other => panic!("unexpected: {other:?}"),
        }
    }

    // Insert.
    cmd.send(Command::InsertOne {
        db: db.clone(),
        coll: coll.clone(),
        doc: doc! { "_id": 1, "kind": "test", "n": 1 },
    })
    .await
    .unwrap();
    match recv(&mut evt).await {
        CoreEvent::WriteDone {
            summary, refresh, ..
        } => {
            assert!(summary.contains("inserted"), "{summary}");
            assert!(refresh);
        }
        other => panic!("expected WriteDone, got {other:?}"),
    }

    // Replace by _id.
    cmd.send(Command::ReplaceOne {
        db: db.clone(),
        coll: coll.clone(),
        id: Bson::Int32(1),
        doc: doc! { "kind": "test", "n": 2, "edited": true },
    })
    .await
    .unwrap();
    match recv(&mut evt).await {
        CoreEvent::WriteDone { summary, .. } => {
            assert!(summary.contains("matched 1"), "{summary}")
        }
        other => panic!("expected WriteDone, got {other:?}"),
    }

    // UpdateMany.
    cmd.send(Command::UpdateMany {
        db: db.clone(),
        coll: coll.clone(),
        filter: doc! { "kind": "test" },
        update: doc! { "$set": { "bulk": true } },
    })
    .await
    .unwrap();
    match recv(&mut evt).await {
        CoreEvent::WriteDone { summary, .. } => {
            assert!(summary.contains("updated 1"), "{summary}")
        }
        other => panic!("expected WriteDone, got {other:?}"),
    }

    // Index create / list / drop.
    cmd.send(Command::CreateIndex {
        db: db.clone(),
        coll: coll.clone(),
        keys: doc! { "n": 1 },
    })
    .await
    .unwrap();
    assert!(matches!(recv(&mut evt).await, CoreEvent::WriteDone { .. }));
    let created = match recv(&mut evt).await {
        CoreEvent::Indexes { indexes, .. } => {
            assert!(indexes.iter().any(|i| i.name == "n_1"), "{indexes:?}");
            "n_1".to_string()
        }
        other => panic!("expected Indexes, got {other:?}"),
    };
    cmd.send(Command::DropIndex {
        db: db.clone(),
        coll: coll.clone(),
        name: created,
    })
    .await
    .unwrap();
    assert!(matches!(recv(&mut evt).await, CoreEvent::WriteDone { .. }));
    match recv(&mut evt).await {
        CoreEvent::Indexes { indexes, .. } => {
            assert!(!indexes.iter().any(|i| i.name == "n_1"))
        }
        other => panic!("expected Indexes, got {other:?}"),
    }

    // DeleteOne then DeleteMany.
    cmd.send(Command::DeleteOne {
        db: db.clone(),
        coll: coll.clone(),
        id: Bson::Int32(1),
    })
    .await
    .unwrap();
    match recv(&mut evt).await {
        CoreEvent::WriteDone { summary, .. } => assert!(summary.contains("1 doc"), "{summary}"),
        other => panic!("expected WriteDone, got {other:?}"),
    }
    cmd.send(Command::DeleteMany {
        db: db.clone(),
        coll: coll.clone(),
        filter: doc! {},
    })
    .await
    .unwrap();
    assert!(matches!(recv(&mut evt).await, CoreEvent::WriteDone { .. }));

    // Clean up the scratch db's collection.
    cmd.send(Command::DropCollection { db, coll })
        .await
        .unwrap();
}

#[tokio::test]
async fn read_only_mode_rejects_writes() {
    let Some(uri) = test_uri() else {
        return;
    };
    ensure_seeded(&uri).await;
    let (cmd, mut evt, _cancel) = actor::spawn(true);
    cmd.send(Command::Connect { uri }).await.unwrap();
    assert!(matches!(recv(&mut evt).await, CoreEvent::Connected { .. }));

    cmd.send(Command::InsertOne {
        db: "app_db".into(),
        coll: "users".into(),
        doc: doc! { "hax": true },
    })
    .await
    .unwrap();
    match recv(&mut evt).await {
        CoreEvent::Error(e) => assert!(e.contains("read-only"), "{e}"),
        other => panic!("expected read-only Error, got {other:?}"),
    }

    // Reads still work.
    cmd.send(Command::StartFind {
        generation: 1,
        db: "app_db".into(),
        coll: "users".into(),
        spec: FindSpec {
            limit: Some(1),
            ..Default::default()
        },
    })
    .await
    .unwrap();
    match recv(&mut evt).await {
        CoreEvent::Batch { docs, .. } => assert_eq!(docs.len(), 1),
        other => panic!("expected Batch, got {other:?}"),
    }
}

#[tokio::test]
async fn cancellation_and_write_stage_guard() {
    let Some(uri) = test_uri() else {
        return;
    };
    ensure_seeded(&uri).await;
    let (cmd, mut evt, cancel) = actor::spawn(false);
    cmd.send(Command::Connect { uri }).await.unwrap();
    assert!(matches!(recv(&mut evt).await, CoreEvent::Connected { .. }));

    // Cancel generation 5 up-front: the find must abort, not return a batch.
    cancel.send(5).unwrap();
    cmd.send(Command::StartFind {
        generation: 5,
        db: "app_db".into(),
        coll: "users".into(),
        spec: spec(""),
    })
    .await
    .unwrap();
    match recv(&mut evt).await {
        CoreEvent::Cancelled { generation: 5 } => {}
        other => panic!("expected Cancelled, got {other:?}"),
    }

    // A later generation is unaffected.
    cmd.send(Command::StartFind {
        generation: 6,
        db: "app_db".into(),
        coll: "users".into(),
        spec: FindSpec {
            limit: Some(1),
            ..Default::default()
        },
    })
    .await
    .unwrap();
    match recv(&mut evt).await {
        CoreEvent::Batch {
            generation: 6,
            docs,
            ..
        } => assert_eq!(docs.len(), 1),
        other => panic!("expected Batch gen 6, got {other:?}"),
    }

    // The aggregation preview must refuse $out / $merge write stages.
    cmd.send(Command::Aggregate {
        generation: 7,
        db: "app_db".into(),
        coll: "users".into(),
        pipeline: parse_pipeline("[{ $match: {} }, { $out: 'hax' }]").unwrap(),
        limit: 10,
    })
    .await
    .unwrap();
    match recv(&mut evt).await {
        CoreEvent::Error(e) => assert!(e.contains("$out"), "{e}"),
        other => panic!("expected $out rejection, got {other:?}"),
    }
}
