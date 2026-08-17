//! Integration tests for `Docker::events`.
//!
//! Gated behind the `docker-tests` feature; needs a live daemon.

#![cfg(feature = "docker-tests")]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use docker::test_support::{TEST_LABEL, TestContainer};
use docker::{Docker, EventMessage};
use futures_util::{Stream, StreamExt};

const IMAGE: &str = "alpine:3.20";

/// Read events until one is about `name`, or give up after 30s.
async fn wait_for(stream: impl Stream<Item = docker::Result<EventMessage>>, name: &str) -> bool {
    tokio::pin!(stream);
    let found = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(item) = stream.next().await {
            let event = item.expect("event");
            if event
                .actor
                .attributes
                .get("name")
                .is_some_and(|n| n == name)
            {
                return true;
            }
        }
        false
    })
    .await;
    found.unwrap_or(false)
}

/// The whole point of `since`: subscribing after the fact still sees what was
/// missed. Without it, the `start` below would be gone by the time we connect.
#[tokio::test(flavor = "multi_thread")]
async fn since_replays_events_from_before_subscribing() {
    let client = Docker::connect().await.expect("connect");
    // A margin for any clock skew between us and the daemon.
    let before = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs()
        - 10;

    let container = TestContainer::start(&client, IMAGE, &["sleep", "60"]).await;

    let (key, value) = TEST_LABEL.split_once('=').expect("TEST_LABEL is key=value");
    let stream = client
        .events()
        .with_type("container")
        .with_event("start")
        .with_label(key, value)
        .since(before.to_string())
        .call()
        .await
        .expect("events");

    assert!(
        wait_for(stream, container.id()).await,
        "start event from before the subscription should be replayed",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn live_events_arrive() {
    let client = Docker::connect().await.expect("connect");
    let (key, value) = TEST_LABEL.split_once('=').expect("TEST_LABEL is key=value");
    let stream = client
        .events()
        .with_type("container")
        .with_event("start")
        .with_label(key, value)
        .call()
        .await
        .expect("events");

    let container = TestContainer::start(&client, IMAGE, &["sleep", "60"]).await;

    assert!(
        wait_for(stream, container.id()).await,
        "start event should arrive on a live subscription",
    );
}
