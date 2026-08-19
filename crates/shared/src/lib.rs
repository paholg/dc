//! Wire format and shared constants between the devconcurrent CLI and the
//! devconcurrent-proxy service.
//!
//! The CLI writes one `<project>.json` file per project into the
//! `devconcurrent-proxy-config` volume; the proxy reads them at startup. The
//! file is the merged [`ProxyOptions`] for that project — the same struct
//! the CLI builds from `customizations.devconcurrent.proxy` in
//! `devcontainer.json`. No transformation, no separate wire struct.

use indexmap::IndexMap;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// Resource names.
pub const PROXY_CONTAINER_NAME: &str = "devconcurrent-proxy";
pub const PROXY_CONFIG_VOLUME: &str = "devconcurrent-proxy-config";

// In-container paths.
pub const PROXY_CONFIG_DIR: &str = "/etc/proxy";
/// Single file inside [`PROXY_CONFIG_DIR`] containing the merged
/// `HashMap<project_name, ProxyOptions>` for all proxy-enabled projects.
pub const PROXY_CONFIG_FILE: &str = "projects.json";
/// Directory inside the proxy container where the CLI uploads the intermediate CA before starting
/// it. It lives in the container's writable layer, never in a volume: the key dies with the
/// container.
pub const PROXY_CA_DIR: &str = "/etc/proxy-ca";
/// Intermediate CA cert + key file names inside [`PROXY_CA_DIR`].
pub const PROXY_CA_CERT_FILE: &str = "intermediateCA.pem";
pub const PROXY_CA_KEY_FILE: &str = "intermediateCA-key.pem";

/// Root CA file names on the host, inside the configured `proxy.caRoot`. The
/// names match mkcert's so `CAROOT=<caRoot> mkcert -install` can install
/// trust. Read by the CLI to sign the intermediate; never mounted into any
/// container.
pub const ROOT_CA_PEM: &str = "rootCA.pem";
pub const ROOT_CA_KEY_PEM: &str = "rootCA-key.pem";

/// Directory inside each sidecar container where the proxy writes the per-
/// service plan and (if TLS is enabled) cert + key.
pub const SIDECAR_PLAN_DIR: &str = "/etc/sidecar";
pub const SIDECAR_PLAN_FILE: &str = "plan.json";
pub const SIDECAR_CERT_FILE: &str = "cert.pem";
pub const SIDECAR_KEY_FILE: &str = "key.pem";

// Environment variables read by the proxy on startup.
pub const ENV_DNS_PORT: &str = "DEVCONCURRENT_PROXY_DNS_PORT";
/// Set by the CLI when an intermediate CA was uploaded. The proxy loads
/// [`PROXY_CA_CERT_FILE`] + [`PROXY_CA_KEY_FILE`] from this directory.
pub const ENV_CA_DIR: &str = "DEVCONCURRENT_PROXY_CA_DIR";

/// Default Handlebars template for proxied hostnames.
pub const DEFAULT_HOSTNAME_TEMPLATE: &str = "{{workspace}}.{{service}}.test";

/// Build a docker container or volume name from a fixed `prefix` and the
/// identity `parts` it belongs to.
///
/// The parts are joined with `-` so the name stays readable, but that half is
/// not injective: `-` is legal *inside* a project, workspace, or service name,
/// and characters docker forbids fold to `_`. So `("a-b", "c")` and
/// `("a", "b-c")` read alike, as do `("a/b", "c")` and `("a_b", "c")`. A short
/// digest of the raw, length-prefixed parts is appended, so distinct tuples
/// always get distinct names — which matters because a name is what docker
/// enforces uniqueness on.
///
/// `prefix` is expected to be a literal, so it supplies the alphanumeric
/// leading character docker requires.
#[must_use]
pub fn container_name(prefix: &str, parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        // Length-prefixed, so no concatenation of parts can be mistaken for a
        // different split of the same bytes.
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    let suffix: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();

    let readable: String = parts
        .join("-")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();

    format!("{prefix}-{readable}-{suffix}")
}

/// Per-project proxy configuration.
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct ProxyOptions {
    /// Enable the devconcurrent DNS and HTTP proxy for this project.
    pub enable: bool,

    /// Handlebars template for the proxied hostname, used by every service
    /// that does not set its own.
    ///
    /// Available variables:
    /// - `root` (bool) — whether this is the root workspace
    /// - `project` — project name
    /// - `workspace` — workspace name
    /// - `service` — name of the service from compose
    #[schemars(extend("default" = DEFAULT_HOSTNAME_TEMPLATE))]
    pub hostname: Option<Template>,

    /// Configure proxy settings for each docker compose service.
    pub services: IndexMap<String, ProxyService>,
}

