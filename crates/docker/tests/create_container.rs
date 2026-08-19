//! Integration tests for `Docker::create_container` and `start_container`.
//!
//! Gated behind the `docker-tests` feature; needs a live daemon.

#![cfg(feature = "docker-tests")]

use std::net::{IpAddr, Ipv4Addr};

use docker::{ContainerStatus, Docker};

use docker::test_support::{ContainerCleanup, TEST_LABEL, VolumeCleanup, unique_name};

const ALPINE: &str = "docker.io/library/alpine:3.20";

async fn pull_alpine(client: &Docker) {
    client.ensure_image(ALPINE).await.expect("ensure_image");
}

#[tokio::test(flavor = "multi_thread")]
async fn create_then_start_runs_the_container() {
    let client = Docker::connect().await.expect("connect");
    pull_alpine(&client).await;

    let name = unique_name();
    let _cleanup = ContainerCleanup {
        client: client.clone(),
        name: name.clone(),
    };
    let (test_key, test_value) = TEST_LABEL.split_once('=').expect("TEST_LABEL is key=value");

    let id = client
        .create_container(&name)
        .image(ALPINE)
        .entrypoint(vec!["sh".to_string()])
        .cmd(vec!["-c".to_string(), "sleep 30".to_string()])
        .with_label(test_key, test_value)
        .with_label("dc-test-extra", "yes")
        .call()
        .await
        .expect("create_container");

    client.start_container(&id).await.expect("start_container");

    let details = client
        .inspect_container(&id)
        .await
        .expect("inspect_container");
    assert_eq!(details.state.status, ContainerStatus::Running);
    assert_eq!(
        details
            .config
            .labels
            .get("dc-test-extra")
            .map(String::as_str),
        Some("yes"),
        "custom label should be set on the new container",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn create_with_port_binding_publishes_on_host() {
    let client = Docker::connect().await.expect("connect");
    pull_alpine(&client).await;

    let name = unique_name();
    let _cleanup = ContainerCleanup {
        client: client.clone(),
        name: name.clone(),
    };
    let (test_key, test_value) = TEST_LABEL.split_once('=').expect("TEST_LABEL is key=value");

    let id = client
        .create_container(&name)
        .image(ALPINE)
        .entrypoint(vec!["sh".to_string()])
        .cmd(vec!["-c".to_string(), "sleep 30".to_string()])
        .with_label(test_key, test_value)
        .with_tcp_port_binding(80, IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
        .call()
        .await
        .expect("create_container");
    client.start_container(&id).await.expect("start_container");

    let summary = client
        .list_containers()
        .all(true)
        .with_id(&id)
        .call()
        .await
        .expect("list_containers");
    let entry = summary
        .iter()
        .find(|c| c.id == id)
        .expect("container should appear in list");
    let mapped = entry
        .ports
        .iter()
        .find(|p| p.private_port == 80 && p.public_port.is_some());
    assert!(
        mapped.is_some(),
        "port 80 should be mapped to a host port; got {:?}",
        entry.ports,
    );
}

/// The daemon answers 304 for a container that is already running; that is
/// "nothing to do", not a failure.
#[tokio::test(flavor = "multi_thread")]
async fn starting_a_running_container_is_not_an_error() {
    let client = Docker::connect().await.expect("connect");
    pull_alpine(&client).await;

    let name = unique_name();
    let _cleanup = ContainerCleanup {
        client: client.clone(),
        name: name.clone(),
    };
    let (test_key, test_value) = TEST_LABEL.split_once('=').expect("TEST_LABEL is key=value");

    let id = client
        .create_container(&name)
        .image(ALPINE)
        .entrypoint(vec!["sh".to_string()])
        .cmd(vec!["-c".to_string(), "sleep 30".to_string()])
        .with_label(test_key, test_value)
        .call()
        .await
        .expect("create_container");

    client.start_container(&id).await.expect("first start");
    client.start_container(&id).await.expect("second start");

    let details = client
        .inspect_container(&id)
        .await
        .expect("inspect_container");
    assert_eq!(details.state.status, ContainerStatus::Running);
}

#[tokio::test(flavor = "multi_thread")]
async fn create_with_bind_mounts_the_volume() {
    let client = Docker::connect().await.expect("connect");
    pull_alpine(&client).await;

    let volume = unique_name();
    let vol = client
        .create_volume(&volume)
        .call()
        .await
        .expect("create_volume");
    let _vol_cleanup = VolumeCleanup {
        client: client.clone(),
        name: vol.name.clone(),
    };

    let name = unique_name();
    let _cleanup = ContainerCleanup {
        client: client.clone(),
        name: name.clone(),
    };
    let (test_key, test_value) = TEST_LABEL.split_once('=').expect("TEST_LABEL is key=value");

    let id = client
        .create_container(&name)
        .image(ALPINE)
        .entrypoint(vec!["sh".to_string()])
        .cmd(vec![
            "-c".to_string(),
            "echo hi > /data/marker && sleep 30".to_string(),
        ])
        .with_bind(&vol.name, "/data")
        .with_label(test_key, test_value)
        .call()
        .await
        .expect("create_container");
    client.start_container(&id).await.expect("start_container");

    let details = client
        .inspect_container(&id)
        .await
        .expect("inspect_container");
    assert_eq!(details.state.status, ContainerStatus::Running);
}
