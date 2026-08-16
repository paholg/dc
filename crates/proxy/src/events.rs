//! Manage proxy sidecars based on compose container events.
//!
//! Treats a container as part of a project iff one of the compose containers has
//! the label `com.paholg.devconcurrent.project=PROJECT_NAME`. If that container
//! matches one of the projects services, then a sidecar is launched for it.
//!
//! Every start/die event triggers a full sync of the affected compose
//! project. This handles arbitrary startup orders (siblings before the
//! primary, etc.) without explicit state machines or pending queues.

use std::collections::HashSet;
use std::net::IpAddr;

use docker::{
    COMPOSE_PROJECT_LABEL, COMPOSE_SERVICE_LABEL, Docker, EventActor, PROJECT_LABEL, PROXY_LABEL,
    WORKSPACE_LABEL,
};
use eyre::Result;
use futures_util::StreamExt;
use indexmap::IndexMap;
use shared::{ProxyOptions, ProxyService};

use crate::certs::CaHolder;
use crate::registry::{Registry, RunningService};
use crate::sidecar;

/// Run the event loop. Reconnects on connection drops with a brief backoff.
pub async fn run(docker: Docker, registry: Registry, ca: Option<CaHolder>) {
    loop {
        let stream = match docker
            .events()
            .with_type("container")
            .with_event("start")
            .with_event("die")
            .with_event("destroy")
            .call()
            .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("failed to open docker events: {e}; retrying in 2s");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
        };
        tokio::pin!(stream);
        tracing::info!("subscribed to docker events");

        while let Some(item) = stream.next().await {
            match item {
                Ok(ev) => handle_event(&docker, &registry, ca.as_ref(), ev).await,
                Err(e) => {
                    tracing::warn!("docker events stream error: {e}");
                    break;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

async fn handle_event(
    docker: &Docker,
    registry: &Registry,
    ca: Option<&CaHolder>,
    ev: docker::EventMessage,
) {
    // Ignore events on our own sidecars.
    if ev.actor.attributes.contains_key(PROXY_LABEL) {
        return;
    }
    let Some(action) = ev.action.as_deref() else {
        return;
    };
    match action {
        "start" => {
            if let Some(cp) = ev.actor.attributes.get(COMPOSE_PROJECT_LABEL).cloned() {
                sync_compose_project(docker, registry, ca, &cp).await;
            }
        }
        "die" | "destroy" => on_die(docker, registry, ev.actor).await,
        _ => {}
    }
}

async fn on_die(docker: &Docker, registry: &Registry, actor: EventActor) {
    let Some(svc) = registry.untrack_service(&actor.id).await else {
        return;
    };
    if let Some(sidecar_id) = svc.sidecar_id {
        sidecar::remove_sidecar(docker, &sidecar_id).await;
    }
}

/// Re-sync one compose project: discover its primary (any container in the
/// project labeled with `dev.devconcurrent.project`), look up the matching
/// config, and adopt every container whose service name appears there.
/// Already-adopted containers are skipped.
pub(crate) async fn sync_compose_project(
    docker: &Docker,
    registry: &Registry,
    ca: Option<&CaHolder>,
    compose_project: &str,
) {
    let containers = match docker
        .list_containers()
        .with_label(COMPOSE_PROJECT_LABEL, compose_project)
        .call()
        .await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(compose_project, "list containers: {e}");
            return;
        }
    };

    let Some(primary) = containers
        .iter()
        .find(|c| c.labels.contains_key(PROJECT_LABEL))
    else {
        // No primary present (yet, or this project isn't ours). Siblings
        // that arrived earlier will be picked up when the primary's start
        // event fires.
        return;
    };

    let Some(project) = primary.labels.get(PROJECT_LABEL).cloned() else {
        return;
    };
    let Some(opts) = registry.config_for(&project).await else {
        tracing::debug!(
            project,
            "compose project references unknown devconcurrent project"
        );
        return;
    };
    let workspace = derive_workspace_for(&primary.labels, compose_project);

    for c in &containers {
        if registry.has_service(&c.id).await {
            continue;
        }
        let Some(compose_service) = c.labels.get(COMPOSE_SERVICE_LABEL).cloned() else {
            continue;
        };
        let port_config = opts.services.get(&compose_service).cloned();
        adopt(
            docker,
            registry,
            ca,
            &project,
            &opts,
            &workspace,
            &compose_service,
            port_config.as_ref(),
            &c.id,
        )
        .await;
    }
}

/// Inspect the service container, create a sidecar if `port_config` lists
/// ports, and register it. Services without listed ports register DNS only;
/// they're reachable on their natural ports but the source IP isn't
/// rewritten to 127.0.0.1.
#[allow(clippy::too_many_arguments)]
async fn adopt(
    docker: &Docker,
    registry: &Registry,
    ca: Option<&CaHolder>,
    project: &str,
    opts: &ProxyOptions,
    workspace: &str,
    service: &str,
    port_config: Option<&ProxyService>,
    target_cid: &str,
) {
    let container_ip = match inspect_container_ip(docker, target_cid).await {
        Ok(ip) => ip,
        Err(e) => {
            tracing::error!(
                container = %target_cid,
                project,
                workspace,
                service,
                "couldn't read container IP, skipping: {e:?}"
            );
            return;
        }
    };

    tracing::info!(
        container = %target_cid,
        project,
        workspace,
        service,
        %container_ip,
        container_port = port_config.and_then(|s| s.container_port),
        "adopting service"
    );

    let sidecar_id = if let Some(svc) = port_config.filter(|s| s.container_port.is_some()) {
        let root = workspace == project;
        let hostname = opts
            .render_hostname(project, workspace, service, root)
            .unwrap_or_else(|| {
                tracing::warn!(project, "failed to render domain template");
                format!("{service}.{project}.test")
            });
        match sidecar::create_sidecar(
            docker, ca, project, workspace, service, svc, &hostname, target_cid,
        )
        .await
        {
            Ok(id) => id,
            Err(e) => {
                tracing::error!(
                    project,
                    workspace,
                    service,
                    target_cid,
                    "create sidecar failed: {e:?}"
                );
                None
            }
        }
    } else {
        None
    };

    registry
        .track_service(RunningService {
            project: project.to_string(),
            workspace: workspace.to_string(),
            service: service.to_string(),
            target_cid: target_cid.to_string(),
            container_ip,
            sidecar_id,
        })
        .await;
}

/// Workspace identifier: prefer the explicit `WORKSPACE_LABEL` (set by `dc
/// up`'s compose override), otherwise fall back to the compose project name
/// with the `_devcontainer` suffix stripped if present. The fallback is what
/// makes VSCode-launched workspaces work.
fn derive_workspace_for(labels: &IndexMap<String, String>, compose_project: &str) -> String {
    if let Some(ws) = labels.get(WORKSPACE_LABEL).filter(|s| !s.is_empty()) {
        return ws.clone();
    }
    compose_project
        .strip_suffix("_devcontainer")
        .unwrap_or(compose_project)
        .to_string()
}

/// Inspect the container and return the first non-empty IP from any of its
/// networks. Compose puts each service on the project's default network; we
/// don't care which network as long as we get an IP routable from the host
/// (directly on Linux, via docker-mac-net-connect on macOS).
pub(crate) async fn inspect_container_ip(docker: &Docker, cid: &str) -> Result<IpAddr> {
    let details = docker
        .inspect_container(cid)
        .await
        .map_err(|e| eyre::eyre!("inspect container {cid}: {e}"))?;
    for endpoint in details.network_settings.networks.values() {
        let Some(raw) = endpoint.ip_address.as_deref() else {
            continue;
        };
        if raw.is_empty() {
            continue;
        }
        return raw
            .parse::<IpAddr>()
            .map_err(|e| eyre::eyre!("parse container ip {raw:?}: {e}"));
    }
    eyre::bail!("container {cid} has no network with an IP");
}

/// Bootstrap: at startup, find every compose project containing at least one
/// container with `PROJECT_LABEL` and sync it.
pub(crate) async fn bootstrap(
    docker: &Docker,
    registry: &Registry,
    ca: Option<&CaHolder>,
) -> Result<()> {
    let primaries = docker
        .list_containers()
        .with_label_key(PROJECT_LABEL)
        .call()
        .await?;
    let mut seen: HashSet<String> = HashSet::new();
    for c in primaries {
        if c.labels.contains_key(PROXY_LABEL) {
            continue;
        }
        let Some(cp) = c.labels.get(COMPOSE_PROJECT_LABEL) else {
            continue;
        };
        if seen.insert(cp.clone()) {
            sync_compose_project(docker, registry, ca, cp).await;
        }
    }
    Ok(())
}
