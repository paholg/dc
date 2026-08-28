//! The per-service checks, run as one pipeline that publishes each result the
//! moment it has it.
//!
//! The layers are checked separately on purpose: knowing that
//! `feature.app.test` is unreachable is nearly useless, whereas knowing that
//! the proxy resolves it but the system resolver doesn't points straight at
//! `/etc/systemd/resolved.conf.d` — and knowing that the TLS handshake
//! succeeds but the app answers 502 points straight at the app.

use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use docker::{ContainerStatus, Docker, PROXY_CA_EXPIRY_LABEL, PROXY_CONFIG_HASH_LABEL};
use eyre::{Result, WrapErr};
use jiff::SignedDuration;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use serde::Serialize;
use shared::{
    ENV_CA_DIR, HTTP_PORT, HTTPS_PORT, PROXY_CONTAINER_NAME, ROOT_CA_KEY_PEM, ROOT_CA_PEM,
    navigation,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use super::ProxyChecks;
use super::endpoints::{Endpoint, Sidecar};
use crate::ansi::{GRAY, GREEN, RED, RESET};
use crate::cli::proxy::{PROXY_IMAGE, dns, intermediate, trust};
use crate::table::Datum;
use crate::table::gatherer::Publisher;

const DNS_TIMEOUT: Duration = Duration::from_secs(1);
const RESOLVER_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
const HTTP_TIMEOUT: Duration = Duration::from_secs(3);

/// Longest a whole row is allowed to take, as a backstop against a stage that
/// somehow outlives its own timeout.
pub(super) const ROW_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Outcome {
    Ok,
    /// Doesn't apply to this service — a DNS-only one has nothing to connect to.
    Skip,
    Fail,
}

/// One check's result: an outcome, the text to put in its cell, and the full
/// explanation for the DETAIL column.
#[derive(Debug, Clone, Serialize)]
pub(super) struct Check {
    pub(super) outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    short: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) detail: Option<String>,
}

impl Check {
    fn ok() -> Self {
        Check {
            outcome: Outcome::Ok,
            short: None,
            detail: None,
        }
    }

    /// Passed, with something worth showing in the cell — an HTTP status, say.
    fn ok_with(short: impl Into<String>) -> Self {
        Check {
            short: Some(short.into()),
            ..Check::ok()
        }
    }

    fn skip() -> Self {
        Check {
            outcome: Outcome::Skip,
            short: None,
            detail: None,
        }
    }

    /// Not checked, for a reason worth recording (but not worth a red cell).
    fn skip_because(detail: impl Into<String>) -> Self {
        Check {
            detail: Some(detail.into()),
            ..Check::skip()
        }
    }

    fn fail(detail: impl Into<String>) -> Self {
        Check {
            outcome: Outcome::Fail,
            short: None,
            detail: Some(detail.into()),
        }
    }

    /// Attach the explanation shown alongside a check that passed — what the
    /// proxy table puts in its DETAIL column.
    fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub(super) fn failed(&self) -> bool {
        self.outcome == Outcome::Fail
    }
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.outcome, self.short.as_deref()) {
            (Outcome::Ok, Some(short)) => write!(f, "{GREEN}{short}{RESET}"),
            (Outcome::Ok, None) => write!(f, "{GREEN}✓{RESET}"),
            (Outcome::Skip, _) => write!(f, "{GRAY}-{RESET}"),
            (Outcome::Fail, _) => write!(f, "{RED}✗{RESET}"),
        }
    }
}

/// Every check for one service, in the order they're run and displayed.
///
/// `http` is the service's answer over port 80; `connect` and `tls` are the
/// https path's prerequisites and `https` is the status it answered with.
#[derive(Debug, Clone, Default, Serialize)]
pub(super) struct RowChecks {
    pub(super) container: Datum<Check>,
    pub(super) sidecar: Datum<Check>,
    pub(super) dns: Datum<Check>,
    pub(super) resolver: Datum<Check>,
    pub(super) http: Datum<Check>,
    pub(super) connect: Datum<Check>,
    pub(super) tls: Datum<Check>,
    pub(super) https: Datum<Check>,
}

/// Reads one check out of a row; how a column or a note knows which stage it's
/// reporting on.
pub(super) type PickStage = fn(&RowChecks) -> &Datum<Check>;

/// Every stage, in the order it runs and displays. The name is the column
/// header, and — lowercased — how a note names the stage it came from.
pub(super) const STAGES: [(&str, PickStage); 8] = [
    ("CONTAINER", |c| &c.container),
    ("SIDECAR", |c| &c.sidecar),
    ("DNS", |c| &c.dns),
    ("RESOLV", |c| &c.resolver),
    ("HTTP", |c| &c.http),
    ("CONNECT", |c| &c.connect),
    ("TLS", |c| &c.tls),
    ("HTTPS", |c| &c.https),
];

impl RowChecks {
    /// Each failure, as the stage that reported it and what it said. Every one
    /// is listed, not just the first: a name the proxy resolves and a port
    /// nothing is listening on are two separate things to fix.
    pub(super) fn failures(&self) -> Vec<(&'static str, String)> {
        STAGES
            .iter()
            .filter_map(|(name, pick)| match pick(self) {
                Datum::Value(check) if check.failed() => Some((
                    *name,
                    check.detail.clone().unwrap_or_else(|| "failed".to_string()),
                )),
                _ => None,
            })
            .collect()
    }

    pub(super) fn failed(&self) -> bool {
        STAGES
            .iter()
            .any(|(_, pick)| matches!(pick(self), Datum::Value(check) if check.failed()))
    }

