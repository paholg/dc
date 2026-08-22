//! Sidecar mode of the proxy binary. The same image runs in this mode when
//! invoked with the `sidecar` subcommand; the proxy creates these sidecars
//! inside the netns of each compose service that declares a `containerPort`.
//!
//! The plan + optional cert/key are written into `/etc/sidecar/` by the
//! proxy before the container starts. We read them once on boot and spawn
//! one tokio task per listener: 80 always, and 443 when we have a cert.
//!
//! Both listeners are HTTP/1.1 reverse proxies onto the container port over
//! loopback, and both forward the client's `Host` untouched, so the app sees
//! the name the user typed rather than `127.0.0.1`. That is what Caddy,
//! Traefik, Envoy and HAProxy all do by default; nginx is the one that
//! replaces it, and `proxy_set_header Host $host` is the first line of every
//! nginx reverse-proxy snippet because of it.
//!
//! Port 80 additionally bounces browser navigations to https — but only when
//! there is an https listener to bounce them to.
//!
//! Small tower layers add the `X-Forwarded-Proto` and `X-Forwarded-Host`
//! headers, and on 443 an `upgrade-insecure-requests` CSP so that `http://`
//! URLs the app emits get fetched over https rather than blocked as mixed
//! content.
//!
//! `X-Forwarded-For` is deliberately not added — the apparent client inside
//! the netns is the docker bridge gateway, and forwarding that to the app
//! would defeat dev tools (web-console, `ActionCable` origin checks, etc.)
//! that gate on "is this localhost". The app sees the socket peer, which is
//! 127.0.0.1 because both listeners reach the container over loopback.
//!
//! Only HTTP/1.1 lives on these two ports. Anything else a service wants to
//! speak belongs on one of its other ports, which are forwarded raw.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_reverse_proxy::{HostBehaviour, ProxyPolicy, ReverseProxy};
use eyre::{Context, Result};
use http::header::{HOST, LOCATION};
use http::{HeaderMap, HeaderName, HeaderValue, Method};
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use shared::{
    HTTP_PORT, HTTPS_PORT, SIDECAR_CERT_FILE, SIDECAR_KEY_FILE, SIDECAR_PLAN_DIR,
    SIDECAR_PLAN_FILE, SidecarPlan, navigation,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tower_http::set_header::{SetRequestHeaderLayer, SetResponseHeaderLayer};
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

/// Which listener a router is being built for. They differ in the scheme they
/// announce upstream, whether responses ask the browser to upgrade the URLs
/// the app emits, and whether navigations are bounced to https.
#[derive(Clone, Copy, Debug)]
enum Listener {
    /// Port 80. `redirect` is set when there is an https listener to send
    /// browsers to.
    Http { redirect: bool },
    /// Port 443.
    Https,
}

impl Listener {
    /// What the app should believe the request arrived over.
    fn proto(self) -> &'static str {
        match self {
            Listener::Http { .. } => "http",
            Listener::Https => "https",
        }
    }
}

/// Port 80. A reverse proxy like 443, plus the redirect that gets browsers
/// onto https in the first place.
async fn serve_plain(listener: TcpListener, container: u16, redirect: bool) -> Result<()> {
    info!(
        host = HTTP_PORT,
        container, redirect, "plain listener ready"
    );
    let app = build_router(container, Listener::Http { redirect });

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(host = HTTP_PORT, "accept: {e}");
                continue;
            }
        };
        let app = app.clone();
        tokio::spawn(serve_conn(stream, app, HTTP_PORT, peer));
    }
}

async fn serve_tls(listener: TcpListener, container: u16, acceptor: TlsAcceptor) -> Result<()> {
    let host = HTTPS_PORT;
    info!(host, container, "tls listener ready");

    let app = build_router(container, Listener::Https);

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
            serve_conn(stream, app, host, peer).await;
        });
    }
}

