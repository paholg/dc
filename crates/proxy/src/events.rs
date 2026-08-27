//! Manage proxy sidecars based on compose container events.
//!
//! Treats a container as part of a project iff one of the compose containers has
//! the label `com.paholg.devconcurrent.project=PROJECT_NAME`. If that container
//! matches one of the projects services, then a sidecar is launched for it.
//!
//! Every start/die event triggers a full sync of the affected compose
//! project. This handles arbitrary startup orders (siblings before the
//! primary, etc.) without explicit state machines or pending queues.
//!
//! Events can still be missed — the daemon restarts, the socket drops, a
//! reconnect lands after the gap — so the loop also reconciles against the
//! real container list on every (re)subscribe and on a timer. Nothing here
//! depends on having seen every event; the periodic [`resync`] is what makes
//! a missed `die` (leaked sidecar, DNS pointing at a dead IP) heal itself.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::time::Duration;

use docker::{
    COMPOSE_PROJECT_LABEL, COMPOSE_SERVICE_LABEL, Docker, EventActor, NetworkSettings,
    PROJECT_LABEL, PROXY_LABEL, WORKSPACE_LABEL,
};
use eyre::Result;
use futures_util::StreamExt;
use indexmap::IndexMap;
use shared::{ProxyOptions, ProxyService};

use crate::certs::CaHolder;
use crate::registry::{Registry, RunningService};
use crate::sidecar;

/// How often to reconcile tracked state against docker regardless of events.
const RESYNC_INTERVAL: Duration = Duration::from_secs(60);

/// Backoff between a dropped event stream and the next subscribe attempt.
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

/// Run the event loop. Reconnects on connection drops with a brief backoff.
pub async fn run(docker: Docker, registry: Registry, ca: Option<CaHolder>) {
    // Timestamp of the last event we processed, replayed on reconnect so the
    // gap is (mostly) covered. The daemon may resend the event at exactly this
    // time; handling is idempotent, so duplicates are harmless.
    let mut since: Option<String> = None;
    loop {
        let mut builder = docker
            .events()
            .with_type("container")
            .with_event("start")
            .with_event("die")
            .with_event("destroy");
        if let Some(ts) = &since {
            builder = builder.since(ts);
        }
        let stream = match builder.call().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("failed to open docker events: {e}; retrying in 2s");
                tokio::time::sleep(RECONNECT_DELAY).await;
                continue;
            }
        };
        tokio::pin!(stream);
        tracing::info!(since, "subscribed to docker events");

        // Subscribe first, then reconcile: anything that changed while we
        // weren't listening is caught here, and anything that changes from now
        // on arrives as an event.
        resync(&docker, &registry, ca.as_ref()).await;

        let mut ticker = tokio::time::interval(RESYNC_INTERVAL);
        // A resync slower than the interval shouldn't queue up more of them.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await; // The first tick completes immediately.
        loop {
            tokio::select! {
                item = stream.next() => match item {
                    Some(Ok(ev)) => {
                        if let Some(ts) = ev.timestamp() {
                            since = Some(ts);
                        }
                        handle_event(&docker, &registry, ca.as_ref(), ev).await;
                    }
                    // One event we can't parse costs us that event only; the
                    // next resync covers whatever it would have told us.
                    Some(Err(e @ docker::Error::Json { .. })) => {
                        tracing::warn!("skipping unparseable docker event: {e}");
                    }
                    Some(Err(e)) => {
                        tracing::warn!("docker events stream error: {e}");
                        break;
                    }
                    None => {
                        tracing::warn!("docker events stream ended");
                        break;
                    }
                },
                _ = ticker.tick() => resync(&docker, &registry, ca.as_ref()).await,
            }
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

/// Reconcile everything we track against what docker actually has running:
/// drop services whose container is gone (and their sidecars), pick up
/// containers we never saw start, and remove sidecars left behind by a proxy
/// or event-stream outage.
pub(crate) async fn resync(docker: &Docker, registry: &Registry, ca: Option<&CaHolder>) {
    match docker.list_containers().call().await {
        Ok(containers) => {
            let alive: HashMap<String, Option<IpAddr>> = containers
                .into_iter()
                .map(|c| (c.id, first_ip(&c.network_settings)))
                .collect();
            for svc in registry.reconcile_services(&alive).await {
                tracing::info!(
                    project = svc.project,
                    workspace = svc.workspace,
                    service = svc.service,
                    container = svc.target_cid,
                    "service container is gone; untracking"
                );
                if let Some(sidecar_id) = svc.sidecar_id {
                    sidecar::remove_sidecar(docker, &sidecar_id).await;
                }
            }
        }
        Err(e) => tracing::warn!("list containers during resync: {e}"),
    }
    if let Err(e) = bootstrap(docker, registry, ca).await {
        tracing::warn!("adopting running containers during resync: {e:?}");
    }
    if let Err(e) = sidecar::sweep_orphans(docker).await {
        tracing::warn!("orphan sweep failed: {e:?}");
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
        http_proxy_port = port_config.and_then(|s| s.http_proxy_port),
        "adopting service"
    );

    let sidecar_id = if let Some(svc) = port_config.filter(|s| s.http_proxy_port.is_some()) {
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
    first_ip(&details.network_settings)
        .ok_or_else(|| eyre::eyre!("container {cid} has no network with a parseable IP"))
}

/// First non-empty, parseable IP across a container's networks.
fn first_ip(settings: &NetworkSettings) -> Option<IpAddr> {
    settings
        .networks
        .values()
        .filter_map(|endpoint| endpoint.ip_address.as_deref())
        .filter(|raw| !raw.is_empty())
        .find_map(|raw| match raw.parse::<IpAddr>() {
            Ok(ip) => Some(ip),
            Err(e) => {
                tracing::warn!(ip = raw, "unparseable container IP, skipping: {e}");
                None
            }
        })
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
