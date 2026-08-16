use std::sync::LazyLock;
use std::time::Duration;

use clap::{Args, Subcommand};
use clap_complete::engine::ArgValueCompleter;
use color_eyre::owo_colors::OwoColorize;
use docker::{Docker, PROXY_CONFIG_HASH_LABEL, PROXY_GROUP_LABEL, PROXY_LABEL};
use eyre::{Result, WrapErr};
use shared::{
    ENV_CA_DIR, ENV_DNS_PORT, PROXY_CA_DIR, PROXY_CONFIG_DIR, PROXY_CONFIG_VOLUME,
    PROXY_CONTAINER_NAME,
};

use crate::complete::complete_workspace;
use crate::run::{Runnable, Runner};

mod dns;
mod proxy_state;
mod status;
pub(crate) use proxy_state::ProxyState;

/// OCI image used by the proxy container.
const PROXY_IMAGE_NAME: &str = "ghcr.io/paholg/devconcurrent-proxy";

/// We keep the proxy and CLI versions in sync, so using the CLI version here is fine.
const PROXY_IMAGE_TAG: &str = env!("CARGO_PKG_VERSION");

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
    /// Check that every configured hostname and port is reachable
    Status(status::StatusArgs),
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
                let proxy = ProxyState::resolve(project, args.workspace()).await?;
                status::run(&proxy, &args).await
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

    loop {
        // A failure here is just "not up yet": the proxy drops the packet, or
        // docker's forwarder rejects it because nothing is listening behind it.
        if dns::is_answering(port).await {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            eyre::bail!("proxy did not answer a dns query on port {port} within {timeout}s");
        }
        // A failed send returns immediately, so pace the retries ourselves.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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
        .with_udp_port_binding(proxy.config.port, dns::LISTEN_IP, proxy.config.port)
        .with_tcp_port_binding(proxy.config.port, dns::LISTEN_IP, proxy.config.port)
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
