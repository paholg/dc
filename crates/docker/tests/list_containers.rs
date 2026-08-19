//! Integration tests for `Docker::list_containers`.
//!
//! Gated behind the `docker-tests` feature; needs a live daemon.

#![cfg(feature = "docker-tests")]

use docker::{ContainerStatus, Docker};

use docker::test_support::{ContainerCleanup, TEST_LABEL, TestContainer, unique_name};

const IMAGE: &str = "alpine:3.20";

#[tokio::test(flavor = "multi_thread")]
async fn lists_only_running_by_default() {
    let client = Docker::connect().await.expect("connect");
    let container = TestContainer::start(&client, IMAGE, &["sleep", "60"]).await;
    let (key, value) = TEST_LABEL.split_once('=').expect("TEST_LABEL is key=value");

    let summaries = client
        .list_containers()
        .with_label(key, value)
        .call()
        .await
        .expect("list");

    assert!(
        summaries.iter().any(|s| s.id == container.id()
            || s.names
                .iter()
                .any(|n| n.trim_start_matches('/') == container.id())),
        "newly-started container should be in the list",
    );
    assert!(
        summaries
            .iter()
            .all(|s| s.state == ContainerStatus::Running),
        "default list_containers should return only running entries",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn filter_label_narrows_results() {
    let client = Docker::connect().await.expect("connect");
    let _container = TestContainer::start(&client, IMAGE, &["sleep", "60"]).await;

    let summaries = client
        .list_containers()
        .with_label("no-such-key-zzzzz", "value")
        .call()
        .await
        .expect("list");

    assert!(
        summaries.is_empty(),
        "filtering on a label nothing has should return zero results, got {}",
        summaries.len(),
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn all_includes_stopped() {
    let client = Docker::connect().await.expect("connect");
    let container = TestContainer::start(&client, IMAGE, &["true"]).await;

    // Wait briefly for the container to exit on its own.
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let details = client
            .inspect_container(container.id())
            .await
            .expect("inspect");
        if details.state.status != ContainerStatus::Running {
            break;
        }
    }

    let (key, value) = TEST_LABEL.split_once('=').expect("TEST_LABEL is key=value");
    let summaries = client
        .list_containers()
        .all(true)
        .with_label(key, value)
        .call()
        .await
        .expect("list");

    assert!(
        summaries.iter().any(|s| s
            .names
            .iter()
            .any(|n| n.trim_start_matches('/') == container.id())),
        "with all=true, exited container should be in the list",
    );
}

/// The daemon's own `name` filter is a substring match, so a container whose
/// name merely starts with the one asked for must not come back.
#[tokio::test(flavor = "multi_thread")]
async fn filter_name_is_exact() {
    let client = Docker::connect().await.expect("connect");
    client.ensure_image(IMAGE).await.expect("ensure_image");
    let (key, value) = TEST_LABEL.split_once('=').expect("TEST_LABEL is key=value");

    let name = unique_name();
    let longer = format!("{name}-suffix");
    let mut created = Vec::new();
    for container in [&name, &longer] {
        let _cleanup = ContainerCleanup {
            client: client.clone(),
            name: container.clone(),
        };
        client
            .create_container(container)
            .image(IMAGE)
            .cmd(vec!["sleep".to_string(), "60".to_string()])
            .with_label(key, value)
            .call()
            .await
            .expect("create_container");
        created.push(_cleanup);
    }

    let summaries = client
        .list_containers()
        .all(true)
        .with_name(&name)
        .call()
        .await
        .expect("list");

    let names: Vec<String> = summaries
        .iter()
        .flat_map(|s| s.names.iter())
        .map(|n| n.trim_start_matches('/').to_string())
        .collect();
    assert_eq!(names, vec![name], "got {summaries:#?}");
}