impl ProxyOptions {
    /// Render the hostname for one `(project, workspace, service)` tuple using
    /// the service's `hostname` template, falling back to the project-level
    /// one and then to the default. Returns `None` if the template fails to
    /// render.
    #[must_use]
    pub fn render_hostname(
        &self,
        project: &str,
        workspace: &str,
        service: &str,
        root: bool,
    ) -> Option<String> {
        #[derive(serde::Serialize)]
        struct Ctx<'a> {
            root: bool,
            project: &'a str,
            workspace: &'a str,
            service: &'a str,
        }
        let source = self
            .services
            .get(service)
            .and_then(|s| s.hostname.as_ref())
            .or(self.hostname.as_ref())
            .map_or(DEFAULT_HOSTNAME_TEMPLATE, Template::source);

        let mut hbs = handlebars::Handlebars::new();
        hbs.set_strict_mode(false);

        let ctx = Ctx {
            root,
            project,
            workspace,
            service,
        };
        hbs.render_template(source, &ctx).ok()
    }

    /// Render one `customizations.devconcurrent.env` value for this workspace.
    ///
    /// `{{hostname 'svc'}}` resolves through [`Self::render_hostname`]; `root`,
    /// `project` and `workspace` are available as plain variables.
    ///
    /// Unlike [`Self::render_hostname`], this renders in strict mode: these
    /// values end up as shell variables, where a silently empty one is far
    /// worse than a loud failure.
    pub fn render_env_value(
        &self,
        project: &str,
        workspace: &str,
        root: bool,
        template: &Template,
    ) -> Result<String, handlebars::RenderError> {
        #[derive(serde::Serialize)]
        struct Ctx<'a> {
            root: bool,
            project: &'a str,
            workspace: &'a str,
        }

        let mut hbs = handlebars::Handlebars::new();
        hbs.set_strict_mode(true);
        hbs.register_helper(
            "hostname",
            Box::new(HostnameHelper {
                options: self.clone(),
                project: project.to_string(),
                workspace: workspace.to_string(),
                root,
            }),
        );

        let ctx = Ctx {
            root,
            project,
            workspace,
        };
        hbs.render_template(template.source(), &ctx)
    }
}

/// The `{{hostname 'service'}}` helper available in `env` templates.
struct HostnameHelper {
    options: ProxyOptions,
    project: String,
    workspace: String,
    root: bool,
}

impl handlebars::HelperDef for HostnameHelper {
    fn call<'reg: 'rc, 'rc>(
        &self,
        h: &handlebars::Helper<'rc>,
        _: &'reg handlebars::Handlebars<'reg>,
        _: &'rc handlebars::Context,
        _: &mut handlebars::RenderContext<'reg, 'rc>,
        out: &mut dyn handlebars::Output,
    ) -> handlebars::HelperResult {
        let service = h.param(0).and_then(|p| p.value().as_str()).ok_or_else(|| {
            handlebars::RenderErrorReason::Other(
                "the `hostname` helper takes one string argument, e.g. {{hostname 'app'}}"
                    .to_string(),
            )
        })?;

        let hostname = self
            .options
            .render_hostname(&self.project, &self.workspace, service, self.root)
            .ok_or_else(|| {
                handlebars::RenderErrorReason::Other(format!(
                    "could not render the hostname template for service {service:?}"
                ))
            })?;

        out.write(&hostname)?;
        Ok(())
    }
}

/// The fixed pair of ports the proxy binds in front of every proxied service.
/// Both reach the service on its `containerPort`, which is why that port can
/// be neither of these.
pub const HTTP_PORT: u16 = 80;
pub const HTTPS_PORT: u16 = 443;

/// How the sidecar picks out a browser navigation on [`HTTP_PORT`], and what
/// it answers one with.
///
/// The sidecar matches on these; `dc proxy status` builds a request from them
/// so that its http check exercises the redirect rather than being spliced
/// through as a scripted request would be. They only agree because they are
/// the same constants.
pub mod navigation {
    use http::{HeaderName, StatusCode, header};

    /// Set by every current browser, and says exactly what the request is for.
    pub const MODE_HEADER: HeaderName = HeaderName::from_static("sec-fetch-mode");
    /// The [`MODE_HEADER`] value that means "the user is going to this page".
    pub const MODE: &str = "navigate";

