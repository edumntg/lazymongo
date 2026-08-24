//! Regression: a new StartFind while an unexhausted cursor is alive must
//! drop the old cursor and serve the new query.
use lazymongo_core::actor;
use lazymongo_core::types::{Command, CoreEvent, FindSpec};
use std::time::Duration;
use tokio::time::timeout;

#[tokio::test]
async fn new_find_with_live_cursor() {
    // Skip (like the rest of the integration suite) when no test server
    // is configured — e.g. the macOS/Windows CI builders.
    let Ok(uri) = std::env::var("LAZYMONGO_TEST_URI") else {
        return;
    };
    let (cmd, mut evt, _cancel) = actor::spawn(false);
    cmd.send(Command::Connect { uri }).await.unwrap();
    loop {
        if let CoreEvent::Connected { .. } = timeout(Duration::from_secs(10), evt.recv())
            .await
            .unwrap()
            .unwrap()
        {
            break;
        }
    }
    // gen 1: unlimited find, take only the first batch (cursor stays live)
    cmd.send(Command::StartFind {
        generation: 1,
        db: "app_db".into(),
        coll: "users".into(),
        spec: FindSpec::default(),
    })
    .await
    .unwrap();
    loop {
        if let CoreEvent::Batch {
            generation: 1,
            exhausted,
            ..
        } = timeout(Duration::from_secs(10), evt.recv())
            .await
            .unwrap()
            .unwrap()
        {
            assert!(!exhausted);
            break;
        }
    }
    // gen 2: new find with limit while gen-1 cursor is still open
    cmd.send(Command::StartFind {
        generation: 2,
        db: "app_db".into(),
        coll: "users".into(),
        spec: FindSpec {
            limit: Some(7),
            ..Default::default()
        },
    })
    .await
    .unwrap();
    loop {
        match timeout(Duration::from_secs(10), evt.recv())
            .await
            .expect("TIMED OUT waiting for gen-2 batch")
            .unwrap()
        {
            CoreEvent::Batch {
                generation: 2,
                docs,
                exhausted,
                ..
            } => {
                assert_eq!(docs.len(), 7);
                assert!(exhausted);
                break;
            }
            other => eprintln!("[skip] {other:?}"),
        }
    }
}
