use std::sync::LazyLock;
use std::time::Duration;

use clap::{Args, Subcommand};
use clap_complete::engine::ArgValueCompleter;
use color_eyre::owo_colors::OwoColorize;
use docker::{
    Docker, PROXY_CA_EXPIRY_LABEL, PROXY_CONFIG_HASH_LABEL, PROXY_GROUP_LABEL, PROXY_LABEL,
};
use eyre::{Result, WrapErr};
use shared::{
    ENV_CA_DIR, ENV_DNS_PORT, PROXY_CA_CERT_FILE, PROXY_CA_DIR, PROXY_CA_KEY_FILE,
    PROXY_CONFIG_DIR, PROXY_CONFIG_VOLUME, PROXY_CONTAINER_NAME,
};

use crate::complete::complete_workspace;
use crate::run::{Runnable, Runner};

mod dns;
pub(crate) mod intermediate;
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

/// Manage the DNS server and HTTP proxy
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
    #[command(visible_alias = "s")]
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
                ensure_enabled(&proxy)?;
                proxy_up(&proxy).await
            }
            ProxyCommands::Status(args) => {
                let proxy = ProxyState::resolve(project, args.workspace()).await?;
                ensure_enabled(&proxy)?;
                status::run(&proxy, &args).await
            }
            ProxyCommands::Down => proxy_down().await,
        }
    }
}

fn ensure_enabled(proxy: &ProxyState) -> Result<()> {
    eyre::ensure!(proxy.config.enable, "the proxy is disabled");
    Ok(())
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
    // Mint before touching anything, so a broken caRoot leaves the old proxy
    // running. The root key is only ever read here, on the host; the container
    // receives an intermediate that can't sign outside the configured TLDs.
    let intermediate = intermediate::mint(&proxy.ca_root, &proxy.config.tlds)
        .wrap_err("mint the intermediate CA (check proxy.caRoot)")?;

    proxy
        .docker
        .ensure_image(&PROXY_IMAGE)
        .await
        .wrap_err_with(|| format!("pull {}", *PROXY_IMAGE))?;

    remove_proxy_group(&proxy.docker).await?;

    let id = create_proxy_stopped(proxy, &intermediate).await?;
    proxy.push_configs().await?;
    push_intermediate(&proxy.docker, &intermediate).await?;
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
            Ok(()) | Err(docker::Error::NotFound { .. }) => {}
            Err(e) => tracing::warn!(id = %c.id, "remove proxy-group container: {e}"),
        }
    }
    Ok(())
}