    /// For clients too old to send [`MODE_HEADER`], the closest available
    /// signal is asking for a page.
    pub const ACCEPT_HEADER: HeaderName = header::ACCEPT;
    pub const ACCEPT: &str = "text/html";

    /// Temporary, not permanent: a 301 would be cached against the hostname
    /// more or less forever, which is miserable the first time someone turns
    /// TLS off. It also preserves the method, though only GET and HEAD are
    /// ever redirected.
    pub const REDIRECT_STATUS: StatusCode = StatusCode::TEMPORARY_REDIRECT;
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct ProxyService {
    /// Handlebars template for this service's hostname. Overrides the
    /// project-level `hostname`.
    pub hostname: Option<Template>,

    /// If set, devconcurrent will run an HTTP proxy on ports 80 and 443 to this port in your
    /// container, performing TLS termination on 443.
    ///
    /// If this service runs a web service, put its port here.
    ///
    /// All ports other than 80 and 443 are forwarded raw to the service, whether
    /// this is set or not.
    pub container_port: Option<u16>,
}

/// A Handlebars template, compiled at deserialization time so syntax errors
/// surface as config-load errors rather than at first use.
#[derive(Clone, Debug)]
pub struct Template {
    source: String,
    // TODO: Should we be using this? Currently it's used to ensure validate at deserialization time,
    // but we could probably also use it to render?
    #[allow(unused)]
    compiled: handlebars::Template,
}

impl Template {
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    fn compile(source: String) -> Result<Self, handlebars::TemplateError> {
        let compiled = handlebars::Template::compile(&source)?;
        Ok(Self { source, compiled })
    }
}

impl Default for Template {
    fn default() -> Self {
        Self::compile(DEFAULT_HOSTNAME_TEMPLATE.to_string())
            .expect("default template is a valid Handlebars template")
    }
}

impl PartialEq for Template {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

impl Eq for Template {}

impl<'de> Deserialize<'de> for Template {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::compile(s).map_err(de::Error::custom)
    }
}

impl Serialize for Template {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.source)
    }
}

impl JsonSchema for Template {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "HandlebarsTemplate".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description":
                "A Handlebars template. Hostname templates get `root` (bool), \
                `project`, `workspace` and `service`; `env` templates get \
                `root`, `project`, `workspace` and the `hostname` helper.",
        })
    }
}

/// Sidecar plan, written by the proxy into the sidecar container's
/// filesystem at `<SIDECAR_PLAN_DIR>/<SIDECAR_PLAN_FILE>` before start.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SidecarPlan {
    /// Rendered hostname for this service; used as the TLS cert's SAN.
    pub hostname: String,
    /// The container port to forward to.
    pub port: u16,
}

