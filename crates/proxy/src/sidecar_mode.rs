//! Sidecar mode of the proxy binary. The same image runs in this mode when
//! invoked with the `sidecar` subcommand; the proxy creates these sidecars
//! inside the netns of each compose service that declares a `containerPort`.
//!
//! The plan + optional cert/key are written into `/etc/sidecar/` by the
//! proxy before the container starts. We read them once on boot and spawn
//! one tokio task per listener: 80 always, and 443 when we have a cert. Both
//! forward to the plan's container port.
//!
//! Port 80 redirects browser navigations to https and byte-splices everything
//! else through to the container port. Port 443 runs a hyper HTTP/1.1 server on
//! the decrypted stream and routes into `axum-reverse-proxy` for the upstream
//! side. A small tower layer adds the `X-Forwarded-Proto`
//! and `X-Forwarded-Host` headers Rails needs to reconstruct `https://…`
//! URLs. `X-Forwarded-For` is deliberately not added — the apparent client
//! inside the netns is the docker bridge gateway, and forwarding that to
//! the app would defeat dev tools (web-console, `ActionCable` origin checks,
//! etc.) that gate on "is this localhost". The app sees the socket peer,
//! which is 127.0.0.1 from rpxy connecting over loopback.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

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

    // Both listeners are bound here rather than inside their tasks, so that
    // port 80 never starts redirecting to an https listener that doesn't exist.
    let http = bind(HTTP_PORT).await?;
    let https = match load_tls(&dir, &plan.hostname) {
        Some(acceptor) => Some((bind(HTTPS_PORT).await?, acceptor)),
        None => {
            tracing::warn!(
                hostname = %plan.hostname,
                "no usable cert: serving http only, with no redirect to https"
            );
            None
        }
    };

    let mut tasks = vec![tokio::spawn(serve_plain(http, plan.port, https.is_some()))];
    if let Some((listener, acceptor)) = https {
        tasks.push(tokio::spawn(serve_tls(listener, plan.port, acceptor)));
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

async fn bind(port: u16) -> Result<TcpListener> {
    TcpListener::bind(("0.0.0.0", port))
        .await
        .wrap_err_with(|| format!("bind 0.0.0.0:{port}"))
}

/// Port 80. Browser navigations get bounced to https; everything else is
/// spliced through to the container port untouched.
async fn serve_plain(listener: TcpListener, container: u16, redirect: bool) -> Result<()> {
    info!(
        host = HTTP_PORT,
        container, redirect, "plain listener ready"
    );
    loop {
        let (mut inbound, peer) = match listener.accept().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(host = HTTP_PORT, "accept: {e}");
                continue;
            }
        };
        tokio::spawn(async move {
            if redirect && let Some(location) = redirect_target(&inbound).await {
                tracing::debug!(peer = %peer, %location, "redirecting to https");
                send_redirect(&mut inbound, &location).await;
                return;
            }
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

async fn serve_tls(listener: TcpListener, container: u16, acceptor: TlsAcceptor) -> Result<()> {
    let host = HTTPS_PORT;
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

/// Most of a request head; anything longer than this we don't need to see to
/// make the decision.
const PEEK_LIMIT: usize = 8192;

/// How long to wait for a client to say something before giving up and
/// treating the connection as opaque.
const PEEK_TIMEOUT: Duration = Duration::from_millis(250);

/// Where this request should be redirected to, or `None` to splice it through
/// untouched.
///
/// This peeks rather than reads, so the splice path gets a stream with nothing
/// consumed from it.
async fn redirect_target(stream: &TcpStream) -> Option<String> {
    let mut buf = [0u8; PEEK_LIMIT];
    let n = tokio::time::timeout(PEEK_TIMEOUT, stream.peek(&mut buf))
        .await
        .ok()?
        .ok()?;
    redirect_target_for(&buf[..n])
}

/// The decision itself, over whatever we managed to peek at.
///
/// Only browser navigations are redirected. Everything else — `fetch`/`XHR`,
/// container-to-container calls, health checks, webhooks, anything that isn't
/// HTTP at all — keeps working over plain http, because those clients have no
/// reason to trust our CA and a 307 would just break them.
///
/// Every uncertain case falls through to `None`: a partial head, an
/// unparseable one, or anything not recognisably a navigation.
fn redirect_target_for(head: &[u8]) -> Option<String> {
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut req = httparse::Request::new(&mut headers);
    // A head we can't fully see yet is one we don't get to judge.
    if !matches!(req.parse(head), Ok(httparse::Status::Complete(_))) {
        return None;
    }

    // A navigation is a GET or HEAD; anything else is an API call whatever the
    // other headers say.
    if !matches!(req.method, Some("GET" | "HEAD")) {
        return None;
    }

    let header = |name: &str| {
        req.headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .and_then(|h| std::str::from_utf8(h.value).ok())
    };

    let navigating = match header("sec-fetch-mode") {
        // Every current browser sends this, and says exactly what it's doing.
        Some(mode) => mode.eq_ignore_ascii_case("navigate"),
        // Anything older: the closest available signal is asking for a page.
        None => header("accept").is_some_and(|accept| accept.contains("text/html")),
    };
    if !navigating {
        return None;
    }

    // The name the user typed, minus the `:80` they didn't. The https listener
    // is on 443, so the redirect never needs a port of its own.
    let host = header("host")?.split(':').next()?;
    if host.is_empty() {
        return None;
    }
    Some(format!("https://{host}{}", req.path.unwrap_or("/")))
}

/// 307 rather than 301: a permanent redirect would be cached against this
/// hostname more or less forever, which is miserable the first time someone
/// turns TLS off. 307 also preserves the method, though only GET and HEAD
/// reach here.
async fn send_redirect(stream: &mut TcpStream, location: &str) {
    let response = format!(
        "HTTP/1.1 307 Temporary Redirect\r\n\
         Location: {location}\r\n\
         Content-Length: 0\r\n\
         Connection: close\r\n\
         \r\n"
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A request head, with `\n` in the source standing in for the `\r\n` a
    /// real client sends.
    fn head(lines: &[&str]) -> Vec<u8> {
        format!("{}\r\n\r\n", lines.join("\r\n")).into_bytes()
    }

    const NAVIGATION: &[&str] = &[
        "GET /orders?id=7 HTTP/1.1",
        "Host: app.proj.test",
        "Sec-Fetch-Mode: navigate",
        "Accept: text/html",
    ];

    #[test]
    fn redirects_a_browser_navigation_keeping_the_path_and_query() {
        assert_eq!(
            redirect_target_for(&head(NAVIGATION)).as_deref(),
            Some("https://app.proj.test/orders?id=7"),
        );
    }

    /// The client asked for `:80` explicitly; the https listener is on 443, so
    /// carrying the port over would send them somewhere that isn't listening.
    #[test]
    fn drops_the_port_from_the_host_header() {
        let with_port = [
            "GET / HTTP/1.1",
            "Host: app.proj.test:80",
            "Sec-Fetch-Mode: navigate",
        ];
        assert_eq!(
            redirect_target_for(&head(&with_port)).as_deref(),
            Some("https://app.proj.test/"),
        );
    }

    /// The whole point of the navigation gate: these clients don't have our CA.
    #[test]
    fn leaves_non_navigations_alone() {
        let cases: [(&str, &[&str]); 4] = [
            (
                "a fetch/XHR from a page",
                &[
                    "GET /api/orders HTTP/1.1",
                    "Host: app.proj.test",
                    "Sec-Fetch-Mode: cors",
                ],
            ),
            (
                "a subresource load",
                &[
                    "GET /app.css HTTP/1.1",
                    "Host: app.proj.test",
                    "Sec-Fetch-Mode: no-cors",
                ],
            ),
            (
                "a webhook or API POST",
                &[
                    "POST /hooks HTTP/1.1",
                    "Host: app.proj.test",
                    "Sec-Fetch-Mode: navigate",
                ],
            ),
            (
                "curl, or a health check",
                &["GET / HTTP/1.1", "Host: app.proj.test", "Accept: */*"],
            ),
        ];
        for (what, lines) in cases {
            assert_eq!(redirect_target_for(&head(lines)), None, "{what}");
        }
    }

    /// No `Sec-Fetch-Mode` at all: fall back to whether it wants a page.
    #[test]
    fn falls_back_to_accept_for_clients_without_sec_fetch_mode() {
        let old_browser = [
            "GET / HTTP/1.1",
            "Host: app.proj.test",
            "Accept: text/html,application/xhtml+xml,*/*;q=0.8",
        ];
        assert_eq!(
            redirect_target_for(&head(&old_browser)).as_deref(),
            Some("https://app.proj.test/"),
        );
    }

    /// Anything we can't positively identify keeps the behaviour it had before
    /// the redirect existed.
    #[test]
    fn splices_anything_it_cannot_read() {
        let partial = b"GET / HTTP/1.1\r\nHost: app.proj.test\r\n".as_slice();
        let not_http = b"\x16\x03\x01\x02\x00\x01\x00".as_slice();
        let no_host = head(&["GET / HTTP/1.1", "Sec-Fetch-Mode: navigate"]);
        let empty_host = head(&["GET / HTTP/1.1", "Host:", "Sec-Fetch-Mode: navigate"]);

        for (what, bytes) in [
            ("a head that hasn't finished arriving", partial),
            ("a TLS ClientHello on the wrong port", not_http),
            ("a request with no Host", &no_host),
            ("a request with an empty Host", &empty_host),
        ] {
            assert_eq!(redirect_target_for(bytes), None, "{what}");
        }
    }

    /// Everything above tests the decision; these two test the plumbing around
    /// it, over real sockets.
    mod over_a_socket {
        use super::*;
        use tokio::io::AsyncReadExt;

        async fn serve(container: u16) -> u16 {
            let listener = bind(0).await.expect("bind an ephemeral port");
            let port = listener.local_addr().expect("local addr").port();
            tokio::spawn(serve_plain(listener, container, true));
            port
        }

        async fn send(port: u16, request: &[u8]) -> TcpStream {
            let mut client = TcpStream::connect(("127.0.0.1", port))
                .await
                .expect("connect to the listener");
            client.write_all(request).await.expect("write the request");
            client
        }

        #[tokio::test]
        async fn a_navigation_gets_a_307_to_https() {
            // Nothing is listening on the container port: a redirect must not
            // depend on the upstream being reachable.
            let port = serve(1).await;
            let mut client = send(port, &head(NAVIGATION)).await;

            let mut response = String::new();
            client
                .read_to_string(&mut response)
                .await
                .expect("read the response");

            assert!(
                response.starts_with("HTTP/1.1 307 Temporary Redirect\r\n"),
                "got: {response}"
            );
            assert!(
                response.contains("Location: https://app.proj.test/orders?id=7\r\n"),
                "got: {response}"
            );
        }

        /// The whole splice path rests on `peek` leaving the stream alone, so
        /// check the upstream receives the request byte for byte.
        #[tokio::test]
        async fn a_non_navigation_reaches_the_upstream_intact() {
            let upstream = bind(0).await.expect("bind an ephemeral upstream");
            let upstream_port = upstream.local_addr().expect("local addr").port();

            let request = head(&["GET /api HTTP/1.1", "Host: app.proj.test", "Accept: */*"]);
            let expected = request.clone();

            let received = tokio::spawn(async move {
                let (mut conn, _) = upstream.accept().await.expect("accept upstream");
                let mut buf = vec![0u8; expected.len()];
                conn.read_exact(&mut buf).await.expect("read the request");
                buf
            });

            let port = serve(upstream_port).await;
            let _client = send(port, &request).await;

            assert_eq!(received.await.expect("upstream task"), request);
        }
    }

    #[test]
    fn header_matching_ignores_case() {
        let shouty = [
            "GET / HTTP/1.1",
            "HOST: app.proj.test",
            "SEC-FETCH-MODE: NAVIGATE",
        ];
        assert_eq!(
            redirect_target_for(&head(&shouty)).as_deref(),
            Some("https://app.proj.test/"),
        );
    }
}
