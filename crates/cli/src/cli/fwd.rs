use std::net::{IpAddr, Ipv4Addr};

use clap::{Args, Subcommand};
use clap_complete::ArgValueCompleter;
use docker::{FORWARD_LABEL, FORWARD_TARGET_LABEL, PROJECT_LABEL};
use eyre::eyre;

use color_eyre::owo_colors::OwoColorize;

use crate::cli::State;
use crate::complete::complete_workspace;
use crate::config::Config;
use crate::devcontainer::forward_port::ForwardPort;
use crate::state::DevcontainerState;
use crate::workspace::Workspace;

const SOCAT_IMAGE: &str = "docker.io/alpine/socat:latest";

/// Forward configured `forwardPorts` to a running workspace
#[derive(Debug, Args)]
pub(crate) struct Fwd {
    /// Workspace name [default: current working directory]
    #[arg(short, long, add = ArgValueCompleter::new(complete_workspace))]
    workspace: Option<String>,

    #[command(subcommand)]
    command: Option<FwdCommands>,
}

#[derive(Debug, Subcommand)]
enum FwdCommands {
    /// Stop forwarding ports (remove sidecar containers)
    Stop,
}

impl Fwd {
    pub(crate) async fn run(self, project: Option<String>) -> eyre::Result<()> {
        let config = Config::load()?;
        let state = State::new(project, &config).await?;
        match self.command {
            Some(FwdCommands::Stop) => {
                let docker = state.docker().await?;
                remove_sidecars(&state, &docker.client).await
            }
            None => {
                let workspace = state.resolve_workspace(self.workspace).await?;
                let devcontainer = state.devcontainer_for(&workspace.path)?;
                forward(&devcontainer, &workspace).await
            }
        }
    }
}

pub(crate) async fn forward(
    devcontainer: &DevcontainerState,
    workspace: &Workspace<'_>,
) -> eyre::Result<()> {
    remove_sidecars(workspace.state, &devcontainer.docker().await?.client).await?;

    let ws = workspace.devcontainer(devcontainer).await?;
    let cid = ws.service_container_id()?;

    let (ports, warnings) = dedup_forward_ports(&devcontainer.config.forward_ports);
    for warning in warnings {
        tracing::warn!("{warning}");
    }

    if ports.is_empty() {
        return Ok(());
    }

    let free: Vec<bool> = ports.iter().map(|p| port_is_free(p.port)).collect();
    let available: Vec<ForwardPort> = ports
        .iter()
        .zip(&free)
        .filter(|(_, ok)| **ok)
        .map(|(p, _)| p.clone())
        .collect();

    if !available.is_empty() {
        let client = &devcontainer.docker().await?.client;
        if let Err(e) = create_sidecars(client, workspace, cid, &available).await {
            // A port free when we checked can be taken before docker binds it,
            // and the outer sidecar is the last thing we create — so don't
            // leave the inner sidecar and the socket volume running.
            if let Err(cleanup) = remove_sidecars(workspace.state, client).await {
                tracing::warn!("clean up after failed port forwarding: {cleanup}");
            }
            let wanted = available
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(e.wrap_err(format!(
                "failed to forward ports {wanted}; a port that was free when we checked it can \
                 still be taken before docker binds it"
            )));
        }
    }

    for (port, &ok) in ports.iter().zip(&free) {
        if ok {
            eprintln!("{} {port}", "✓".green());
        } else {
            eprintln!("{} {port} (already in use)", "✗".red());
        }
    }

    Ok(())
}

/// Drop entries that would claim a host port another entry already claimed.
fn dedup_forward_ports(ports: &[ForwardPort]) -> (Vec<ForwardPort>, Vec<String>) {
    let mut kept: Vec<ForwardPort> = Vec::with_capacity(ports.len());
    let mut warnings = Vec::new();

    for fwd_port in ports {
        let port = fwd_port.port;
        match kept.iter().find(|k| k.port == port) {
            Some(first) if first == fwd_port => warnings.push(format!(
                "forwardPorts: {fwd_port} is listed more than once; ignoring the duplicate"
            )),
            Some(first) => warnings.push(format!(
                "forwardPorts: host port {port} is already claimed by {first}; ignoring {fwd_port}",
            )),
            None => kept.push(fwd_port.clone()),
        }
    }

    (kept, warnings)
}

