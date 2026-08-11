use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::LazyLock;
use std::time::Duration;

use clap::{Args, Subcommand};
use clap_complete::engine::ArgValueCompleter;
use color_eyre::owo_colors::OwoColorize;
use docker::{
    ContainerStatus, Docker, PROJECT_LABEL, PROXY_CONFIG_HASH_LABEL, PROXY_GROUP_LABEL,
    PROXY_LABEL, PROXY_SERVICE_LABEL, PROXY_SIDECAR_LABEL, WORKSPACE_LABEL,
};
use eyre::{Result, WrapErr};
use shared::{
    ENV_CA_DIR, ENV_DNS_PORT, PROXY_CA_DIR, PROXY_CONFIG_DIR, PROXY_CONFIG_VOLUME,
    PROXY_CONTAINER_NAME, ProxyService,
};
use tokio::net::UdpSocket;

use crate::complete::complete_workspace;
use crate::run::{Runnable, Runner};
use crate::table::{Align, ColumnDef, TableBuilder, text};

mod proxy_state;
pub(crate) use proxy_state::ProxyState;

/// OCI image used by the proxy container.
const PROXY_IMAGE_NAME: &str = "ghcr.io/paholg/devconcurrent-proxy";

/// We keep the proxy and CLI versions in sync, so using the CLI version here is fine.
const PROXY_IMAGE_TAG: &str = env!("CARGO_PKG_VERSION");

/// Host address the DNS port is published on.
const LISTEN_IP: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// Name queried to check that the proxy is answering; it resolves to nothing.
const PROBE_NAME: &str = "readiness-probe.devconcurrent.test";
const PROBE_TIMEOUT: Duration = Duration::from_millis(100);
const DNS_HEADER_LEN: usize = 12;

/// How long the proxy gets to answer a query after being started. Generous:
/// before it binds its sockets it adopts every running service container, which
/// takes a while with a lot of workspaces up.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

static PROXY_IMAGE: LazyLock<String> =
    LazyLock::new(|| format!("{PROXY_IMAGE_NAME}:{PROXY_IMAGE_TAG}"));

#[derive(Debug, Args)]
pub(crate) struct Proxy {
    #[command(subcommand)]
    command: ProxyCommands,
}

#[derive(Debug, Subcommand)]
enum ProxyCommands {
    /// Start or restart the proxy
    Up(ProxyArgs),
    /// Stop and remove the proxy
    Down,
    /// View the current proxy status
    Status(ProxyArgs),
}

#[derive(Debug, Args)]
struct ProxyArgs {
    /// Workspace name (only useful if its devcontainer.json diverges from the root workspace)
    #[arg(short, long, add = ArgValueCompleter::new(complete_workspace))]
    workspace: Option<String>,
}

impl Proxy {
    /// This command is a bit different than most; it needs to operate on multiple projects, but we
    /// still set a workspace so that a user can edit proxy settings from a workspace and test them.
    pub(crate) async fn run(self, project: Option<String>) -> Result<()> {
        match self.command {
            ProxyCommands::Up(args) => {
                let proxy = ProxyState::resolve(project, args.workspace).await?;
                proxy_up(&proxy).await
            }
            ProxyCommands::Status(args) => {
                let proxy = ProxyState::resolve(project, args.workspace).await?;
                proxy_status(&proxy).await
            }
            ProxyCommands::Down => proxy_down().await,
        }
    }
}

struct ProxyRunner {
    new: bool,
    proxy: ProxyState,
}

impl Runnable for ProxyRunner {
    fn name(&self) -> std::borrow::Cow<'_, str> {
        "proxy".into()
    }

    fn description(&self) -> std::borrow::Cow<'_, str> {
        if self.new {
            "starting".into()
        } else {
            "out-of-date; restarting".into()
        }
    }

    async fn run(self, _: crate::run::Token) -> eyre::Result<()> {
        proxy_up(&self.proxy).await
    }
}