/// One connection, on either listener. `with_upgrades` so that websockets
/// survive the hand-off to the upstream side.
async fn serve_conn<I>(io: I, app: Router, host: u16, peer: SocketAddr)
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let svc = TowerToHyperService::new(app);
    if let Err(e) = hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(io), svc)
        .with_upgrades()
        .await
    {
        tracing::debug!(host, peer = %peer, "http1 conn: {e}");
    }
}

/// Build the router for `listener`, forwarding to the container port over
/// loopback.
///
/// The client's `Host` is forwarded as-is: the hostname is the whole point of
/// the proxy, and an app that reads the header directly — most of them, Rails
/// being the notable exception — has no other way to learn it.
///
/// Request layers:
/// - `X-Forwarded-Proto` — the scheme the client actually used.
/// - `X-Forwarded-Host: <inbound Host>` — the same value as `Host` now, kept
///   for apps that already read it.
///
/// On 443 one response layer: `Content-Security-Policy:
/// upgrade-insecure-requests`, which rewrites any `http://` asset or link the
/// app emits into `https://` before the browser requests it. The port-80
/// redirect can't help there — browsers block mixed active content on an https
/// page without ever making the request — so an app with hardcoded `http://`
/// URLs needs this instead.
fn build_router(container: u16, listener: Listener) -> Router {
    let upstream = format!("http://127.0.0.1:{container}");
    let proxy = ReverseProxy::new("/", &upstream).with_policy(ProxyPolicy {
        host_behaviour: HostBehaviour::Preserve,
    });
    let router: Router = proxy.into();
    let router = router
        .layer(SetRequestHeaderLayer::overriding(
            HeaderName::from_static("x-forwarded-proto"),
            HeaderValue::from_static(listener.proto()),
        ))
        .layer(SetRequestHeaderLayer::overriding(
            HeaderName::from_static("x-forwarded-host"),
            |req: &http::Request<_>| req.headers().get(HOST).cloned(),
        ));

    match listener {
        // Appending rather than overriding: an app that sets its own CSP keeps
        // it, and the browser enforces both.
        Listener::Https => router.layer(SetResponseHeaderLayer::appending(
            HeaderName::from_static("content-security-policy"),
            HeaderValue::from_static("upgrade-insecure-requests"),
        )),
        Listener::Http { redirect: true } => {
            router.layer(axum::middleware::from_fn(redirect_navigations))
        }
        Listener::Http { redirect: false } => router,
    }
}

/// Bounce browser navigations to https; proxy everything else.
async fn redirect_navigations(request: Request, next: Next) -> Response {
    match https_target(&request) {
        Some(location) => {
            tracing::debug!(%location, "redirecting to https");
            (navigation::REDIRECT_STATUS, [(LOCATION, location)]).into_response()
        }
        None => next.run(request).await,
    }
}

/// Where this request should be redirected to, or `None` to proxy it through.
///
/// Only browser navigations are redirected. Everything else — `fetch`/`XHR`,
/// container-to-container calls, health checks, webhooks — keeps working over
/// plain http, because those clients have no reason to trust our CA and a 307
/// would just break them.
fn https_target(request: &Request) -> Option<String> {
    if !is_navigation(request.method(), request.headers()) {
        return None;
    }

    // The name the user typed, minus the `:80` they didn't. The https listener
    // is on 443, so the redirect never needs a port of its own.
    let host = request.headers().get(HOST)?.to_str().ok()?;
    let host = host.split(':').next()?;
    if host.is_empty() {
        return None;
    }

    let path = request.uri().path_and_query().map_or("/", |pq| pq.as_str());
    Some(format!("https://{host}{path}"))
}