    /// True once every check has a result.
    pub(super) fn settled(&self) -> bool {
        STAGES
            .iter()
            .all(|(_, pick)| !matches!(pick(self), Datum::Pending))
    }
}

/// What every row needs to run its checks: the proxy's DNS port, a TLS client
/// set up against the configured CA, and the sidecars that exist.
pub(super) struct Probe {
    dns_port: u16,
    tls: TlsSetup,
    sidecars: HashMap<(String, String, String), Sidecar>,
}

enum TlsSetup {
    Ready(Arc<ClientConfig>),
    /// TLS can't be checked at all, and why.
    Unavailable(String),
}

impl Probe {
    pub(super) fn new(dns_port: u16, ca_root: &Path, sidecars: Vec<Sidecar>) -> Self {
        let tls = match tls_config(ca_root) {
            Ok(config) => TlsSetup::Ready(Arc::new(config)),
            Err(e) => {
                TlsSetup::Unavailable(format!("can't read the CA at {}: {e}", ca_root.display()))
            }
        };
        Probe {
            dns_port,
            tls,
            sidecars: sidecars
                .into_iter()
                .map(|sidecar| (sidecar.key.clone(), sidecar))
                .collect(),
        }
    }
}

/// Trust only the configured CA, so an untrusted certificate is reported as
/// exactly that rather than passing because some system root happens to cover
/// it.
fn tls_config(ca_root: &Path) -> Result<ClientConfig> {
    let path = ca_root.join(ROOT_CA_PEM);
    let mut roots = RootCertStore::empty();
    for cert in
        CertificateDer::pem_file_iter(&path).wrap_err_with(|| format!("read {}", path.display()))?
    {
        roots
            .add(cert.wrap_err_with(|| format!("parse {}", path.display()))?)
            .wrap_err("add the CA to the trust store")?;
    }
    if roots.is_empty() {
        eyre::bail!("{} contains no certificates", path.display());
    }

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .wrap_err("build a tls client")
        .map(|builder| builder.with_root_certificates(roots).with_no_client_auth())
}

/// Check the proxy itself: that it's there, that it's answering, and that it
/// isn't running settings the config has since moved past.
pub(super) async fn run_proxy(
    docker: &Docker,
    dns_port: u16,
    ca_root: &Path,
    expected_hash: &str,
    wants_tls: bool,
    strays: &[String],
    out: &mut Publisher<ProxyChecks>,
) {
    let socket = docker.socket().display().to_string();
    let api = docker.api_version();
    out.update(|c| {
        c.docker = Datum::Value(Check::ok().with_detail(format!("{socket} · api {api}")));
    });

    let details = match docker.inspect_container(PROXY_CONTAINER_NAME).await {
        Ok(details) => Some(details),
        Err(e) => {
            let why = match e {
                docker::Error::NotFound { .. } => {
                    "the proxy isn't there; run `dc proxy up`".to_string()
                }
                e => format!("couldn't inspect the proxy container: {e}"),
            };
            out.update(|c| {
                c.container = Datum::Value(Check::fail(why));
                c.image = Datum::Value(Check::skip());
                c.config = Datum::Value(Check::skip());
            });
            None
        }
    };

    // The image and settings checks are about the instance that's *running*, so
    // they mean nothing if it isn't.
    let running = details.as_ref().is_some_and(|d| d.state.running);
    if let Some(details) = &details {
        let container = if running {
            Check::ok().with_detail(format!("running · {}", details.config.image))
        } else {
            Check::fail(format!(
                "the container is {} (exit code {}); run `dc proxy up`",
                details.state.status, details.state.exit_code,
            ))
        };
        out.update(|c| c.container = Datum::Value(container));

        if running {
            let image = check_image(docker, &details.image).await;
            let config = check_config(
                details.config.labels.get(PROXY_CONFIG_HASH_LABEL),
                expected_hash,
            );
            out.update(|c| {
                c.image = Datum::Value(image);
                c.config = Datum::Value(config);
            });
        } else {
            out.update(|c| {
                c.image = Datum::Value(Check::skip());
                c.config = Datum::Value(Check::skip());
            });
        }
    }

    let dns = check_proxy_dns(dns_port).await;
    out.update(|c| c.dns = Datum::Value(dns));

    let ca = check_ca(ca_root, details.as_ref());
    let intermediate = check_intermediate(
        details.as_ref().filter(|d| d.state.running).map(|d| {
            d.config
                .labels
                .get(PROXY_CA_EXPIRY_LABEL)
                .map(String::as_str)
        }),
        jiff::Timestamp::now(),
    );
    let sidecars = check_strays(strays);
    out.update(|c| {
        c.ca = Datum::Value(ca);
        c.intermediate = Datum::Value(intermediate);
        c.sidecars = Datum::Value(sidecars);
    });

    // Reading the platform's trust store hits the filesystem (and, on macOS,
    // the keychain), which is slow enough to stall the redraw loop.
    let owned_root = ca_root.to_path_buf();
    let trust = tokio::task::spawn_blocking(move || check_trust(&owned_root, wants_tls))
        .await
        .unwrap_or_else(|_| Check::skip_because("the trust check didn't finish"));
    out.update(|c| c.trust = Datum::Value(trust));
}