/// Bring up the proxy and sidecars, recreating them if they already exist.
async fn proxy_up(proxy: &ProxyState) -> Result<()> {
    proxy
        .docker
        .ensure_image(&PROXY_IMAGE)
        .await
        .wrap_err_with(|| format!("pull {}", *PROXY_IMAGE))?;

    remove_proxy_group(&proxy.docker).await?;

    let id = create_proxy_stopped(proxy).await?;
    proxy.push_configs().await?;
    proxy
        .docker
        .start_container(&id)
        .await
        .wrap_err("start proxy container")?;

    wait_for_ready(&proxy.docker, &id, proxy.config.port).await?;

    tracing::info!("{} proxy is running", "✓".green());
    Ok(())
}

async fn remove_proxy_group(docker: &Docker) -> Result<()> {
    let members = docker
        .list_containers()
        .all(true)
        .with_label(PROXY_GROUP_LABEL, "true")
        .call()
        .await
        .wrap_err("list proxy group")?;

    for c in members {
        match docker.remove_container(&c.id).force(true).call().await {
            Ok(()) | Err(docker::Error::NotFound) => {}
            Err(e) => tracing::warn!(id = %c.id, "remove proxy-group container: {e}"),
        }
    }
    Ok(())
}

/// Ensure the proxy is running.
///
/// If it's already running but with stale config, it's recreated.
pub(crate) async fn ensure_up(proxy: ProxyState) -> Result<()> {
    enum State {
        Down,
        Up,
        Old,
    }

    let hash = proxy.config_hash();
    let state = match proxy.docker.inspect_container(PROXY_CONTAINER_NAME).await {
        Ok(d) => {
            if d.state.running {
                if d.config.labels.get(PROXY_CONFIG_HASH_LABEL) == Some(&hash) {
                    State::Up
                } else {
                    State::Old
                }
            } else {
                State::Down
            }
        }
        Err(docker::Error::NotFound) => State::Down,
        Err(e) => return Err(e).wrap_err("inspect proxy"),
    };

    match state {
        State::Up => Ok(()),
        State::Down => Runner::run(ProxyRunner { new: true, proxy }).await,
        State::Old => Runner::run(ProxyRunner { new: false, proxy }).await,
    }
}

async fn proxy_down() -> Result<()> {
    let docker = Docker::connect().await.wrap_err("connect to docker")?;

    remove_proxy_group(&docker).await?;
    tracing::info!("{} proxy stopped", "✓".green());

    Ok(())
}

async fn proxy_status(proxy: &ProxyState) -> Result<()> {
    match proxy.docker.inspect_container(PROXY_CONTAINER_NAME).await {
        Ok(d) if d.state.running => {
            println!(
                "proxy: {} (image={}, dns port={})",
                "running".green(),
                d.config.image,
                proxy.config.port,
            );
        }
        Ok(d) => {
            println!(
                "proxy: {} ({}, image={})",
                "not running".red(),
                d.state.status,
                d.config.image,
            );
        }
        Err(docker::Error::NotFound) => {
            println!("proxy: {}", "not present".red());
            return Ok(());
        }
        Err(e) => return Err(e).wrap_err("inspect proxy"),
    }

    let sidecars = proxy
        .docker
        .list_containers()
        .all(true)
        .with_label(PROXY_SIDECAR_LABEL, "true")
        .call()
        .await
        .wrap_err("list sidecars")?;

    if sidecars.is_empty() {
        println!();
        println!("no sidecars running");
        return Ok(());
    }

    // project -> workspace -> sorted service rows
    let mut grouped: BTreeMap<String, BTreeMap<String, Vec<ServiceRow>>> = BTreeMap::new();
    for sc in sidecars {
        let project = sc.labels.get(PROJECT_LABEL).cloned().unwrap_or_default();
        let workspace = sc.labels.get(WORKSPACE_LABEL).cloned().unwrap_or_default();
        let service = sc
            .labels
            .get(PROXY_SERVICE_LABEL)
            .cloned()
            .unwrap_or_default();
        let opts = proxy.options.get(&project);
        let svc_cfg = opts.and_then(|o| o.services.get(&service)).cloned();
        let domain = opts
            .and_then(|o| o.render_hostname(&project, &workspace, &service, workspace == project));
        grouped
            .entry(project)
            .or_default()
            .entry(workspace)
            .or_default()
            .push(ServiceRow {
                service,
                domain,
                proxy: sc.state,
                container_id: sc.id,
                ports: svc_cfg,
            });
    }
    for workspaces in grouped.values_mut() {
        for rows in workspaces.values_mut() {
            rows.sort_by(|a, b| a.service.cmp(&b.service));
        }
    }

    for (project, workspaces) in &grouped {
        println!();
        println!("project: {}", project.bold());
        print!("{}", proxy_table(workspaces));
    }
    Ok(())
}

