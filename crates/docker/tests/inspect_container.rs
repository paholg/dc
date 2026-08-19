//! Integration tests for `Docker::inspect_container`.
//!
//! Gated behind the `docker-tests` feature; build with
//! `cargo nextest run -p docker --features docker-tests` (or via the
//! workspace-level `cargo nextest run --features docker/docker-tests`).
//! These talk to a real docker (or podman-with-docker-compat) daemon.
//!
//! Each test uses [`docker::test_support::TestContainer`], an RAII guard
//! that removes its container on Drop (including during panic). CI also
//! sweeps all containers labelled `devconcurrent-docker-crate-test=true`
//! before/after each run as a backstop.

#![cfg(feature = "docker-tests")]

use docker::test_support::{TEST_LABEL, TestContainer};
use docker::{ContainerStatus, Docker, Error};

const IMAGE: &str = "alpine:3.20";

#[tokio::test(flavor = "multi_thread")]
async fn inspect_returns_running_container_details() {
    let client = Docker::connect().await.expect("connect");
    let container = TestContainer::start(&client, IMAGE, &["sleep", "60"]).await;

    let details = client.inspect_container(container.id()).await.unwrap();

    assert_eq!(details.state.status, ContainerStatus::Running);
    assert!(details.state.running);
    assert_eq!(details.state.exit_code, 0);
    assert!(!details.id.is_empty(), "container id should be populated");
    assert!(
        details.config.parsed_env().contains_key("PATH"),
        "alpine sets PATH; parsed_env must find it",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn inspect_returns_not_found_for_missing_container() {
    let client = Docker::connect().await.expect("connect");

    let err = client
        .inspect_container("docker-crate-test-does-not-exist")
        .await
        .expect_err("missing container should error");

    let Error::NotFound { message } = &err else {
        panic!("expected Error::NotFound, got {err:?}");
    };
    assert!(
        message.contains("docker-crate-test-does-not-exist"),
        "the daemon's explanation should survive: {message:?}",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn inspect_surfaces_container_labels() {
    let client = Docker::connect().await.expect("connect");
    let container = TestContainer::start(&client, IMAGE, &["sleep", "60"]).await;

    let details = client
        .inspect_container(container.id())
        .await
        .expect("inspect");

    // Our test guard always sets `TEST_LABEL`; if we can read it back through
    // inspect, label propagation is working end-to-end.
    let (key, value) = TEST_LABEL.split_once('=').expect("TEST_LABEL is key=value");
    assert_eq!(
        details.config.labels.get(key).map(String::as_str),
        Some(value),
    );
}