/// Whether the machine trusts the CA — which the TLS check deliberately can't
/// tell you, since it trusts `caRoot` and nothing else so that an unrelated
/// system root can't make a wrong certificate pass. A handshake succeeding here
/// says the proxy serves a correct certificate; it says nothing about whether
/// your browser will accept it.
fn check_trust(ca_root: &Path, wants_tls: bool) -> Check {
    if !wants_tls {
        return Check::skip_because("no tls port needs it");
    }

    let path = ca_root.join(ROOT_CA_PEM);
    let ours: Vec<CertificateDer<'static>> =
        match CertificateDer::pem_file_iter(&path).and_then(std::iter::Iterator::collect) {
            Ok(certs) => certs,
            // The `ca` check already reports an unreadable CA; don't say it twice.
            Err(_) => return Check::skip_because(format!("couldn't read {}", path.display())),
        };

    let native = rustls_native_certs::load_native_certs();
    let problem = native
        .errors
        .first()
        .map(|e| e.to_string())
        .filter(|_| !native.errors.is_empty());
    let check = trust_verdict(&ours, &native.certs, problem);

    // Under WSL, native browsers read the Windows store, not this distro's —
    // an Ok from the Linux side is only half the answer. (A fail already
    // points at `dc proxy trust`, which fills both stores.)
    if check.outcome != Outcome::Ok || !trust::wsl() {
        return check;
    }
    match trust::windows::user_root_store() {
        Err(e) => Check::skip_because(format!(
            "in the system trust store, but the Windows store couldn't be read: {e:#}"
        )),
        Ok(dump)
            if ours
                .iter()
                .any(|cert| trust::windows::store_contains(&dump, cert.as_ref())) =>
        {
            Check::ok().with_detail("in the system and Windows trust stores")
        }
        Ok(_) => Check::fail(
            "the CA isn't in the Windows certificate store that native browsers read; run `dc proxy trust`",
        ),
    }
}