/// Create the socket volume and both sidecars, in the order they depend on
/// each other.
async fn create_sidecars(
    client: &docker::Docker,
    workspace: &Workspace<'_>,
    cid: &str,
    ports: &[ForwardPort],
) -> eyre::Result<()> {
    // The outer sidecar needs the target's network name.
    let network_name = container_network(client, cid).await?;
    client.ensure_image(SOCAT_IMAGE).await?;

    let volume_name = shared::container_name(
        "devconcurrent-fwd-vol",
        &[workspace.state.project_name.as_str(), &workspace.name],
    );
    let mut create = client.create_volume(&volume_name);
    for (key, value) in workspace.docker_fwd_labels() {
        create = create.with_label(key.to_owned(), value.to_owned());
    }
    create.call().await?;

    create_inner_sidecar(client, workspace, cid, &volume_name, ports).await?;
    create_outer_sidecar(client, workspace, cid, &network_name, &volume_name, ports).await?;
    Ok(())
}

async fn container_network(client: &docker::Docker, cid: &str) -> eyre::Result<String> {
    let details = client.inspect_container(cid).await?;
    details
        .network_settings
        .networks
        .into_keys()
        .next()
        .ok_or_else(|| eyre!("container {cid} has no networks"))
}

/// Inner sidecar: shares the target container's network namespace.
/// For each port, listens on a Unix socket and connects to 127.0.0.1:<port>.
async fn create_inner_sidecar(
    client: &docker::Docker,
    workspace: &Workspace<'_>,
    cid: &str,
    volume_name: &str,
    ports: &[ForwardPort],
) -> eyre::Result<()> {
    let name = shared::container_name(
        "devconcurrent-fwd-inner",
        &[workspace.state.project_name.as_str(), &workspace.name],
    );

    let socat_cmds: Vec<String> = ports
        .iter()
        .map(|p| {
            // The socket is named after the port alone, which is unique because
            // `dedup_forward_ports` dropped any second claim on it.
            let target = p.service.as_deref().unwrap_or("127.0.0.1");
            format!(
                "socat UNIX-LISTEN:/socks/{}.sock,fork,reuseaddr TCP:{target}:{}",
                p.port, p.port
            )
        })
        .collect();
    let shell_cmd = join_background(&socat_cmds);

    let network_mode = format!("container:{cid}");
    let mut create = client
        .create_container(&name)
        .image(SOCAT_IMAGE)
        .network_mode(&network_mode)
        .entrypoint(vec!["sh".to_string()])
        .cmd(vec!["-c".to_string(), shell_cmd])
        .with_bind(volume_name, "/socks")
        .with_label(FORWARD_TARGET_LABEL, cid);
    for (key, value) in workspace.docker_fwd_labels() {
        create = create.with_label(key, value);
    }
    let id = create.call().await?;
    client.start_container(&id).await?;
    Ok(())
}