struct ServiceRow {
    service: String,
    domain: Option<String>,
    proxy: ContainerStatus,
    container_id: String,
    ports: Option<ProxyService>,
}

/// One row of the proxy table; `workspace` is blank on all but the first row of
/// each workspace group.
struct ProxyRow {
    workspace: String,
    service: String,
    domain: Option<String>,
    status: ContainerStatus,
    container_id: String,
    ports: Option<ProxyService>,
}

fn proxy_table(workspaces: &BTreeMap<String, Vec<ServiceRow>>) -> String {
    let mut rows: Vec<ProxyRow> = Vec::new();
    for (workspace, services) in workspaces {
        for (i, sc) in services.iter().enumerate() {
            rows.push(ProxyRow {
                workspace: if i == 0 {
                    workspace.clone()
                } else {
                    String::new()
                },
                service: sc.service.clone(),
                domain: sc.domain.clone(),
                status: sc.proxy,
                container_id: sc.container_id.clone(),
                ports: sc.ports.clone(),
            });
        }
    }

    [
        ColumnDef::new("WORKSPACE", Align::Left, |r: &ProxyRow| {
            text(r.workspace.clone())
        }),
        ColumnDef::new("SERVICE", Align::Left, |r: &ProxyRow| {
            text(r.service.clone())
        }),
        ColumnDef::new("DOMAIN", Align::Left, |r: &ProxyRow| {
            text(fmt_domain(r.domain.as_deref()))
        }),
        ColumnDef::new("STATUS", Align::Left, |r: &ProxyRow| {
            text(fmt_status(r.status))
        }),
        ColumnDef::new("CONTAINER", Align::Left, |r: &ProxyRow| {
            text(short_id(&r.container_id))
        }),
        ColumnDef::new("PORTS", Align::Left, |r: &ProxyRow| {
            text(fmt_ports(r.ports.as_ref()))
        }),
    ]
    .into_iter()
    .collect::<TableBuilder<ProxyRow>>()
    .build(&rows, false)
    .rendered()
}

