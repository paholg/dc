//! Sidecar mode of the proxy binary. The same image runs in this mode when
//! invoked with the `sidecar` subcommand; the proxy creates these sidecars
//! inside the netns of each compose service that declares a `containerPort`.
//!
//! The plan + optional cert/key are written into `/etc/sidecar/` by the
//! proxy before the container starts. We read them once on boot and spawn
//! one tokio task per listener: 80 always, and 443 when we have a cert. Both
//! forward to the plan's container port.
//!
//! Port 80 uses a raw byte-splice forwarder. Port 443 runs a hyper HTTP/1.1
//! server on the decrypted stream and routes into `axum-reverse-proxy` for the
//! upstream side. A small tower layer adds the `X-Forwarded-Proto`
//! and `X-Forwarded-Host` headers Rails needs to reconstruct `https://…`
//! URLs. `X-Forwarded-For` is deliberately not added — the apparent client
//! inside the netns is the docker bridge gateway, and forwarding that to
//! the app would defeat dev tools (web-console, `ActionCable` origin checks,
//! etc.) that gate on "is this localhost". The app sees the socket peer,
//! which is 127.0.0.1 from rpxy connecting over loopback.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum_reverse_proxy::ReverseProxy;
use eyre::{Context, Result};
use http::HeaderName;
use http::header::HOST;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use shared::{
    HTTP_PORT, HTTPS_PORT, SIDECAR_CERT_FILE, SIDECAR_KEY_FILE, SIDECAR_PLAN_DIR,
    SIDECAR_PLAN_FILE, SidecarPlan,
};
use tokio::io::{AsyncWriteExt, copy_bidirectional};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tower_http::set_header::SetRequestHeaderLayer;
use tracing::info;

/// Entry point for sidecar mode.
pub async fn run() -> Result<()> {
    let dir = PathBuf::from(SIDECAR_PLAN_DIR);
    let plan_path = dir.join(SIDECAR_PLAN_FILE);
    let plan_bytes =
        std::fs::read(&plan_path).wrap_err_with(|| format!("read {}", plan_path.display()))?;
    let plan: SidecarPlan = serde_json::from_slice(&plan_bytes).wrap_err("parse sidecar plan")?;
    info!(
        hostname = %plan.hostname,
        port = plan.port,
        "sidecar starting"
    );

    let mut tasks = vec![tokio::spawn(serve_plain(HTTP_PORT, plan.port))];

    match load_tls(&dir, &plan.hostname) {
        Some(acceptor) => tasks.push(tokio::spawn(serve_tls(HTTPS_PORT, plan.port, acceptor))),
        None => tracing::warn!(
            hostname = %plan.hostname,
            "no usable cert: serving http only"
        ),
    }

    // If any listener task exits, the sidecar exits — the proxy/docker
    // lifecycle will recreate it.
    let (result, _idx, _rest) = futures_util::future::select_all(tasks).await;
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(e) => Err(eyre::eyre!("listener task panicked: {e}")),
    }
}

fn load_tls(dir: &Path, hostname: &str) -> Option<TlsAcceptor> {
    let cert_path = dir.join(SIDECAR_CERT_FILE);
    let key_path = dir.join(SIDECAR_KEY_FILE);
    if !cert_path.exists() || !key_path.exists() {
        return None;
    }
    let certs: Vec<CertificateDer<'static>> = match CertificateDer::pem_file_iter(&cert_path)
        .and_then(std::iter::Iterator::collect::<Result<_, _>>)
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(hostname, path = %cert_path.display(), "load cert: {e}");
            return None;
        }
    };
    let key: PrivateKeyDer<'static> = match PrivateKeyDer::from_pem_file(&key_path) {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!(hostname, path = %key_path.display(), "load key: {e}");
            return None;
        }
    };
    let config = match ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(hostname, "build rustls config: {e}");
            return None;
        }
    };
    Some(TlsAcceptor::from(Arc::new(config)))
}

async fn serve_plain(host: u16, container: u16) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", host))
        .await
        .wrap_err_with(|| format!("bind 0.0.0.0:{host}"))?;
    info!(host, container, "plain listener ready");
    loop {
        let (mut inbound, peer) = match listener.accept().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(host, "accept: {e}");
                continue;
            }
        };
        tokio::spawn(async move {
            let mut outbound = match TcpStream::connect(("127.0.0.1", container)).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!(container, peer = %peer, "upstream connect: {e}");
                    let _ = inbound.shutdown().await;
                    return;
                }
            };
            let _ = copy_bidirectional(&mut inbound, &mut outbound).await;
        });
    }
}

async fn serve_tls(host: u16, container: u16, acceptor: TlsAcceptor) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", host))
        .await
        .wrap_err_with(|| format!("bind 0.0.0.0:{host}"))?;
    info!(host, container, "tls listener ready");

    let app = build_router(container);

    loop {
        let (raw, peer) = match listener.accept().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(host, "accept: {e}");
                continue;
            }
        };
        let acceptor = acceptor.clone();
        let app = app.clone();
        tokio::spawn(async move {
            let stream = match acceptor.accept(raw).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!(host, peer = %peer, "tls handshake: {e}");
                    return;
                }
            };
            let svc = TowerToHyperService::new(app);
            if let Err(e) = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), svc)
                .with_upgrades()
                .await
            {
                tracing::debug!(host, peer = %peer, "http1 conn: {e}");
            }
        });
    }
}

/// Build the reverse-proxy router. Layers added:
/// - `X-Forwarded-Proto: https` — always.
/// - `X-Forwarded-Host: <inbound Host>` — preserves the user-facing hostname
///   so the app can reconstruct correct absolute URLs and redirects.
///
/// `X-Forwarded-For` is intentionally **not** set: the apparent client
/// inside the netns is the docker bridge gateway, which is misleading; with
/// the header absent, the app falls back to the socket peer (127.0.0.1
/// from our loopback upstream connect), making the request look like it
/// originated on the same machine.
fn build_router(container: u16) -> Router {
    let upstream = format!("http://127.0.0.1:{container}");
    let proxy = ReverseProxy::new("/", &upstream);
    let router: Router = proxy.into();
    router
        .layer(SetRequestHeaderLayer::overriding(
            HeaderName::from_static("x-forwarded-proto"),
            http::HeaderValue::from_static("https"),
        ))
        .layer(SetRequestHeaderLayer::overriding(
            HeaderName::from_static("x-forwarded-host"),
            |req: &http::Request<_>| req.headers().get(HOST).cloned(),
        ))
}
