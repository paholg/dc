use std::collections::HashMap;

use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig, PortBinding, PortMap};
use bollard::query_parameters::{
    CreateContainerOptions, CreateImageOptionsBuilder, ListContainersOptions,
    RemoveContainerOptions,
};
use clap::Args;
use clap_complete::engine::ArgValueCompleter;
use eyre::{WrapErr, eyre};
use futures::StreamExt;

use crate::cli::State;
use crate::complete;
use crate::workspace::Workspace;

const SOCAT_IMAGE: &str = "docker.io/alpine/socat:latest";

/// Forward port(s) to a running devcontainer.
///
/// Supply either project or name, or leave both blank to get a picker.
#[derive(Debug, Args)]
#[command(verbatim_doc_comment)]
pub struct Fwd {
    /// name of workspace [default: Root workspace for project]
    #[arg(add = ArgValueCompleter::new(complete::complete_workspace))]
    name: Option<String>,
}

impl Fwd {
    pub async fn run(self, state: State) -> eyre::Result<()> {
        forward(&state, self.name.as_deref()).await
    }
}

pub async fn forward(state: &State, name: Option<&str>) -> eyre::Result<()> {
    let ws = Workspace::get(state, name).await?;
    let cid = ws.service_container_id()?;

    let dc = state.devcontainer()?;
    let dc_options = dc.common.customizations.dc;

    let host_port = dc_options
        .forward_port
        .ok_or_else(|| eyre!("no forwardPort set in devcontainer.json"))?;

    let container_port = dc_options.container_port.unwrap_or(host_port);

    remove_sidecars(state).await?;

    // Get container IP and network
    let info = state
        .docker
        .docker
        .inspect_container(cid, None)
        .await
        .wrap_err_with(|| format!("failed to inspect container {cid}"))?;
    let networks = info
        .network_settings
        .and_then(|ns| ns.networks)
        .ok_or_else(|| eyre!("container has no networks"))?;

    let (network_name, ip) = networks
        .into_iter()
        .find_map(|(name, ep)| {
            ep.ip_address.and_then(|ip| {
                if ip.is_empty() {
                    None
                } else {
                    Some((name, ip))
                }
            })
        })
        .ok_or_else(|| eyre!("container has no IP address"))?;

    ensure_image(&state.docker.docker).await?;

    // Create and start sidecar
    let sidecar_name = format!("dc-fwd-{}", ws.compose_project_name);
    let port_key = format!("{host_port}/tcp");

    let mut port_bindings: PortMap = HashMap::new();
    port_bindings.insert(
        port_key.clone(),
        Some(vec![PortBinding {
            host_ip: Some("127.0.0.1".to_string()),
            host_port: Some(host_port.to_string()),
        }]),
    );

    let mut labels = HashMap::new();
    labels.insert("dev.dc.fwd".to_string(), "true".to_string());
    labels.insert("dev.dc.fwd.project".to_string(), state.project_name.clone());
    labels.insert(
        "dev.dc.fwd.workspace".to_string(),
        ws.compose_project_name.clone(),
    );

    state
        .docker
        .docker
        .create_container(
            Some(CreateContainerOptions {
                name: Some(sidecar_name.clone()),
                ..Default::default()
            }),
            ContainerCreateBody {
                image: Some(SOCAT_IMAGE.to_string()),
                cmd: Some(vec![
                    format!("TCP-LISTEN:{host_port},fork,reuseaddr"),
                    format!("TCP:{ip}:{container_port}"),
                ]),
                labels: Some(labels),
                exposed_ports: Some(vec![port_key.clone()]),
                host_config: Some(HostConfig {
                    network_mode: Some(network_name),
                    port_bindings: Some(port_bindings),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await?;

    state
        .docker
        .docker
        .start_container(&sidecar_name, None)
        .await?;

    eprintln!(
        "Forwarding 127.0.0.1:{host_port} -> {ip}:{container_port} (sidecar: {sidecar_name})"
    );

    Ok(())
}

async fn ensure_image(docker: &Docker) -> eyre::Result<()> {
    if docker.inspect_image(SOCAT_IMAGE).await.is_ok() {
        return Ok(());
    }
    docker
        .create_image(
            Some(
                CreateImageOptionsBuilder::new()
                    .from_image(SOCAT_IMAGE)
                    .build(),
            ),
            None,
            None,
        )
        .collect::<Vec<_>>()
        .await;
    Ok(())
}

async fn remove_sidecars(state: &State) -> eyre::Result<()> {
    let project = &state.project_name;
    let mut filters = HashMap::new();
    filters.insert(
        "label".into(),
        vec![
            "dev.dc.fwd=true".to_string(),
            format!("dev.dc.fwd.project={project}"),
        ],
    );

    let containers = state
        .docker
        .docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            filters: Some(filters),
            ..Default::default()
        }))
        .await?;

    for c in containers {
        if let Some(id) = c.id {
            let _ = state
                .docker
                .docker
                .remove_container(
                    &id,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await;
        }
    }
    Ok(())
}