fn trust_verdict(
    ours: &[CertificateDer<'_>],
    native: &[CertificateDer<'_>],
    problem: Option<String>,
) -> Check {
    let trusted = ours
        .iter()
        .any(|ours| native.iter().any(|theirs| theirs.as_ref() == ours.as_ref()));
    if trusted {
        return Check::ok().with_detail("in the system trust store");
    }
    if let Some(problem) = problem {
        // The store didn't read cleanly, so its not being there proves nothing.
        return Check::skip_because(format!("couldn't read the system trust store: {problem}"));
    }
    Check::fail(
        "the CA isn't in the system trust store; run `dc proxy trust` (if you've added it to your browser directly, feel free to ignore this)",
    )
}

/// The container's image only moves when it's recreated, so a local image that
/// has since been re-pulled under the same tag means a restart is due.
async fn check_image(docker: &Docker, running_image_id: &str) -> Check {
    match docker.inspect_image(&PROXY_IMAGE).await {
        Err(docker::Error::NotFound { .. }) => {
            Check::skip_because(format!("{} isn't present locally", *PROXY_IMAGE))
        }
        Err(e) => Check::fail(format!("couldn't inspect {}: {e}", *PROXY_IMAGE)),
        Ok(image) if image.id == running_image_id => {
            Check::ok().with_detail(format!("matches the local {}", *PROXY_IMAGE))
        }
        Ok(_) => Check::fail(format!(
            "a newer {} has been pulled since the proxy started; run `dc proxy up`",
            *PROXY_IMAGE,
        )),
    }
}

fn check_config(label: Option<&String>, expected: &str) -> Check {
    match label {
        Some(hash) if hash == expected => Check::ok().with_detail("up to date"),
        Some(_) => Check::fail("the proxy is running older settings; run `dc proxy up`"),
        None => Check::fail("the proxy carries no settings hash; run `dc proxy up`"),
    }
}

/// Separate "nothing is there" from "something is there but not answering":
/// docker's port forwarder accepts connections whether or not the server behind
/// it is up.
async fn check_proxy_dns(port: u16) -> Check {
    let addr = SocketAddr::new(dns::LISTEN_IP, port);
    match dns::query(port, dns::PROBE_NAME, dns::Family::V4, DNS_TIMEOUT).await {
        Ok(_) => Check::ok().with_detail(format!("answering on {addr}")),
        Err(_) => match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
            Ok(Ok(_)) => Check::fail(format!(
                "{addr} accepts connections but no DNS answer came back; \
                 check `docker logs {PROXY_CONTAINER_NAME}`",
            )),
            _ => Check::fail(format!("nothing is listening on {addr}")),
        },
    }
}

fn check_ca(ca_root: &Path, details: Option<&docker::ContainerDetails>) -> Check {
    for file in [ROOT_CA_PEM, ROOT_CA_KEY_PEM] {
        let path = ca_root.join(file);
        if !path.exists() {
            return Check::fail(format!(
                "{} is missing; `dc proxy up` generates it",
                path.display()
            ));
        }
    }

    // The proxy predates CA delivery.
    if let Some(details) = details
        && details.state.running
        && !details.config.parsed_env().contains_key(ENV_CA_DIR)
    {
        return Check::fail("the proxy is running without a CA delivered; run `dc proxy up`");
    }

    Check::ok().with_detail(ca_root.display().to_string())
}

/// The intermediate CA the CLI handed the proxy at creation, judged by the
/// `notAfter` label stamped on the container. `running_label` is `None` when
/// the proxy isn't running, `Some(label)` otherwise.
fn check_intermediate(running_label: Option<Option<&str>>, now: jiff::Timestamp) -> Check {
    let Some(label) = running_label else {
        // The `container` row already covers a proxy that isn't running.
        return Check::skip();
    };
    match intermediate::expiry(label, now) {
        intermediate::Expiry::Missing => {
            Check::fail("the proxy carries no intermediate CA; run `dc proxy up`")
        }
        intermediate::Expiry::Expired(t) => Check::fail(format!(
            "the intermediate CA expired {t}; https is down until `dc proxy up`"
        )),
        intermediate::Expiry::ExpiresSoon(t) => Check::fail(format!(
            "the intermediate CA expires in {}; any dc command will renew it",
            humanize(t.duration_since(now)),
        )),
        intermediate::Expiry::Valid(t) => {
            Check::ok().with_detail(format!("expires in {}", humanize(t.duration_since(now))))
        }
    }
}

/// Render a duration the way a person tracks cert expiry: whole days once
/// there are at least two of them, hours below that.
fn humanize(d: SignedDuration) -> String {
    let hours = d.as_hours();
    if hours >= 48 {
        format!("{}d", hours / 24)
    } else {
        format!("{hours}h")
    }
}

fn check_strays(strays: &[String]) -> Check {
    if strays.is_empty() {
        return Check::ok().with_detail("no strays");
    }
    Check::fail(format!(
        "{} sidecar(s) left over from a previous run ({}); run `dc proxy up`",
        strays.len(),
        strays.join(", "),
    ))
}

/// Run every check for one service, publishing after each.
pub(super) async fn run(probe: &Probe, endpoint: &Endpoint, out: &mut Publisher<RowChecks>) {
    let Some((container_id, ip)) = check_container(endpoint, out) else {
        // Nothing else can be true if the container isn't.
        return skip_rest(out, Check::skip());
    };

    out.update(|c| c.sidecar = Datum::Value(check_sidecar(probe, endpoint, &container_id)));

    let Some(hostname) = endpoint.hostname.clone() else {
        // There is no name to look up or connect to, so this is the one and
        // only thing wrong with the row.
        out.update(|c| {
            c.dns = Datum::Value(Check::fail("the hostname template failed to render"));
        });
        return skip_rest(out, Check::skip());
    };

    let dns_ok = check_dns(probe, endpoint, &hostname, ip, out).await;
    check_resolver(&hostname, ip, dns_ok, probe.dns_port, out).await;

    let Some(http_proxy_port) = endpoint.http_proxy_port else {
        // DNS-only: there's nothing to reach, and that's not a fault.
        out.update(|c| {
            c.http = Datum::Value(Check::skip());
            c.connect = Datum::Value(Check::skip_because("no httpProxyPort is configured"));
            c.tls = Datum::Value(Check::skip());
            c.https = Datum::Value(Check::skip());
        });
        return;
    };

    // The two paths are reported independently: https can be broken while http
    // is fine, and saying which is the point of checking both.
    check_http(&hostname, ip, http_proxy_port, out).await;
    check_https(probe, &hostname, ip, http_proxy_port, out).await;
}

/// The https path: connect to 443, prove the sidecar's cert, then prove the
/// service behind it.
async fn check_https(
    probe: &Probe,
    hostname: &str,
    ip: IpAddr,
    http_proxy_port: u16,
    out: &mut Publisher<RowChecks>,
) {
    let addr = SocketAddr::new(ip, HTTPS_PORT);
    let Some(stream) = connect(addr, |c| &mut c.connect, out).await else {
        out.update(|c| {
            c.tls = Datum::Value(Check::skip());
            c.https = Datum::Value(Check::skip());
        });
        return;
    };
    check_https_app(probe, hostname, http_proxy_port, stream, out).await;
}

/// The http path: connect to 80 and see what the service answers.
async fn check_http(
    hostname: &str,
    ip: IpAddr,
    http_proxy_port: u16,
    out: &mut Publisher<RowChecks>,
) {
    let addr = SocketAddr::new(ip, HTTP_PORT);
    let Some(stream) = connect(addr, |c| &mut c.http, out).await else {
        return;
    };
    check_http_app(hostname, http_proxy_port, stream, out).await;
}

/// Mark everything that wasn't reached, so no cell is left spinning.
fn skip_rest(out: &mut Publisher<RowChecks>, reason: Check) {
    out.update(|c| {
        for slot in [
            &mut c.sidecar,
            &mut c.dns,
            &mut c.resolver,
            &mut c.http,
            &mut c.connect,
            &mut c.tls,
            &mut c.https,
        ] {
            if matches!(slot, Datum::Pending) {
                *slot = Datum::Value(reason.clone());
            }
        }
    });
}

/// Returns the container's id and IP if it's actually usable.
fn check_container(
    endpoint: &Endpoint,
    out: &mut Publisher<RowChecks>,
) -> Option<(String, IpAddr)> {
    let Some(target) = endpoint.container.as_ref() else {
        let why = format!(
            "no container for service `{}` in workspace `{}`",
            endpoint.service, endpoint.workspace
        );
        out.update(|c| c.container = Datum::Value(Check::fail(why)));
        return None;
    };

    if target.status != ContainerStatus::Running {
        let why = format!("the container is {}", target.status);
        out.update(|c| c.container = Datum::Value(Check::fail(why)));
        return None;
    }

    let Some(ip) = target.ip else {
        out.update(|c| {
            c.container = Datum::Value(Check::fail(
                "the container is running but has no network address",
            ));
        });
        return None;
    };

    out.update(|c| c.container = Datum::Value(Check::ok()));
    Some((target.id.clone(), ip))
}

fn check_sidecar(probe: &Probe, endpoint: &Endpoint, target_cid: &str) -> Check {
    if !endpoint.needs_sidecar() {
        return Check::skip();
    }
    let Some(expected) = endpoint.sidecar.as_ref() else {
        return Check::skip();
    };
    let Some(sidecar) = probe.sidecars.get(&endpoint.key()) else {
        return Check::fail("no sidecar for this service; run `dc proxy up`");
    };
    if sidecar.status != ContainerStatus::Running {
        return Check::fail(format!(
            "the sidecar is {}; check `docker logs {}`",
            sidecar.status,
            short_id(&sidecar.id),
        ));
    }
    if sidecar.target.as_deref() != Some(target_cid) {
        return Check::fail(
            "the sidecar is attached to a container that has since been replaced; \
             run `dc proxy up`",
        );
    }
    match sidecar.plan_hash.as_deref() {
        Some(hash) if hash == expected.plan_hash => Check::ok(),
        Some(_) => Check::fail("the sidecar was built from older settings; run `dc proxy up`"),
        // Sidecars only started carrying the hash recently; the proxy-level
        // `image` check is what flags that.
        None => Check::ok_with("?"),
    }
}

/// Ask the proxy itself, so a misconfigured system resolver can't mask what the
/// proxy knows (or hide that it knows nothing).
async fn check_dns(
    probe: &Probe,
    endpoint: &Endpoint,
    hostname: &str,
    expected: IpAddr,
    out: &mut Publisher<RowChecks>,
) -> bool {
    let check = match dns::query_for(probe.dns_port, hostname, expected, DNS_TIMEOUT).await {
        Err(e) => Check::fail(format!(
            "the proxy isn't answering on {}:{}: {e}",
            dns::LISTEN_IP,
            probe.dns_port,
        )),
        Ok(dns::Answer::Unknown) => Check::fail(format!(
            "the proxy has no record for {hostname}; it hasn't adopted this container",
        )),
        Ok(dns::Answer::Address(got)) if got != expected => match &endpoint.collides_with {
            Some(other) => Check::fail(format!(
                "{hostname} is also used by {other}, and the proxy keeps the first \
                 registration (it resolves to {got})",
            )),
            None => Check::fail(format!(
                "the proxy resolves {hostname} to {got}, but the container is at {expected}",
            )),
        },
        Ok(dns::Answer::Address(_)) => Check::ok(),
    };
    let ok = !check.failed();
    out.update(|c| c.dns = Datum::Value(check));
    ok
}

/// Go through `getaddrinfo`, which is what everything else on the machine uses.
/// Notably it reads `/etc/resolver` on macOS, where `dig` does not.
async fn check_resolver(
    hostname: &str,
    expected: IpAddr,
    dns_ok: bool,
    dns_port: u16,
    out: &mut Publisher<RowChecks>,
) {
    let lookup = tokio::time::timeout(
        RESOLVER_TIMEOUT,
        tokio::net::lookup_host((hostname.to_string(), 0)),
    )
    .await;

    let unrouted = || {
        let suffix = hostname.rsplit('.').next().unwrap_or("test");
        format!(
            "the system resolver doesn't know {hostname}; .{suffix} isn't routed to \
             {}:{dns_port} (see the DNS section of the README)",
            dns::LISTEN_IP,
        )
    };

    let check = match lookup {
        Err(_) => Check::fail(format!("the system resolver didn't answer for {hostname}")),
        Ok(Err(_)) => {
            if dns_ok {
                Check::fail(unrouted())
            } else {
                // The proxy has nothing to hand out, so this tells us nothing
                // about the resolver.
                Check::skip_because("the proxy has no record to resolve")
            }
        }
        Ok(Ok(addrs)) => {
            let addrs: Vec<IpAddr> = addrs.map(|a| a.ip()).collect();
            if addrs.is_empty() {
                Check::fail(unrouted())
            } else if addrs.contains(&expected) {
                Check::ok()
            } else {
                Check::fail(format!(
                    "the system resolver returns {} for {hostname}, not the container's \
                     {expected}; something else is answering for this name",
                    join(&addrs),
                ))
            }
        }
    };
    out.update(|c| c.resolver = Datum::Value(check));
}

/// `slot` says which cell the result lands in, since the https and http paths
/// each report their own connect.
async fn connect(
    addr: SocketAddr,
    slot: fn(&mut RowChecks) -> &mut Datum<Check>,
    out: &mut Publisher<RowChecks>,
) -> Option<TcpStream> {
    let result = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await;
    let (check, stream) = match result {
        Err(_) => (
            Check::fail(format!(
                "connecting to {addr} timed out; the container's network isn't reachable \
                 from the host (macOS and Windows need docker-mac-net-connect or OrbStack)",
            )),
            None,
        ),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => (
            Check::fail(format!(
                "nothing is listening on {addr}; the sidecar isn't serving port {}",
                addr.port(),
            )),
            None,
        ),
        Ok(Err(e)) => (
            Check::fail(format!("connecting to {addr} failed: {e}")),
            None,
        ),
        Ok(Ok(stream)) => (Check::ok(), Some(stream)),
    };
    out.update(|c| *slot(c) = Datum::Value(check));
    stream
}

/// A TLS port is terminated by the sidecar, which then reverse-proxies HTTP to
/// the container port — so the handshake proves the sidecar and its cert, and
/// the response proves the app behind it.
async fn check_https_app(
    probe: &Probe,
    hostname: &str,
    http_proxy_port: u16,
    stream: TcpStream,
    out: &mut Publisher<RowChecks>,
) {
    let config = match &probe.tls {
        TlsSetup::Unavailable(why) => {
            let why = why.clone();
            out.update(|c| {
                c.tls = Datum::Value(Check::fail(why));
                c.https = Datum::Value(Check::skip());
            });
            return;
        }
        TlsSetup::Ready(config) => config.clone(),
    };

    let server_name = match ServerName::try_from(hostname.to_string()) {
        Ok(name) => name,
        Err(e) => {
            out.update(|c| {
                c.tls = Datum::Value(Check::fail(format!(
                    "{hostname} is not a valid TLS name: {e}"
                )));
                c.https = Datum::Value(Check::skip());
            });
            return;
        }
    };

    let handshake = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        TlsConnector::from(config).connect(server_name, stream),
    )
    .await;

    let mut stream = match handshake {
        Err(_) => {
            out.update(|c| {
                c.tls = Datum::Value(Check::fail("the TLS handshake timed out"));
                c.https = Datum::Value(Check::skip());
            });
            return;
        }
        Ok(Err(e)) => {
            let why = describe_handshake_error(&e, hostname);
            out.update(|c| {
                c.tls = Datum::Value(Check::fail(why));
                c.https = Datum::Value(Check::skip());
            });
            return;
        }
        Ok(Ok(stream)) => {
            out.update(|c| c.tls = Datum::Value(Check::ok()));
            stream
        }
    };

    let check = match http_status(&mut stream, hostname).await {
        Err(e) => Check::fail(format!("no HTTP response over TLS: {e}")),
        // The sidecar's reverse proxy answers these when nothing is listening
        // on the container port, so it's the app that's missing, not the proxy.
        Ok(status @ (502 | 504)) => Check::fail(format!(
            "the proxy answered {status}: nothing is serving on container port {}",
            http_proxy_port,
        )),
        Ok(status) => Check::ok_with(status.to_string()),
    };
    out.update(|c| c.https = Datum::Value(check));
}

