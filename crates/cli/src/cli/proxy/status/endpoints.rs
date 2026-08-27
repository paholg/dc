//! Work out what the proxy *should* be serving.
//!
//! This deliberately mirrors how the proxy itself decides
//! (`crates/proxy/src/events.rs`: `bootstrap`, `sync_compose_project`,
//! `derive_workspace_for`) so that a disagreement between the two shows up as a
//! failed check rather than as a row that quietly doesn't exist. Everything
//! here comes out of a single container listing, so it resolves fast enough to
//! build the table from.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::Arc;

use docker::{
    COMPOSE_PROJECT_LABEL, COMPOSE_SERVICE_LABEL, ContainerStatus, ContainerSummary, Docker,
    PROJECT_LABEL, PROXY_LABEL, PROXY_SERVICE_LABEL, PROXY_SIDECAR_LABEL, WORKSPACE_LABEL,
};
use eyre::{Result, WrapErr};
use indexmap::IndexMap;
use shared::{ProxyOptions, ProxyService, SidecarPlan};

/// Everything `proxy status` checks, plus the sidecars it found along the way.
pub(super) struct Discovery {
    pub(super) endpoints: Vec<Arc<Endpoint>>,
    pub(super) sidecars: Vec<Sidecar>,
}

/// One row: one service. The proxy serves each on the same fixed pair of
/// ports, so the row covers both — https on 443 and http on 80, each reaching
/// `http_proxy_port`.
pub(super) struct Endpoint {
    pub(super) project: String,
    pub(super) workspace: String,
    pub(super) service: String,
    /// `None` when the hostname template failed to render.
    pub(super) hostname: Option<String>,
    /// `None` for a DNS-only service: the proxy answers for its hostname but
    /// puts no listeners in front of it.
    pub(super) http_proxy_port: Option<u16>,
    /// `None` when the service has no container at all.
    pub(super) container: Option<Target>,
    /// The sidecar this service should have, if any.
    pub(super) sidecar: Option<Arc<ExpectedSidecar>>,
    /// Another endpoint that renders the same hostname. The proxy keeps the
    /// first registration and ignores the rest.
    pub(super) collides_with: Option<String>,
}

#[derive(Clone)]
pub(super) struct Target {
    pub(super) id: String,
    pub(super) status: ContainerStatus,
    pub(super) ip: Option<IpAddr>,
}

pub(super) struct ExpectedSidecar {
    pub(super) plan_hash: String,
}

pub(super) struct Sidecar {
    pub(super) id: String,
    pub(super) status: ContainerStatus,
    pub(super) target: Option<String>,
    pub(super) plan_hash: Option<String>,
    /// `(project, workspace, service)`, from the sidecar's own labels.
    pub(super) key: (String, String, String),
}

impl Endpoint {
    pub(super) fn needs_sidecar(&self) -> bool {
        self.http_proxy_port.is_some()
    }

    pub(super) fn key(&self) -> (String, String, String) {
        (
            self.project.clone(),
            self.workspace.clone(),
            self.service.clone(),
        )
    }
}

/// List every container once and derive the full set of endpoints from it.
pub(super) async fn discover(
    docker: &Docker,
    options: &BTreeMap<String, ProxyOptions>,
) -> Result<Discovery> {
    let containers = docker
        .list_containers()
        .all(true)
        .call()
        .await
        .wrap_err("list containers")?;

    Ok(build(&containers, options))
}

