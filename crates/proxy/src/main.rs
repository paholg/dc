use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;

use docker::Docker;
use eyre::{Result, WrapErr};
use shared::{ENV_CA_DIR, ENV_DNS_PORT, PROXY_CONFIG_DIR, PROXY_CONFIG_FILE, ProxyOptions};
use tracing::info;

mod certs;
mod dns;
mod events;
mod registry;
mod sidecar;
mod sidecar_mode;

use certs::CaHolder;
use registry::Registry;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // The same binary runs in two modes: proxy (default, no args) and sidecar
    // (`devconcurrent-proxy sidecar`, used by the per-service sidecar
    // containers the proxy creates).
    if std::env::args().nth(1).as_deref() == Some("sidecar") {
        return sidecar_mode::run().await;
    }

    let dns_port: u16 = std::env::var(ENV_DNS_PORT)
        .wrap_err_with(|| format!("{ENV_DNS_PORT} not set"))?
        .parse()
        .wrap_err_with(|| format!("invalid {ENV_DNS_PORT}"))?;

    info!(
        version = env!("CARGO_PKG_VERSION"),
        dns_port, "devconcurrent-proxy starting"
    );

    let ca = if let Ok(dir) = std::env::var(ENV_CA_DIR) {
        match CaHolder::load(Path::new(&dir)) {
            Ok(ca) => {
                info!(dir, "loaded mkcert CA");
                Some(ca)
            }
            Err(e) => {
                tracing::warn!(
                    dir,
                    "failed to load CA: {e:?}; services will be served over http only"
                );
                None
            }
        }
    } else {
        info!("no {ENV_CA_DIR}; services will be served over http only");
        None
    };

    let docker = Docker::connect()
        .await
        .wrap_err("connect to docker daemon")?;

    let registry = Registry::new();
    let configs = read_project_configs(Path::new(PROXY_CONFIG_DIR))?;
    info!(count = configs.len(), "loaded project configs");
    registry.load_configs(configs).await;

    // Adopt any already-running service containers as if they'd just started.
    events::bootstrap(&docker, &registry, ca.as_ref()).await?;
    // Drop sidecars whose targets no longer exist.
    if let Err(e) = sidecar::sweep_orphans(&docker).await {
        tracing::warn!("orphan sweep failed: {e:?}");
    }

    // All interfaces: the port is published by the CLI, which is where the
    // host-side listen address is restricted.
    let dns_bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), dns_port);

    let dns_task = tokio::spawn({
        let registry = registry.clone();
        async move {
            if let Err(e) = dns::serve(dns_bind, registry).await {
                tracing::error!("dns server stopped: {e:?}");
            }
        }
    });
    let events_task = tokio::spawn({
        let docker = docker.clone();
        let registry = registry.clone();
        let ca = ca.clone();
        async move { events::run(docker, registry, ca).await }
    });

    tokio::select! {
        _ = dns_task => tracing::error!("dns task exited"),
        _ = events_task => tracing::error!("events task exited"),
    }
    Ok(())
}

/// Read `<dir>/projects.json` as the merged `HashMap<project_name, ProxyOptions>`
/// pushed by the CLI. Missing file → empty map (proxy comes up with nothing
/// configured, which is fine).
fn read_project_configs(dir: &Path) -> Result<Vec<(String, ProxyOptions)>> {
    let path = dir.join(PROXY_CONFIG_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).wrap_err_with(|| format!("read {}", path.display())),
    };
    let map: std::collections::HashMap<String, ProxyOptions> =
        serde_json::from_slice(&bytes).wrap_err_with(|| format!("parse {}", path.display()))?;
    Ok(map.into_iter().collect())
}