/// The service's own answer over port 80. The sidecar only redirects browser
/// navigations to https; this asks the way a script would, which proxies
/// straight through, so what comes back is the app's.
async fn check_http_app(
    hostname: &str,
    http_proxy_port: u16,
    mut stream: TcpStream,
    out: &mut Publisher<RowChecks>,
) {
    let check = match http_status(&mut stream, hostname).await {
        Err(e) => Check::fail(format!(
            "no HTTP response on port {HTTP_PORT}: {e}; check that container port \
             {http_proxy_port} is serving"
        )),
        // Both ports are reverse proxies, so a container port with nothing on
        // it is the sidecar's own gateway status rather than the app's.
        Ok(status @ (502 | 504)) => Check::fail(format!(
            "the proxy answered {status}: nothing is serving on container port {http_proxy_port}",
        )),
        Ok(status) => Check::ok_with(status.to_string()),
    };
    // Overwrites this path's own connect result; the https path owns `https`.
    out.update(|c| c.http = Datum::Value(check));
}

/// rustls' errors are precise but not friendly; the two that actually happen in
/// this setup are worth naming.
fn describe_handshake_error(error: &std::io::Error, hostname: &str) -> String {
    let text = error.to_string();
    if text.contains("UnknownIssuer") {
        format!(
            "{hostname} presents a certificate the configured CA didn't issue; \
             the proxy may be running with a different proxy.caRoot, or its \
             intermediate CA is missing or expired (run `dc proxy up`)",
        )
    } else if text.contains("NotValidForName") {
        format!("the certificate served on this port isn't valid for {hostname}")
    } else if text.contains("NameConstraintViolation") {
        format!(
            "{hostname} is outside the TLDs the intermediate CA may vouch for; \
             add its TLD to proxy.tlds and run `dc proxy up`",
        )
    } else {
        format!("the TLS handshake failed: {text}")
    }
}