/// Check that every hostname this workspace's services render to falls under
/// one of `proxy.tlds`. Outside them, DNS is never routed to the proxy and
/// the name-constrained CA cannot vouch for the name, so the misconfiguration
/// would otherwise only surface as a browser error much later. Services whose
/// template fails to render are skipped; the proxy reports those itself.
pub(crate) fn check_hostname_tlds<'a>(
    opts: &shared::ProxyOptions,
    project: &str,
    workspace: &str,
    root: bool,
    services: impl IntoIterator<Item = &'a str>,
    tlds: &[String],
) -> Result<()> {
    for service in services {
        let Some(hostname) = opts.render_hostname(project, workspace, service, root) else {
            continue;
        };
        // dNSName matching is case-insensitive; a TLD covers itself and every
        // subdomain, on label boundaries.
        let lower = hostname.to_ascii_lowercase();
        let covered = tlds.iter().any(|tld| {
            let tld = tld.to_ascii_lowercase();
            lower == tld || lower.ends_with(&format!(".{tld}"))
        });
        if !covered {
            eyre::bail!(
                "\
The hostname {hostname} for service {service} is not included in `proxy.tlds`.
Devconcurrent's DNS will not resolve it, and its CA cannot mint a certificate for it.

Fix the hostname template or add its TLD to proxy.tlds in config.toml"
            );
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
                let hash_ok = d.config.labels.get(PROXY_CONFIG_HASH_LABEL) == Some(&hash);
                // A missing/unparseable label also counts as stale: it means
                // the proxy was created by an older CLI, without an
                // intermediate.
                let ca_fresh = matches!(
                    intermediate::expiry(
                        d.config
                            .labels
                            .get(PROXY_CA_EXPIRY_LABEL)
                            .map(String::as_str),
                        jiff::Timestamp::now(),
                    ),
                    intermediate::Expiry::Valid(_)
                );
                if hash_ok && ca_fresh {
                    State::Up
                } else {
                    State::Old
                }
            } else {
                State::Down
            }
        }
        Err(docker::Error::NotFound { .. }) => State::Down,
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
            Err(docker::Error::NotFound { .. }) => {
                eyre::bail!("proxy container vanished after start")
            }
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

async fn create_proxy_stopped(
    proxy: &ProxyState,
    intermediate: &intermediate::Intermediate,
) -> Result<String> {
    // Not necessarily the socket the CLI itself talks to: on Docker Desktop
    // only the default path is shared into a container.
    let socket_path = proxy.docker.socket_mount_source();

    // The DNS port is published rather than the container sharing the host's
    // network namespace: on macOS and Windows the "host" of a host-networked
    // container is Docker's VM, not the machine the user's resolver runs on.
    let builder = proxy
        .docker
        .create_container(PROXY_CONTAINER_NAME)
        .image(&PROXY_IMAGE)
        .with_udp_port_binding(proxy.config.port, dns::LISTEN_IP, proxy.config.port)
        .with_tcp_port_binding(proxy.config.port, dns::LISTEN_IP, proxy.config.port)
        .with_label(PROXY_LABEL, "true")
        .with_label(PROXY_GROUP_LABEL, "true")
        .with_label(PROXY_CONFIG_HASH_LABEL, proxy.config_hash())
        .with_bind(PROXY_CONFIG_VOLUME, PROXY_CONFIG_DIR)
        .with_bind(socket_path.display(), "/var/run/docker.sock")
        .with_env(ENV_DNS_PORT, proxy.config.port)
        .with_env(ENV_CA_DIR, PROXY_CA_DIR)
        .with_label(PROXY_CA_EXPIRY_LABEL, intermediate.not_after.to_string());

    builder.call().await.wrap_err("create proxy container")
}

/// Upload the intermediate CA into the stopped proxy container's writable
/// layer, where it dies with the container.
async fn push_intermediate(
    docker: &Docker,
    intermediate: &intermediate::Intermediate,
) -> Result<()> {
    let tar = docker::build_archive(&[
        (PROXY_CA_CERT_FILE, intermediate.cert_pem.as_bytes()),
        (PROXY_CA_KEY_FILE, intermediate.key_pem.as_bytes()),
    ])
    .wrap_err("build intermediate CA archive")?;
    docker
        .upload_archive(PROXY_CONTAINER_NAME, PROXY_CA_DIR, tar)
        .await
        .wrap_err("upload intermediate CA")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(value: serde_json::Value) -> shared::ProxyOptions {
        serde_json::from_value(value).expect("valid proxy options")
    }

    fn check(options: &shared::ProxyOptions, tlds: &[&str]) -> Result<()> {
        let tlds: Vec<String> = tlds.iter().map(ToString::to_string).collect();
        check_hostname_tlds(options, "proj", "feature", false, ["app"], &tlds)
    }

    #[test]
    fn a_hostname_outside_the_tlds_is_an_immediate_error() {
        // The default template ends in `.test`, which is not in proxy.tlds.
        let options = opts(serde_json::json!({"enable": true}));
        let err = check(&options, &["dev"]).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("feature.app.test"), "{text}");
        assert!(text.contains("proxy.tlds"), "{text}");
    }

    #[test]
    fn a_hostname_under_a_configured_tld_passes() {
        let options = opts(serde_json::json!({
            "enable": true,
            "hostname": "{{workspace}}.{{service}}.internal.dev",
        }));
        check(&options, &["internal.dev"]).unwrap();
        // Multi-entry tlds: any match suffices.
        check(&options, &["test", "internal.dev"]).unwrap();
    }

    #[test]
    fn tld_matching_is_case_insensitive_and_on_label_boundaries() {
        let options = opts(serde_json::json!({
            "enable": true,
            "hostname": "{{workspace}}.{{service}}.TEST",
        }));
        check(&options, &["test"]).unwrap();

        // `mytest` must not match the `test` TLD by substring.
        let options = opts(serde_json::json!({
            "enable": true,
            "hostname": "{{workspace}}.{{service}}.mytest",
        }));
        assert!(check(&options, &["test"]).is_err());
    }

    #[test]
    fn a_per_service_hostname_is_checked_too() {
        let options = opts(serde_json::json!({
            "enable": true,
            "services": {"app": {"hostname": "app.localhost"}},
        }));
        let err = check(&options, &["test"]).unwrap_err();
        assert!(err.to_string().contains("app.localhost"), "{err}");
    }
}
