//! Integration tests for the volume API.
//!
//! Gated behind the `docker-tests` feature; needs a live daemon.

#![cfg(feature = "docker-tests")]

use docker::{Docker, Error};

use docker::test_support::{TEST_LABEL, VolumeCleanup, unique_name};

#[tokio::test(flavor = "multi_thread")]
async fn create_then_list_then_remove() {
    let client = Docker::connect().await.expect("connect");
    let name = unique_name();
    let _cleanup = VolumeCleanup {
        client: client.clone(),
        name: name.clone(),
    };
    let (key, value) = TEST_LABEL.split_once('=').expect("TEST_LABEL is key=value");

    let created = client
        .create_volume(&name)
        .with_label(key, value)
        .call()
        .await
        .expect("create");
    assert_eq!(created.name, name);
    assert_eq!(created.labels.get(key).map(String::as_str), Some(value));

    let listed = client
        .list_volumes()
        .with_label(key, value)
        .call()
        .await
        .expect("list");
    assert!(
        listed.iter().any(|x| x.name == name),
        "created volume should be in the labelled list"
    );

    client.remove_volume(&name).call().await.expect("remove");
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_missing_volume_returns_not_found() {
    let client = Docker::connect().await.expect("connect");
    let err = client
        .remove_volume("docker-crate-test-no-such-vol")
        .call()
        .await
        .expect_err("missing volume should error");
    assert!(
        matches!(err, Error::NotFound { .. }),
        "expected NotFound, got {err:?}",
    );
}
