use std::collections::HashMap;

use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig, PortBinding};
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
use crate::devcontainer::forward_port::ForwardPort;
use crate::workspace::Workspace;

const SOCAT_IMAGE: &str = "docker.io/alpine/socat:latest";

/// Forward port(s) to a running devcontainer.
///
/// Supply either project or name, or leave both blank to get a picker.
#[derive(Debug, Args)]
#[command(verbatim_doc_comment)]
pub struct Fwd {
    /// name of workspace [default: current working directory]
    #[arg(add = ArgValueCompleter::new(complete::complete_workspace))]
    name: Option<String>,
}

impl Fwd {
    pub async fn run(self, state: State) -> eyre::Result<()> {
        let name = state.resolve_name(self.name).await?;
        forward(&state, &name).await
    }
}

pub async fn forward(state: &State, name: &str) -> eyre::Result<()> {
    let ws = Workspace::get(state, name).await?;
    let cid = ws.service_container_id()?;

    let dc = state.devcontainer()?;

    let ports = dc.common.forward_ports;

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

    for port in &ports {
        create_sidecar(state, &ws.compose_project_name, &network_name, &ip, port).await?;
    }

    Ok(())
}

async fn create_sidecar(
    state: &State,
    compose_project_name: &str,
    network_name: &str,
    ip: &str,
    fwd_port: &ForwardPort,
) -> eyre::Result<()> {
    let sidecar_name = format!("dc-fwd-{compose_project_name}-{}", fwd_port);
    let port_key = format!("{}/tcp", fwd_port.port);

    let mut port_bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
    port_bindings.insert(
        port_key.clone(),
        Some(vec![PortBinding {
            host_ip: Some("127.0.0.1".to_string()),
            host_port: Some(fwd_port.port.to_string()),
        }]),
    );

    let mut labels = HashMap::new();
    labels.insert("dev.dc.fwd".to_string(), "true".to_string());
    labels.insert("dev.dc.project".to_string(), state.project_name.clone());
    labels.insert(
        "dev.dc.workspace".to_string(),
        compose_project_name.to_string(),
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
                    format!("TCP-LISTEN:{},fork,reuseaddr", fwd_port.port),
                    format!(
                        "TCP:{}:{}",
                        fwd_port.service.as_deref().unwrap_or(ip),
                        fwd_port.port
                    ),
                ]),
                labels: Some(labels),
                exposed_ports: Some(vec![port_key.clone()]),
                host_config: Some(HostConfig {
                    network_mode: Some(network_name.to_string()),
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

    eprintln!("Forwarding to {fwd_port}");

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
            format!("dev.dc.project={project}"),
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
