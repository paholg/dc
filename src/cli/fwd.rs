use std::collections::HashMap;
use std::net::TcpListener;

use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig, PortBinding, PortMap};
use bollard::query_parameters::{
    CreateContainerOptions, CreateImageOptionsBuilder, ListContainersOptions,
    RemoveContainerOptions,
};
use clap::Args;
use eyre::eyre;
use futures::StreamExt;

use crate::config::Config;
use crate::devcontainer::DevContainer;
use crate::workspace::{Speed, Workspace};
use bollard::secret::ContainerSummaryStateEnum;

const SOCAT_IMAGE: &str = "docker.io/alpine/socat:latest";

/// Forward a local TCP port to a running devcontainer
///
/// Supply either project or name, or leave both blank to get a picker.
#[derive(Debug, Args)]
#[command(verbatim_doc_comment)]
pub struct Fwd {
    #[arg(short, long, conflicts_with = "name")]
    project: Option<String>,

    #[arg(short, long, conflicts_with = "project")]
    name: Option<String>,

    /// Host port to listen on (defaults to fwd_port in config)
    port: Option<u16>,
}

impl Fwd {
    pub async fn run(self, docker: &Docker, config: &Config) -> eyre::Result<()> {
        let (container_id, project, ws) = if let Some(ref name) = self.name {
            let workspaces = Workspace::list_project(docker, None, config, Speed::Fast).await?;
            let ws = workspaces
                .into_iter()
                .find(|ws| {
                    ws.path
                        .file_name()
                        .map(|f| f == name.as_str())
                        .unwrap_or(false)
                })
                .ok_or_else(|| eyre!("no workspace found with name: {name}"))?;
            if ws.status != ContainerSummaryStateEnum::RUNNING {
                return Err(eyre!("workspace is not running: {}", ws.path.display()));
            }
            let cid = ws
                .container_ids
                .first()
                .cloned()
                .ok_or_else(|| eyre!("no containers for workspace"))?;
            let project = ws.project.clone();
            (cid, project, ws)
        } else {
            let mut workspaces =
                Workspace::list_project(docker, self.project.as_deref(), config, Speed::Fast)
                    .await?;
            workspaces.retain(|ws| ws.status == ContainerSummaryStateEnum::RUNNING);
            let (path, cid, project) = crate::workspace::pick_workspace(workspaces)?;
            let all = Workspace::list_project(docker, Some(&project), config, Speed::Fast).await?;
            let ws = all
                .into_iter()
                .find(|w| w.path == path)
                .ok_or_else(|| eyre!("workspace disappeared"))?;
            (cid, project, ws)
        };

        let (_, proj) = config.project(Some(&project))?;
        let dc = DevContainer::load(proj)?;
        let dc_options = dc.common.customizations.dc;

        let host_port = self
            .port
            .or(dc_options.forward_port)
            .ok_or_else(|| eyre!("no port specified and no fwdPort in devcontainer.json"))?;

        let container_port = dc_options.container_port.unwrap_or(host_port);

        // Remove existing forwards in this project
        remove_project_sidecars(docker, &project).await?;

        // Check port availability among non-project containers
        check_port_available(docker, &project, host_port).await?;

        // Get container IP and network
        let info = docker.inspect_container(&container_id, None).await?;
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

        // Ensure socat image is available
        ensure_image(docker).await?;

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
        labels.insert("dev.dc.fwd.project".to_string(), project.clone());
        labels.insert(
            "dev.dc.fwd.workspace".to_string(),
            ws.compose_project_name.clone(),
        );

        docker
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

        docker.start_container(&sidecar_name, None).await?;

        eprintln!(
            "Forwarding 127.0.0.1:{host_port} -> {ip}:{container_port} (sidecar: {sidecar_name})"
        );

        Ok(())
    }
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

async fn remove_project_sidecars(docker: &Docker, project: &str) -> eyre::Result<()> {
    let mut filters = HashMap::new();
    filters.insert(
        "label".into(),
        vec![
            "dev.dc.fwd=true".to_string(),
            format!("dev.dc.fwd.project={project}"),
        ],
    );
    let containers = docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            filters: Some(filters),
            ..Default::default()
        }))
        .await?;
    for c in containers {
        if let Some(id) = c.id {
            let _ = docker
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

async fn check_port_available(docker: &Docker, project: &str, host_port: u16) -> eyre::Result<()> {
    // Check if another container (not our project's sidecar) has this port
    let containers = docker
        .list_containers(Some(ListContainersOptions {
            all: false,
            ..Default::default()
        }))
        .await?;
    for c in containers {
        let is_ours = c
            .labels
            .as_ref()
            .and_then(|l| l.get("dev.dc.fwd.project"))
            .map(|p| p == project)
            .unwrap_or(false);
        if is_ours {
            continue;
        }
        if let Some(ports) = c.ports {
            for p in ports {
                if p.public_port == Some(host_port) {
                    let name = c
                        .names
                        .as_ref()
                        .and_then(|n| n.first())
                        .map(|s| s.as_str())
                        .unwrap_or("unknown");
                    return Err(eyre!(
                        "port {host_port} is already published by container {name}"
                    ));
                }
            }
        }
    }

    // Check if a host process holds the port
    if TcpListener::bind(format!("127.0.0.1:{host_port}")).is_err() {
        return Err(eyre!("port {host_port} is already in use on the host"));
    }

    Ok(())
}