/// The pure half of [`discover`], so the grouping rules can be tested without a
/// daemon.
fn build(containers: &[ContainerSummary], options: &BTreeMap<String, ProxyOptions>) -> Discovery {
    let sidecars = collect_sidecars(containers);

    // Compose project -> its containers, skipping our own.
    let mut compose: IndexMap<&str, Vec<&ContainerSummary>> = IndexMap::new();
    for container in containers {
        if container.labels.contains_key(PROXY_SIDECAR_LABEL)
            || container.labels.contains_key(PROXY_LABEL)
        {
            continue;
        }
        let Some(project) = container.labels.get(COMPOSE_PROJECT_LABEL) else {
            continue;
        };
        compose.entry(project).or_default().push(container);
    }

    let mut endpoints = Vec::new();
    for (compose_project, members) in compose {
        // The "primary" is whichever container carries our project label; it's
        // what ties a compose project to a devconcurrent one.
        let Some(project) = members
            .iter()
            .find_map(|c| c.labels.get(PROJECT_LABEL))
            .filter(|p| !p.is_empty())
        else {
            continue;
        };
        let Some(opts) = options.get(project.as_str()) else {
            continue;
        };
        // A workspace that isn't up has nothing for the proxy to serve, and its
        // containers hang around for days after a `dc down`. Reporting each one
        // as broken would bury the workspaces you actually have running.
        if !members.iter().any(|c| c.state == ContainerStatus::Running) {
            continue;
        }
        let workspace = derive_workspace(&members, compose_project);
        let root = workspace == *project;

        let mut seen: Vec<&str> = Vec::new();
        for container in &members {
            let Some(service) = container.labels.get(COMPOSE_SERVICE_LABEL) else {
                continue;
            };
            seen.push(service);
            let target = Some(Target {
                id: container.id.clone(),
                status: container.state,
                ip: container_ip(container),
            });
            endpoints.push(row_for(
                opts,
                project,
                &workspace,
                service,
                root,
                opts.services.get(service),
                target,
            ));
        }

        // A configured service with no container of its own: the workspace is
        // up, but this piece of it isn't.
        for (service, svc) in &opts.services {
            if seen.contains(&service.as_str()) {
                continue;
            }
            endpoints.push(row_for(
                opts,
                project,
                &workspace,
                service,
                root,
                Some(svc),
                None,
            ));
        }
    }

    mark_collisions(&mut endpoints);

    Discovery {
        endpoints: endpoints.into_iter().map(Arc::new).collect(),
        sidecars,
    }
}

/// The one row a service gets. A service with no container port still gets
/// one — the proxy registers a hostname for it either way.
fn row_for(
    opts: &ProxyOptions,
    project: &str,
    workspace: &str,
    service: &str,
    root: bool,
    svc: Option<&ProxyService>,
    target: Option<Target>,
) -> Endpoint {
    let hostname = opts.render_hostname(project, workspace, service, root);
    Endpoint {
        project: project.to_string(),
        workspace: workspace.to_string(),
        service: service.to_string(),
        sidecar: svc.and_then(|svc| expected_sidecar(hostname.as_deref(), svc)),
        hostname,
        http_proxy_port: svc.and_then(|s| s.http_proxy_port),
        container: target,
        collides_with: None,
    }
}

/// The plan the proxy would build for this service, hashed the same way it
/// stamps it on the sidecar it creates.
fn expected_sidecar(hostname: Option<&str>, svc: &ProxyService) -> Option<Arc<ExpectedSidecar>> {
    let port = svc.http_proxy_port?;
    // The proxy falls back to this shape when a template fails to render.
    let hostname = hostname?.to_string();
    let plan = SidecarPlan { hostname, port };
    Some(Arc::new(ExpectedSidecar {
        plan_hash: plan.hash(),
    }))
}

fn collect_sidecars(containers: &[ContainerSummary]) -> Vec<Sidecar> {
    containers
        .iter()
        .filter(|c| c.labels.contains_key(PROXY_SIDECAR_LABEL))
        .map(|c| Sidecar {
            id: c.id.clone(),
            status: c.state,
            target: c.labels.get(docker::PROXY_TARGET_LABEL).cloned(),
            plan_hash: c.labels.get(docker::PROXY_CONFIG_HASH_LABEL).cloned(),
            key: (
                c.labels.get(PROJECT_LABEL).cloned().unwrap_or_default(),
                c.labels.get(WORKSPACE_LABEL).cloned().unwrap_or_default(),
                c.labels
                    .get(PROXY_SERVICE_LABEL)
                    .cloned()
                    .unwrap_or_default(),
            ),
        })
        .collect()
}

/// Prefer the explicit workspace label, else the compose project name with a
/// `_devcontainer` suffix stripped — which is what makes VSCode-launched
/// workspaces work. Same rule as the proxy's `derive_workspace_for`.
fn derive_workspace(members: &[&ContainerSummary], compose_project: &str) -> String {
    members
        .iter()
        .find_map(|c| c.labels.get(WORKSPACE_LABEL))
        .filter(|ws| !ws.is_empty())
        .cloned()
        .unwrap_or_else(|| {
            compose_project
                .strip_suffix("_devcontainer")
                .unwrap_or(compose_project)
                .to_string()
        })
}