/// Whether this is a user going to a page, rather than something a page (or a
/// script, or another container) issued on its own.
fn is_navigation(method: &Method, headers: &HeaderMap) -> bool {
    // A navigation is a GET or HEAD; anything else is an API call whatever the
    // other headers say.
    if !matches!(*method, Method::GET | Method::HEAD) {
        return false;
    }

    let header = |name: &HeaderName| headers.get(name).and_then(|v| v.to_str().ok());
    match header(&navigation::MODE_HEADER) {
        Some(mode) => mode.eq_ignore_ascii_case(navigation::MODE),
        // For clients too old to send it, the closest signal is asking for a
        // page.
        None => header(&navigation::ACCEPT_HEADER)
            .is_some_and(|accept| accept.contains(navigation::ACCEPT)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::Body;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tower::ServiceExt;

    const NAVIGATION: &[(&str, &str)] = &[
        ("host", "app.proj.test"),
        ("sec-fetch-mode", "navigate"),
        ("accept", "text/html"),
    ];

    /// An upstream that answers 200 and reports back what it was asked.
    async fn stub_upstream() -> (u16, tokio::sync::oneshot::Receiver<Vec<u8>>) {
        let listener = bind(0).await.expect("bind a stub upstream");
        let port = listener.local_addr().expect("local addr").port();
        let (tx, rx) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.expect("accept");
            let mut request = vec![0u8; 1024];
            let n = conn.read(&mut request).await.expect("read the request");
            request.truncate(n);
            conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .expect("write the response");
            let _ = tx.send(request);
        });

        (port, rx)
    }

    /// Drive one request through a router for `listener`. The receiver only
    /// resolves for requests that were proxied rather than answered here.
    async fn send(
        listener: Listener,
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
    ) -> (
        http::Response<Body>,
        tokio::sync::oneshot::Receiver<Vec<u8>>,
    ) {
        let (port, upstream) = stub_upstream().await;
        let mut request = http::Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let request = request.body(Body::empty()).expect("build the request");

        let response = build_router(port, listener)
            .oneshot(request)
            .await
            .expect("the router answers");
        (response, upstream)
    }

    /// A navigation on port 80, with an https listener to send it to.
    async fn navigate(uri: &str, headers: &[(&str, &str)]) -> http::Response<Body> {
        send(Listener::Http { redirect: true }, Method::GET, uri, headers)
            .await
            .0
    }

    fn location(response: &http::Response<Body>) -> Option<&str> {
        response.headers().get(LOCATION)?.to_str().ok()
    }

    /// Whether a request head carries exactly this header line. Anchored to
    /// the whole line: `host: app.proj.test` is a substring of
    /// `x-forwarded-host: app.proj.test`, so `contains` would pass either way.
    fn has_line(head: &str, line: &str) -> bool {
        head.lines().any(|l| l == line)
    }

    /// Both this matcher and the request `dc proxy status` probes with are
    /// built from `shared::navigation`, so they agree by construction. This
    /// checks the constants still describe a request the redirect matches.
    #[tokio::test]
    async fn redirects_a_request_built_from_the_shared_constants() {
        let accept_header = navigation::ACCEPT_HEADER;
        let mode_header = navigation::MODE_HEADER;
        let probe = [
            ("host", "app.proj.test"),
            (accept_header.as_str(), navigation::ACCEPT),
            (mode_header.as_str(), navigation::MODE),
        ];
        let response = navigate("/", &probe).await;

        assert_eq!(response.status(), navigation::REDIRECT_STATUS);
        assert_eq!(location(&response), Some("https://app.proj.test/"));
    }

    #[tokio::test]
    async fn redirects_a_browser_navigation_keeping_the_path_and_query() {
        let response = navigate("/orders?id=7", NAVIGATION).await;

        assert_eq!(response.status(), navigation::REDIRECT_STATUS);
        assert_eq!(
            location(&response),
            Some("https://app.proj.test/orders?id=7"),
        );
    }

    /// The client asked for `:80` explicitly; the https listener is on 443, so
    /// carrying the port over would send them somewhere that isn't listening.
    #[tokio::test]
    async fn drops_the_port_from_the_host_header() {
        let response = navigate(
            "/",
            &[("host", "app.proj.test:80"), ("sec-fetch-mode", "navigate")],
        )
        .await;

        assert_eq!(location(&response), Some("https://app.proj.test/"));
    }

    /// No `Sec-Fetch-Mode` at all: fall back to whether it wants a page.
    #[tokio::test]
    async fn falls_back_to_accept_for_clients_without_sec_fetch_mode() {
        let old_browser = [
            ("host", "app.proj.test"),
            ("accept", "text/html,application/xhtml+xml,*/*;q=0.8"),
        ];
        let response = navigate("/", &old_browser).await;

        assert_eq!(location(&response), Some("https://app.proj.test/"));
    }

    #[tokio::test]
    async fn header_matching_ignores_case() {
        let shouty = [("HOST", "app.proj.test"), ("SEC-FETCH-MODE", "NAVIGATE")];
        let response = navigate("/", &shouty).await;

        assert_eq!(location(&response), Some("https://app.proj.test/"));
    }

    /// The whole point of the navigation gate: these clients don't have our CA.
    #[tokio::test]
    async fn proxies_everything_that_is_not_a_navigation() {
        /// What the request is, its method, and its headers.
        type Case<'a> = (&'a str, Method, &'a [(&'a str, &'a str)]);

        let cases: [Case; 5] = [
            (
                "a fetch/XHR from a page",
                Method::GET,
                &[("host", "app.proj.test"), ("sec-fetch-mode", "cors")],
            ),
            (
                "a subresource load",
                Method::GET,
                &[("host", "app.proj.test"), ("sec-fetch-mode", "no-cors")],
            ),
            (
                "a webhook or API POST",
                Method::POST,
                &[("host", "app.proj.test"), ("sec-fetch-mode", "navigate")],
            ),
            (
                "curl, or a health check",
                Method::GET,
                &[("host", "app.proj.test"), ("accept", "*/*")],
            ),
            (
                "a request with no Host to redirect to",
                Method::GET,
                &[("sec-fetch-mode", "navigate")],
            ),
        ];

        for (what, method, headers) in cases {
            let (response, upstream) =
                send(Listener::Http { redirect: true }, method, "/", headers).await;

            assert_eq!(response.status(), 200, "{what}");
            upstream.await.unwrap_or_else(|_| panic!("{what}"));
        }
    }

    /// Nothing to upgrade to: a browser landing on http has to be served.
    #[tokio::test]
    async fn serves_navigations_directly_when_there_is_no_https_listener() {
        let (response, upstream) = send(
            Listener::Http { redirect: false },
            Method::GET,
            "/",
            NAVIGATION,
        )
        .await;

        assert_eq!(response.status(), 200);
        upstream.await.expect("the upstream was asked");
    }

    /// What the app ends up seeing, driven against a stub upstream.
    mod upstream {
        use super::*;

        async fn seen(listener: Listener) -> String {
            let (_, upstream) = send(
                listener,
                Method::GET,
                "/",
                &[("host", "app.proj.test"), ("accept", "*/*")],
            )
            .await;
            let seen = upstream.await.expect("the upstream was asked");
            String::from_utf8_lossy(&seen).to_lowercase()
        }

        /// The app is reached over loopback, so without this it would see
        /// `127.0.0.1` as its own hostname and build URLs nobody can follow.
        #[tokio::test]
        async fn the_hostname_the_client_asked_for_survives_the_proxy() {
            for listener in [Listener::Http { redirect: true }, Listener::Https] {
                let seen = seen(listener).await;
                assert!(
                    has_line(&seen, "host: app.proj.test"),
                    "no client host in: {seen}"
                );
                assert!(
                    !seen.contains("127.0.0.1"),
                    "upstream authority leaked in: {seen}"
                );
            }
        }

        #[tokio::test]
        async fn the_upstream_is_told_which_scheme_the_request_arrived_over() {
            let over_tls = seen(Listener::Https).await;
            assert!(
                has_line(&over_tls, "x-forwarded-proto: https"),
                "no forwarded proto in: {over_tls}"
            );
            assert!(
                has_line(&over_tls, "x-forwarded-host: app.proj.test"),
                "no forwarded host in: {over_tls}"
            );

            let plain = seen(Listener::Http { redirect: true }).await;
            assert!(
                has_line(&plain, "x-forwarded-proto: http"),
                "no forwarded proto in: {plain}"
            );
        }

        /// An app that emits `http://` asset URLs would otherwise have them
        /// blocked as mixed content on an https page. There is nothing to
        /// upgrade to on port 80, so it says nothing.
        #[tokio::test]
        async fn only_https_responses_carry_the_upgrade_insecure_requests_policy() {
            for (listener, expected) in [
                (Listener::Https, Some("upgrade-insecure-requests")),
                (Listener::Http { redirect: true }, None),
            ] {
                let (response, _) = send(
                    listener,
                    Method::GET,
                    "/",
                    &[("host", "app.proj.test"), ("accept", "*/*")],
                )
                .await;

                assert_eq!(
                    response
                        .headers()
                        .get("content-security-policy")
                        .and_then(|v| v.to_str().ok()),
                    expected,
                    "{listener:?}",
                );
            }
        }
    }

    /// Everything above tests the decision; these two test the plumbing around
    /// it, over real sockets.
    mod over_a_socket {
        use super::*;

        async fn serve(container: u16) -> u16 {
            let listener = bind(0).await.expect("bind an ephemeral port");
            let port = listener.local_addr().expect("local addr").port();
            tokio::spawn(serve_plain(listener, container, true));
            port
        }

        /// `Connection: close`, so reading to EOF is enough to have the whole
        /// response.
        fn head(lines: &[&str]) -> Vec<u8> {
            format!("{}\r\nConnection: close\r\n\r\n", lines.join("\r\n")).into_bytes()
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
            let request = head(&[
                "GET /orders?id=7 HTTP/1.1",
                "Host: app.proj.test",
                "Sec-Fetch-Mode: navigate",
            ]);
            let mut client = send(port, &request).await;

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
                response.contains("location: https://app.proj.test/orders?id=7\r\n"),
                "got: {response}"
            );
        }

        /// `dc proxy status` reads this status to say "nothing is serving on
        /// the container port", so it has to be the proxy's answer rather than
        /// the hang-up the byte splice used to produce.
        #[tokio::test]
        async fn a_dead_container_port_answers_502() {
            let port = serve(1).await;
            let request = head(&["GET /api HTTP/1.1", "Host: app.proj.test", "Accept: */*"]);
            let mut client = send(port, &request).await;

            let mut response = String::new();
            client
                .read_to_string(&mut response)
                .await
                .expect("read the response");

            assert!(response.starts_with("HTTP/1.1 502 "), "got: {response}");
        }

        #[tokio::test]
        async fn a_non_navigation_reaches_the_upstream_with_its_host() {
            let upstream = bind(0).await.expect("bind an ephemeral upstream");
            let upstream_port = upstream.local_addr().expect("local addr").port();

            let received = tokio::spawn(async move {
                let (mut conn, _) = upstream.accept().await.expect("accept upstream");
                let mut buf = vec![0u8; 1024];
                let n = conn.read(&mut buf).await.expect("read the request");
                buf.truncate(n);
                String::from_utf8_lossy(&buf).to_lowercase()
            });

            let port = serve(upstream_port).await;
            let request = head(&["GET /api HTTP/1.1", "Host: app.proj.test", "Accept: */*"]);
            let _client = send(port, &request).await;

            let seen = received.await.expect("upstream task");
            assert!(seen.starts_with("get /api http/1.1\r\n"), "got: {seen}");
            assert!(has_line(&seen, "host: app.proj.test"), "got: {seen}");
        }
    }
}