impl SidecarPlan {
    /// Hash of everything the sidecar was built from, stamped on the sidecar
    /// container as a label so the CLI can tell a live sidecar from one whose
    /// plan the config has since moved past.
    #[must_use]
    pub fn hash(&self) -> String {
        let json = serde_json::to_string(self).expect("a sidecar plan always serializes");
        let digest = Sha256::digest(json.as_bytes());
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(source: &str) -> Template {
        Template::compile(source.to_string()).expect("valid template")
    }

    #[test]
    fn service_hostname_overrides_the_project_one() {
        let opts = ProxyOptions {
            hostname: Some(template("{{workspace}}.{{service}}.test")),
            services: [(
                "app".to_string(),
                ProxyService {
                    hostname: Some(template("{{workspace}}.test")),
                    ..ProxyService::default()
                },
            )]
            .into_iter()
            .collect(),
            ..ProxyOptions::default()
        };
        assert_eq!(
            opts.render_hostname("proj", "feature", "app", false)
                .unwrap(),
            "feature.test"
        );
        assert_eq!(
            opts.render_hostname("proj", "feature", "db", false)
                .unwrap(),
            "feature.db.test"
        );
    }

    #[test]
    fn falls_back_to_the_default_template() {
        let opts = ProxyOptions::default();
        assert_eq!(
            opts.render_hostname("proj", "feature", "db", false)
                .unwrap(),
            "feature.db.test"
        );
    }

    #[test]
    fn env_hostname_helper_follows_the_service_override() {
        let opts = ProxyOptions {
            services: [(
                "postgres".to_string(),
                ProxyService {
                    hostname: Some(template("db.{{workspace}}.test")),
                    ..ProxyService::default()
                },
            )]
            .into_iter()
            .collect(),
            ..ProxyOptions::default()
        };
        let url = template("postgres://user@{{hostname 'postgres'}}:5432/db");
        assert_eq!(
            opts.render_env_value("proj", "feature", false, &url)
                .unwrap(),
            "postgres://user@db.feature.test:5432/db"
        );
    }

    #[test]
    fn env_hostname_helper_takes_either_quote_style() {
        let opts = ProxyOptions::default();
        for source in ["{{hostname 'app'}}", r#"{{hostname "app"}}"#] {
            assert_eq!(
                opts.render_env_value("proj", "feature", false, &template(source))
                    .unwrap(),
                "feature.app.test",
                "{source}"
            );
        }
    }

    #[test]
    fn env_templates_see_the_workspace() {
        let opts = ProxyOptions::default();
        assert_eq!(
            opts.render_env_value("proj", "feature", false, &template("db_{{workspace}}"))
                .unwrap(),
            "db_feature"
        );
    }

    /// A silently empty shell variable is worse than a failed prompt.
    #[test]
    fn env_templates_reject_unknown_variables() {
        let opts = ProxyOptions::default();
        let err = opts
            .render_env_value("proj", "feature", false, &template("{{postgres}}"))
            .expect_err("strict mode");
        assert!(err.to_string().contains("postgres"), "got: {err}");
    }

    #[test]
    fn env_hostname_helper_needs_a_service() {
        let opts = ProxyOptions::default();
        let err = opts
            .render_env_value("proj", "feature", false, &template("{{hostname}}"))
            .expect_err("no argument");
        assert!(
            err.to_string().contains("one string argument"),
            "got: {err}"
        );
    }

    /// The key the whole config now hangs on, so pin its spelling.
    #[test]
    fn deserializes_container_port() {
        let svc: ProxyService = serde_json::from_str(r#"{"containerPort": 3000}"#).unwrap();
        assert_eq!(svc.container_port, Some(3000));
    }

    /// A service may exist purely so the proxy answers DNS for its hostname.
    #[test]
    fn container_port_is_optional() {
        let svc: ProxyService =
            serde_json::from_str(r#"{"hostname": "{{workspace}}.test"}"#).unwrap();
        assert_eq!(svc.container_port, None);
    }

    fn plan(hostname: &str, port: u16) -> SidecarPlan {
        SidecarPlan {
            hostname: hostname.to_string(),
            port,
        }
    }

    #[test]
    fn plan_hash_is_deterministic_and_a_valid_label_value() {
        let plan = plan("feature.app.test", 8080);
        let hash = plan.hash();
        assert_eq!(hash, plan.hash());
        assert_eq!(hash.len(), 64);
        assert!(
            hash.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "unexpected character in {hash}",
        );
    }

    #[test]
    fn plan_hash_changes_when_any_input_changes() {
        let base = plan("feature.app.test", 8080).hash();
        assert_ne!(base, plan("other.app.test", 8080).hash());
        assert_ne!(base, plan("feature.app.test", 3000).hash());
    }

    #[test]
    fn deserializes_valid_template() {
        let t: Template = serde_json::from_str("\"{{project}}.test\"").unwrap();
        assert_eq!(t.source(), "{{project}}.test");
    }

    #[test]
    fn rejects_invalid_template() {
        assert!(serde_json::from_str::<Template>("\"{{#unclosed\"").is_err());
    }

    #[test]
    fn container_name_is_deterministic() {
        let name = container_name("pre", &["a", "b", "c"]);
        assert_eq!(name, container_name("pre", &["a", "b", "c"]));
        assert!(name.starts_with("pre-a-b-c-"), "unexpected name {name}");
    }

    #[test]
    fn container_name_distinguishes_parts_the_join_cannot() {
        // The readable half of both is `pre-a-b-c-…`.
        assert_ne!(
            container_name("pre", &["a-b", "c"]),
            container_name("pre", &["a", "b-c"]),
        );
        // The readable half of both is `pre-a_b-c-…`.
        assert_ne!(
            container_name("pre", &["a/b", "c"]),
            container_name("pre", &["a_b", "c"]),
        );
    }

    #[test]
    fn container_name_is_legal_for_docker() {
        let name = container_name("pre", &["a/b", "c:d", "é", ""]);
        let mut chars = name.chars();
        assert!(chars.next().unwrap().is_ascii_alphanumeric());
        assert!(
            chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
            "unexpected character in {name}",
        );
    }
}
