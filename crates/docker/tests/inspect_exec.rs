//! Integration tests for `Docker::inspect_exec`.
//!
//! Gated behind the `docker-tests` feature; needs a live daemon.

#![cfg(feature = "docker-tests")]

use std::process::Command;

use docker::{Docker, Error};

use docker::test_support::TestContainer;

const IMAGE: &str = "alpine:3.20";

#[tokio::test(flavor = "multi_thread")]
async fn inspect_exec_returns_running_for_live_exec() {
    let client = Docker::connect().await.expect("connect");
    let container = TestContainer::start(&client, IMAGE, &["sleep", "60"]).await;

    // Start a background exec so the container has an ExecID to inspect.
    // The docker crate doesn't expose exec create/start yet, so shell out.
    let status = Command::new("docker")
        .args(["exec", "-d", container.id(), "sleep", "30"])
        .status()
        .expect("docker exec");
    assert!(status.success());

    let details = client
        .inspect_container(container.id())
        .await
        .expect("inspect container");
    let exec_id = details
        .exec_ids
        .first()
        .expect("exec_ids should be populated after `docker exec -d`")
        .clone();

    let exec = client.inspect_exec(&exec_id).await.expect("inspect exec");
    assert_eq!(exec.id, exec_id);
    assert!(exec.running, "exec should still be running");
    assert!(exec.exit_code.is_none(), "no exit code while still running");
}

#[tokio::test(flavor = "multi_thread")]
async fn inspect_exec_returns_not_found_for_missing() {
    let client = Docker::connect().await.expect("connect");
    let err = client
        .inspect_exec("docker-crate-test-no-such-exec")
        .await
        .expect_err("missing exec should error");
    assert!(
        matches!(err, Error::NotFound { .. }),
        "expected NotFound, got {err:?}"
    );
}