/// Send the smallest request that gets a status line back, and read only that.
/// Asks as a script would, not as a browser navigation, so port 80 proxies it
/// through to the service instead of redirecting it to https.
async fn http_status<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S, host: &str) -> Result<u16> {
    let request = format!(
        "GET / HTTP/1.1\r\nHost: {host}\r\nUser-Agent: devconcurrent\r\n\
         {}: */*\r\nConnection: close\r\n\r\n",
        navigation::ACCEPT_HEADER,
    );

    tokio::time::timeout(HTTP_TIMEOUT, async {
        stream.write_all(request.as_bytes()).await?;
        stream.flush().await?;

        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        while line.len() < 128 {
            if stream.read(&mut byte).await? == 0 {
                break;
            }
            if byte[0] == b'\n' {
                break;
            }
            line.push(byte[0]);
        }
        Ok::<_, std::io::Error>(line)
    })
    .await
    .map_err(|_| eyre::eyre!("the request timed out"))?
    .wrap_err("read the response")
    .and_then(|line| parse_status_line(&line))
}

fn parse_status_line(line: &[u8]) -> Result<u16> {
    let line = String::from_utf8_lossy(line);
    let line = line.trim_end();
    let mut parts = line.split(' ');
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/") {
        if line.is_empty() {
            eyre::bail!("the connection closed without a response");
        }
        eyre::bail!("the response didn't start with a status line");
    }
    parts
        .next()
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| eyre::eyre!("the status line has no status code"))
}

fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