fn fmt_domain(domain: Option<&str>) -> String {
    match domain {
        Some(d) if !d.is_empty() => d.to_string(),
        _ => "-".dimmed().to_string(),
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

fn fmt_status(status: ContainerStatus) -> String {
    match status {
        ContainerStatus::Running => status.green().to_string(),
        ContainerStatus::Exited | ContainerStatus::Dead => status.red().to_string(),
        _ => status.yellow().to_string(),
    }
}

fn fmt_ports(svc: Option<&ProxyService>) -> String {
    match svc {
        Some(svc) if !svc.ports.is_empty() => svc
            .ports
            .iter()
            .map(|p| p.host.to_string())
            .collect::<Vec<_>>()
            .join(", "),
        _ => "-".dimmed().to_string(),
    }
}

/// Wait for the proxy to answer DNS queries.
///
/// Neither of the cheap signals means ready: docker reports the container
/// running the moment its entrypoint execs, and a published port accepts
/// connections whether or not anything in the container is listening yet. Only
/// a reply proves it.
async fn wait_for_ready(docker: &Docker, id: &str, port: u16) -> Result<()> {
    let timeout = READY_TIMEOUT.as_secs();
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    loop {
        match docker.inspect_container(id).await {
            Ok(d) if d.state.running => break,
            Ok(_) => {}
            Err(docker::Error::NotFound) => eyre::bail!("proxy container vanished after start"),
            Err(e) => return Err(e).wrap_err("inspect proxy after start"),
        }
        if std::time::Instant::now() >= deadline {
            eyre::bail!("proxy container did not reach running state within {timeout}s");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let socket = UdpSocket::bind((LISTEN_IP, 0))
        .await
        .wrap_err("bind dns probe socket")?;
    socket
        .connect((LISTEN_IP, port))
        .await
        .wrap_err("connect dns probe socket")?;

    let query = dns_query(rand::random(), PROBE_NAME);
    let mut response = [0u8; 512];
    loop {
        // Errors here are all "not up yet": the proxy drops the packet, or
        // docker's forwarder rejects it because nothing is listening behind it.
        if socket.send(&query).await.is_ok()
            && let Ok(Ok(len)) =
                tokio::time::timeout(PROBE_TIMEOUT, socket.recv(&mut response)).await
            && len >= DNS_HEADER_LEN
            && response[..2] == query[..2]
        {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            eyre::bail!("proxy did not answer a dns query on port {port} within {timeout}s");
        }
        // A failed send returns immediately, so pace the retries ourselves.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A minimal `<name> A IN` query packet, used to check that the proxy is
/// answering. The name need not exist; an NXDOMAIN is just as good an answer.
fn dns_query(id: u16, name: &str) -> Vec<u8> {
    let mut query = Vec::new();
    query.extend_from_slice(&id.to_be_bytes());
    query.extend_from_slice(&[0x01, 0x00]); // standard query, recursion desired
    query.extend_from_slice(&[0x00, 0x01]); // one question
    query.extend_from_slice(&[0x00; 6]); // no answer, authority, or additional records
    for label in name.split('.') {
        query.push(u8::try_from(label.len()).expect("PROBE_NAME has short labels"));
        query.extend_from_slice(label.as_bytes());
    }
    query.push(0); // root label
    query.extend_from_slice(&[0x00, 0x01]); // qtype: A
    query.extend_from_slice(&[0x00, 0x01]); // qclass: IN
    query
}

async fn create_proxy_stopped(proxy: &ProxyState) -> Result<String> {
    let socket_path = proxy.docker.socket().display();

    // The DNS port is published rather than the container sharing the host's
    // network namespace: on macOS and Windows the "host" of a host-networked
    // container is Docker's VM, not the machine the user's resolver runs on.
    let mut builder = proxy
        .docker
        .create_container(PROXY_CONTAINER_NAME)
        .image(&PROXY_IMAGE)
        .with_udp_port_binding(proxy.config.port, LISTEN_IP, proxy.config.port)
        .with_tcp_port_binding(proxy.config.port, LISTEN_IP, proxy.config.port)
        .with_label(PROXY_LABEL, "true")
        .with_label(PROXY_GROUP_LABEL, "true")
        .with_label(PROXY_CONFIG_HASH_LABEL, proxy.config_hash())
        .with_bind(PROXY_CONFIG_VOLUME, PROXY_CONFIG_DIR)
        .with_bind(socket_path, "/var/run/docker.sock")
        .with_env(ENV_DNS_PORT, proxy.config.port);

    if let Some(ca_root) = &proxy.config.ca_root {
        builder = builder
            .with_ro_bind(ca_root.display(), PROXY_CA_DIR)
            .with_env(ENV_CA_DIR, PROXY_CA_DIR);
    }

    builder.call().await.wrap_err("create proxy container")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_query_is_a_well_formed_question() {
        let query = dns_query(0xbeef, "foo.test");
        assert_eq!(
            query,
            [
                0xbe, 0xef, // id
                0x01, 0x00, // flags
                0x00, 0x01, // qdcount
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // an/ns/ar count
                3, b'f', b'o', b'o', 4, b't', b'e', b's', b't', 0, // qname
                0x00, 0x01, // qtype: A
                0x00, 0x01, // qclass: IN
            ]
        );
    }

    #[test]
    fn probe_name_labels_fit_in_a_length_byte() {
        dns_query(0, PROBE_NAME);
    }
}
