//! Integration tests for `Docker::remove_container`.
//!
//! Gated behind the `docker-tests` feature; needs a live daemon.

#![cfg(feature = "docker-tests")]

use docker::{Docker, Error};

use docker::test_support::TestContainer;

const IMAGE: &str = "alpine:3.20";

#[tokio::test(flavor = "multi_thread")]
async fn remove_force_kills_a_running_container() {
    let client = Docker::connect().await.expect("connect");
    let container = TestContainer::start(&client, IMAGE, &["sleep", "60"]).await;

    client
        .remove_container(container.id())
        .force(true)
        .call()
        .await
        .expect("remove");

    // After removal, inspect should return NotFound.
    let err = client
        .inspect_container(container.id())
        .await
        .expect_err("container should be gone");
    assert!(
        matches!(err, Error::NotFound { .. }),
        "expected NotFound, got {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_missing_container_returns_not_found() {
    let client = Docker::connect().await.expect("connect");
    let err = client
        .remove_container("docker-crate-test-no-such")
        .call()
        .await
        .expect_err("missing container should error");
    assert!(
        matches!(err, Error::NotFound { .. }),
        "expected NotFound, got {err:?}"
    );
}