/// First non-empty IP from any network, matching the proxy's
/// `inspect_container_ip`.
fn container_ip(container: &ContainerSummary) -> Option<IpAddr> {
    container
        .network_settings
        .networks
        .values()
        .filter_map(|e| e.ip_address.as_deref())
        .find(|ip| !ip.is_empty())
        .and_then(|ip| ip.parse().ok())
}

/// Note every endpoint whose hostname another one already claimed. The proxy
/// keeps the first registration and warns about the rest into a log nobody
/// reads, so the losing rows would otherwise just look mysteriously wrong.
fn mark_collisions(endpoints: &mut [Endpoint]) {
    let mut first: HashMap<String, String> = HashMap::new();
    for endpoint in endpoints.iter_mut() {
        let Some(hostname) = endpoint.hostname.clone() else {
            continue;
        };
        let owner = format!("{}/{}", endpoint.workspace, endpoint.service);
        match first.get(&hostname) {
            Some(existing) if *existing != owner => {
                endpoint.collides_with = Some(existing.clone());
            }
            Some(_) => {}
            None => {
                first.insert(hostname, owner);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn container(id: &str, labels: &[(&str, &str)], ip: &str) -> ContainerSummary {
        container_in_state(id, labels, ip, "running")
    }

    fn container_in_state(
        id: &str,
        labels: &[(&str, &str)],
        ip: &str,
        state: &str,
    ) -> ContainerSummary {
        let labels: serde_json::Map<String, serde_json::Value> = labels
            .iter()
            .map(|(k, v)| ((*k).to_string(), json!(v)))
            .collect();
        serde_json::from_value(json!({
            "Id": id,
            "Names": [],
            "Image": "img",
            "State": state,
            "Created": 0,
            "Labels": labels,
            "Ports": [],
            "NetworkSettings": {"Networks": {"default": {"IPAddress": ip}}},
        }))
        .expect("valid container summary")
    }

    fn options(value: serde_json::Value) -> BTreeMap<String, ProxyOptions> {
        [(
            "proj".to_string(),
            serde_json::from_value(value).expect("valid proxy options"),
        )]
        .into_iter()
        .collect()
    }

    fn app_and_db() -> serde_json::Value {
        json!({
            "enable": true,
            "services": {
                "app": {"httpProxyPort": 8080},
                "db": {},
            },
        })
    }

    #[test]
    fn one_row_per_service_whether_or_not_it_has_an_http_proxy_port() {
        let containers = [
            container(
                "app-cid",
                &[
                    (COMPOSE_PROJECT_LABEL, "feature_devcontainer"),
                    (COMPOSE_SERVICE_LABEL, "app"),
                    (PROJECT_LABEL, "proj"),
                    (WORKSPACE_LABEL, "feature"),
                ],
                "172.18.0.2",
            ),
            container(
                "db-cid",
                &[
                    (COMPOSE_PROJECT_LABEL, "feature_devcontainer"),
                    (COMPOSE_SERVICE_LABEL, "db"),
                ],
                "172.18.0.3",
            ),
        ];

        let found = build(&containers, &options(app_and_db()));
        let rows: Vec<_> = found
            .endpoints
            .iter()
            .map(|e| {
                (
                    e.service.as_str(),
                    e.http_proxy_port,
                    e.hostname.clone().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("app", Some(8080), "feature.app.test".to_string()),
                ("db", None, "feature.db.test".to_string()),
            ]
        );
    }

    #[test]
    fn only_a_service_with_an_http_proxy_port_expects_a_sidecar() {
        let containers = [container(
            "app-cid",
            &[
                (COMPOSE_PROJECT_LABEL, "feature_devcontainer"),
                (COMPOSE_SERVICE_LABEL, "app"),
                (PROJECT_LABEL, "proj"),
                (WORKSPACE_LABEL, "feature"),
            ],
            "172.18.0.2",
        )];

        let found = build(&containers, &options(app_and_db()));
        let [app, db] = &found.endpoints[..] else {
            panic!("expected a row each for app and db");
        };
        assert!(app.sidecar.is_some());
        assert!(app.needs_sidecar());
        assert!(db.sidecar.is_none());
        assert!(!db.needs_sidecar());
    }

    #[test]
    fn a_configured_service_with_no_container_still_gets_rows() {
        let containers = [container(
            "app-cid",
            &[
                (COMPOSE_PROJECT_LABEL, "feature_devcontainer"),
                (COMPOSE_SERVICE_LABEL, "app"),
                (PROJECT_LABEL, "proj"),
                (WORKSPACE_LABEL, "feature"),
            ],
            "172.18.0.2",
        )];

        let found = build(&containers, &options(app_and_db()));
        let db = found
            .endpoints
            .iter()
            .find(|e| e.service == "db")
            .expect("db row");
        assert!(db.container.is_none());
    }

    #[test]
    fn the_workspace_falls_back_to_the_compose_project_name() {
        let containers = [container(
            "app-cid",
            &[
                (COMPOSE_PROJECT_LABEL, "feature_devcontainer"),
                (COMPOSE_SERVICE_LABEL, "app"),
                (PROJECT_LABEL, "proj"),
            ],
            "172.18.0.2",
        )];

        let found = build(&containers, &options(app_and_db()));
        assert_eq!(found.endpoints[0].workspace, "feature");
    }

    #[test]
    fn collisions_point_at_whoever_registered_first() {
        // Both workspaces render the same hostname, since the template ignores
        // the workspace.
        let opts = options(json!({
            "enable": true,
            "hostname": "{{project}}.test",
            "services": {"app": {"ports": []}},
        }));
        let containers = [
            container(
                "one-cid",
                &[
                    (COMPOSE_PROJECT_LABEL, "one_devcontainer"),
                    (COMPOSE_SERVICE_LABEL, "app"),
                    (PROJECT_LABEL, "proj"),
                    (WORKSPACE_LABEL, "one"),
                ],
                "172.18.0.2",
            ),
            container(
                "two-cid",
                &[
                    (COMPOSE_PROJECT_LABEL, "two_devcontainer"),
                    (COMPOSE_SERVICE_LABEL, "app"),
                    (PROJECT_LABEL, "proj"),
                    (WORKSPACE_LABEL, "two"),
                ],
                "172.18.0.3",
            ),
        ];

        let found = build(&containers, &opts);
        assert_eq!(found.endpoints[0].collides_with, None);
        assert_eq!(found.endpoints[1].collides_with.as_deref(), Some("one/app"),);
    }

    #[test]
    fn a_workspace_that_is_entirely_down_is_left_out() {
        let containers = [
            container_in_state(
                "app-cid",
                &[
                    (COMPOSE_PROJECT_LABEL, "old_devcontainer"),
                    (COMPOSE_SERVICE_LABEL, "app"),
                    (PROJECT_LABEL, "proj"),
                    (WORKSPACE_LABEL, "old"),
                ],
                "",
                "exited",
            ),
            container_in_state(
                "db-cid",
                &[
                    (COMPOSE_PROJECT_LABEL, "old_devcontainer"),
                    (COMPOSE_SERVICE_LABEL, "db"),
                ],
                "",
                "exited",
            ),
        ];

        let found = build(&containers, &options(app_and_db()));
        assert!(found.endpoints.is_empty());
    }

    #[test]
    fn projects_without_proxy_config_are_ignored() {
        let containers = [container(
            "app-cid",
            &[
                (COMPOSE_PROJECT_LABEL, "feature_devcontainer"),
                (COMPOSE_SERVICE_LABEL, "app"),
                (PROJECT_LABEL, "other"),
                (WORKSPACE_LABEL, "feature"),
            ],
            "172.18.0.2",
        )];

        let found = build(&containers, &options(app_and_db()));
        assert!(found.endpoints.is_empty());
    }

    #[test]
    fn sidecars_are_collected_separately_from_endpoints() {
        let containers = [container(
            "sidecar-cid",
            &[
                (PROXY_SIDECAR_LABEL, "true"),
                (docker::PROXY_TARGET_LABEL, "abc123"),
                (docker::PROXY_CONFIG_HASH_LABEL, "deadbeef"),
                (PROJECT_LABEL, "proj"),
                (WORKSPACE_LABEL, "feature"),
                (PROXY_SERVICE_LABEL, "app"),
            ],
            "",
        )];

        let found = build(&containers, &options(app_and_db()));
        assert!(found.endpoints.is_empty());
        assert_eq!(found.sidecars.len(), 1);
        assert_eq!(found.sidecars[0].target.as_deref(), Some("abc123"));
        assert_eq!(
            found.sidecars[0].key,
            ("proj".into(), "feature".into(), "app".into()),
        );
    }
}