/// Outer sidecar: on the Docker network with host port bindings.
/// For each port, listens on TCP and connects via the Unix socket.
async fn create_outer_sidecar(
    client: &docker::Docker,
    workspace: &Workspace<'_>,
    cid: &str,
    network_name: &str,
    volume_name: &str,
    ports: &[ForwardPort],
) -> eyre::Result<()> {
    let name = shared::container_name(
        "devconcurrent-fwd-outer",
        &[workspace.state.project_name.as_str(), &workspace.name],
    );

    let socat_cmds: Vec<String> = ports
        .iter()
        .map(|p| {
            format!(
                "socat TCP-LISTEN:{},fork,reuseaddr UNIX:/socks/{}.sock",
                p.port, p.port
            )
        })
        .collect();
    let shell_cmd = join_background(&socat_cmds);

    let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let mut create = client
        .create_container(&name)
        .image(SOCAT_IMAGE)
        .network_mode(network_name)
        .entrypoint(vec!["sh".to_string()])
        .cmd(vec!["-c".to_string(), shell_cmd])
        .with_bind(volume_name, "/socks")
        .with_label(FORWARD_TARGET_LABEL, cid);
    for (key, value) in workspace.docker_fwd_labels() {
        create = create.with_label(key, value);
    }
    for p in ports {
        create = create.with_tcp_port_binding(p.port, loopback, p.port);
    }
    let id = create.call().await?;
    client.start_container(&id).await?;
    Ok(())
}

/// Build a shell command that runs all socat processes in the background then waits.
fn join_background(cmds: &[String]) -> String {
    let mut parts: Vec<String> = cmds.iter().map(|c| format!("{c} &")).collect();
    parts.push("wait".to_string());
    parts.join(" ")
}

pub(crate) async fn remove_sidecars(
    state: &State<'_>,
    client: &docker::Docker,
) -> eyre::Result<()> {
    let project = state.project_name.as_str();

    let sidecars = client
        .list_containers()
        .all(true)
        .with_label(FORWARD_LABEL, "true")
        .with_label(PROJECT_LABEL, project)
        .call()
        .await?;
    for c in sidecars {
        match client.remove_container(&c.id).force(true).call().await {
            Ok(()) | Err(docker::Error::NotFound) => {}
            Err(e) => tracing::warn!(container = %c.id, "failed to remove sidecar: {e}"),
        }
    }

    let volumes = client
        .list_volumes()
        .with_label(FORWARD_LABEL, "true")
        .with_label(PROJECT_LABEL, project)
        .call()
        .await?;
    for vol in volumes {
        match client.remove_volume(&vol.name).call().await {
            Ok(()) | Err(docker::Error::NotFound) => {}
            Err(e) => tracing::warn!(volume = %vol.name, "failed to remove volume: {e}"),
        }
    }

    Ok(())
}

/// Best-effort check that we can publish `port` on the host — not a guarantee.
///
/// It binds and immediately drops a listener, so anything may take the port
/// between here and docker binding it for real; `forward` handles that failure
/// rather than pretending it cannot happen. `127.0.0.1` is what docker binds,
/// so a port held only on another specific interface does not count as taken.
fn port_is_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(port: u16) -> ForwardPort {
        ForwardPort {
            service: None,
            port,
        }
    }

    fn service_port(service: &str, port: u16) -> ForwardPort {
        ForwardPort {
            service: Some(service.to_string()),
            port,
        }
    }

    #[test]
    fn distinct_ports_pass_through_in_order() {
        let ports = vec![port(3000), service_port("db", 5432), port(8080)];
        let (kept, warnings) = dedup_forward_ports(&ports);
        assert_eq!(kept, ports);
        assert!(warnings.is_empty());
    }

    #[test]
    fn an_exactly_repeated_entry_is_kept_once() {
        let (kept, warnings) = dedup_forward_ports(&[port(3000), port(3000)]);
        assert_eq!(kept, vec![port(3000)]);
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("listed more than once"),
            "unexpected warning: {}",
            warnings[0]
        );
    }

    #[test]
    fn two_services_claiming_one_host_port_keeps_the_first() {
        let (kept, warnings) = dedup_forward_ports(&[port(3000), service_port("db", 3000)]);
        assert_eq!(kept, vec![port(3000)]);
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("already claimed by"),
            "unexpected warning: {}",
            warnings[0]
        );
    }

    #[test]
    fn a_service_mapping_can_be_the_first_claim() {
        let (kept, warnings) = dedup_forward_ports(&[service_port("db", 3000), port(3000)]);
        assert_eq!(kept, vec![service_port("db", 3000)]);
        assert_eq!(warnings.len(), 1);
    }
}