fn join(addrs: &[IpAddr]) -> String {
    addrs
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::proxy::status::endpoints::{ExpectedSidecar, Target};

    /// `feature/app`, a served service that expects a sidecar.
    fn endpoint() -> Endpoint {
        let plan = shared::SidecarPlan {
            hostname: "feature.app.test".to_string(),
            port: 8080,
        };
        Endpoint {
            project: "proj".to_string(),
            workspace: "feature".to_string(),
            service: "app".to_string(),
            hostname: Some(plan.hostname.clone()),
            http_proxy_port: Some(plan.port),
            container: Some(Target {
                id: "target-cid".to_string(),
                status: ContainerStatus::Running,
                ip: Some(std::net::IpAddr::from([172, 18, 0, 2])),
            }),
            sidecar: Some(Arc::new(ExpectedSidecar {
                plan_hash: plan.hash(),
            })),
            collides_with: None,
        }
    }

    fn sidecar(status: ContainerStatus, target: &str, hash: Option<&str>) -> Sidecar {
        Sidecar {
            id: "sidecar-cid".to_string(),
            status,
            target: Some(target.to_string()),
            plan_hash: hash.map(ToString::to_string),
            key: ("proj".into(), "feature".into(), "app".into()),
        }
    }

    fn probe(sidecars: Vec<Sidecar>) -> Probe {
        Probe::new(43770, Path::new("/nonexistent"), sidecars)
    }

    fn expected_hash(endpoint: &Endpoint) -> String {
        endpoint.sidecar.as_ref().unwrap().plan_hash.clone()
    }

    #[test]
    fn a_matching_sidecar_passes() {
        let endpoint = endpoint();
        let hash = expected_hash(&endpoint);
        let probe = probe(vec![sidecar(
            ContainerStatus::Running,
            "target-cid",
            Some(&hash),
        )]);
        assert_eq!(
            check_sidecar(&probe, &endpoint, "target-cid").outcome,
            Outcome::Ok,
        );
    }

    #[test]
    fn a_missing_sidecar_fails() {
        let check = check_sidecar(&probe(Vec::new()), &endpoint(), "target-cid");
        assert!(check.failed());
        assert!(check.detail.unwrap().contains("dc proxy up"));
    }

    #[test]
    fn a_stopped_sidecar_fails() {
        let endpoint = endpoint();
        let hash = expected_hash(&endpoint);
        let probe = probe(vec![sidecar(
            ContainerStatus::Exited,
            "target-cid",
            Some(&hash),
        )]);
        let check = check_sidecar(&probe, &endpoint, "target-cid");
        assert!(check.failed());
        assert!(check.detail.unwrap().contains("exited"));
    }

    #[test]
    fn a_sidecar_pointing_at_a_replaced_container_fails() {
        let endpoint = endpoint();
        let hash = expected_hash(&endpoint);
        let probe = probe(vec![sidecar(
            ContainerStatus::Running,
            "old-cid",
            Some(&hash),
        )]);
        assert!(check_sidecar(&probe, &endpoint, "target-cid").failed());
    }

    #[test]
    fn a_sidecar_built_from_older_settings_fails() {
        let probe = probe(vec![sidecar(
            ContainerStatus::Running,
            "target-cid",
            Some("some-other-hash"),
        )]);
        let check = check_sidecar(&probe, &endpoint(), "target-cid");
        assert!(check.failed());
        assert!(check.detail.unwrap().contains("older settings"));
    }

    /// Sidecars from a proxy image that predates the label can't be judged, and
    /// the proxy-level image check is what flags that.
    #[test]
    fn a_sidecar_without_a_plan_hash_is_not_a_failure() {
        let probe = probe(vec![sidecar(ContainerStatus::Running, "target-cid", None)]);
        let check = check_sidecar(&probe, &endpoint(), "target-cid");
        assert_eq!(check.outcome, Outcome::Ok);
        assert_eq!(check.short.as_deref(), Some("?"));
    }

    /// A DNS-only service has nothing in front of it to check.
    #[test]
    fn a_service_with_no_http_proxy_port_needs_no_sidecar() {
        let mut endpoint = endpoint();
        endpoint.http_proxy_port = None;
        assert_eq!(
            check_sidecar(&probe(Vec::new()), &endpoint, "target-cid").outcome,
            Outcome::Skip,
        );
    }

    #[test]
    fn settings_are_compared_by_hash() {
        assert_eq!(
            check_config(Some(&"abc".to_string()), "abc").outcome,
            Outcome::Ok,
        );
        assert!(check_config(Some(&"abc".to_string()), "def").failed());
        assert!(check_config(None, "def").failed());
    }

    #[test]
    fn a_ca_directory_missing_its_files_fails() {
        let dir = tempfile::tempdir().expect("temp dir");
        let check = check_ca(dir.path(), None);
        assert!(check.failed());
        assert!(check.detail.unwrap().contains("rootCA.pem"));

        std::fs::write(dir.path().join("rootCA.pem"), "").expect("write cert");
        let check = check_ca(dir.path(), None);
        assert!(check.failed(), "the key is still missing");

        std::fs::write(dir.path().join("rootCA-key.pem"), "").expect("write key");
        assert_eq!(check_ca(dir.path(), None).outcome, Outcome::Ok);
    }

    #[test]
    fn the_intermediate_is_only_judged_when_the_proxy_is_running() {
        // The container row owns a proxy that isn't running.
        assert_eq!(
            check_intermediate(None, jiff::Timestamp::now()).outcome,
            Outcome::Skip
        );
    }

    #[test]
    fn a_running_proxy_without_an_intermediate_fails() {
        let check = check_intermediate(Some(None), jiff::Timestamp::now());
        assert!(check.failed());
        assert!(check.detail.unwrap().contains("dc proxy up"));
    }

    #[test]
    fn intermediate_expiry_is_reported() {
        let now: jiff::Timestamp = "2026-08-16T00:00:00Z".parse().unwrap();

        let valid = (now + SignedDuration::from_hours(29 * 24)).to_string();
        let check = check_intermediate(Some(Some(&valid)), now);
        assert_eq!(check.outcome, Outcome::Ok);
        assert_eq!(check.detail.as_deref(), Some("expires in 29d"));

        let soon = (now + SignedDuration::from_hours(30)).to_string();
        let check = check_intermediate(Some(Some(&soon)), now);
        assert!(check.failed());
        assert!(check.detail.unwrap().contains("expires in 30h"));

        let expired = (now - SignedDuration::from_hours(1)).to_string();
        let check = check_intermediate(Some(Some(&expired)), now);
        assert!(check.failed());
        assert!(check.detail.unwrap().contains("expired"));
    }

    fn der(bytes: &[u8]) -> CertificateDer<'static> {
        CertificateDer::from(bytes.to_vec())
    }

    #[test]
    fn a_ca_present_in_the_platform_store_is_trusted() {
        let ours = [der(b"our-ca")];
        let native = [der(b"some-public-root"), der(b"our-ca")];
        assert_eq!(trust_verdict(&ours, &native, None).outcome, Outcome::Ok);
    }

    #[test]
    fn a_ca_missing_from_the_platform_store_points_at_proxy_trust() {
        let ours = [der(b"our-ca")];
        let native = [der(b"some-public-root")];
        let check = trust_verdict(&ours, &native, None);
        assert!(check.failed());
        assert!(check.detail.unwrap().contains("dc proxy trust"));
    }

    /// A store we couldn't read is not evidence of anything.
    #[test]
    fn an_unreadable_platform_store_is_not_a_failure() {
        let ours = [der(b"our-ca")];
        let check = trust_verdict(&ours, &[], Some("permission denied".to_string()));
        assert_eq!(check.outcome, Outcome::Skip);
        assert!(check.detail.unwrap().contains("permission denied"));
    }

    #[test]
    fn trust_is_not_checked_when_nothing_needs_it() {
        assert_eq!(
            check_trust(Path::new("/nonexistent"), false).outcome,
            Outcome::Skip,
        );
    }

    #[test]
    fn leftover_sidecars_are_named() {
        assert_eq!(check_strays(&[]).outcome, Outcome::Ok);
        let check = check_strays(&["old/app".to_string()]);
        assert!(check.failed());
        assert!(check.detail.unwrap().contains("old/app"));
    }

    fn checks(container: Check, sidecar: Check) -> RowChecks {
        RowChecks {
            container: Datum::Value(container),
            sidecar: Datum::Value(sidecar),
            ..RowChecks::default()
        }
    }

    #[test]
    fn a_passing_check_renders_a_tick_or_its_own_text() {
        assert!(Check::ok().to_string().contains('✓'));
        assert!(Check::ok_with("200").to_string().contains("200"));
    }

    #[test]
    fn failures_are_named_by_the_stage_that_found_them() {
        let row = checks(Check::ok(), Check::fail("no sidecar"));
        assert_eq!(row.failures(), [("SIDECAR", "no sidecar".to_string())]);
        assert!(row.failed());
    }

    /// Two independent things are wrong, so both get reported — the DNS
    /// failure isn't the cause of the port failure.
    #[test]
    fn every_failure_is_listed_in_stage_order() {
        let mut row = checks(Check::ok(), Check::skip());
        row.resolver = Datum::Value(Check::fail("not routed"));
        row.connect = Datum::Value(Check::fail("refused"));
        assert_eq!(
            row.failures(),
            [
                ("RESOLV", "not routed".to_string()),
                ("CONNECT", "refused".to_string()),
            ],
        );
    }

    #[test]
    fn a_clean_row_has_nothing_to_report() {
        let mut row = checks(Check::ok(), Check::skip());
        for slot in [
            &mut row.dns,
            &mut row.resolver,
            &mut row.connect,
            &mut row.tls,
            &mut row.https,
            &mut row.http,
        ] {
            *slot = Datum::Value(Check::ok());
        }
        assert!(row.failures().is_empty());
        assert!(!row.failed());
        assert!(row.settled());
    }

    #[test]
    fn skipped_checks_are_not_failures() {
        let row = checks(Check::ok(), Check::skip_because("no httpProxyPort"));
        assert!(!row.failed());
    }

    #[test]
    fn reads_the_status_code() {
        assert_eq!(parse_status_line(b"HTTP/1.1 200 OK\r").unwrap(), 200);
        assert_eq!(parse_status_line(b"HTTP/1.0 502 Bad Gateway").unwrap(), 502);
        assert_eq!(parse_status_line(b"HTTP/1.1 404 Not Found").unwrap(), 404);
    }

    #[test]
    fn rejects_anything_that_is_not_a_status_line() {
        assert!(parse_status_line(b"").is_err());
        assert!(parse_status_line(b"\x16\x03\x01 garbage").is_err());
        assert!(parse_status_line(b"HTTP/1.1").is_err());
        assert!(parse_status_line(b"HTTP/1.1 nope").is_err());
    }

    #[test]
    fn handshake_errors_name_the_likely_cause() {
        let untrusted = std::io::Error::other("invalid peer certificate: UnknownIssuer");
        assert!(
            describe_handshake_error(&untrusted, "a.test").contains("proxy.caRoot"),
            "should point at the CA",
        );
        let wrong_name = std::io::Error::other("invalid peer certificate: NotValidForName");
        assert!(describe_handshake_error(&wrong_name, "a.test").contains("a.test"));
    }
}
