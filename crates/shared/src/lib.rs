//! Wire format and shared constants between the devconcurrent CLI and the
//! devconcurrent-proxy service.
//!
//! The CLI writes one `<project>.json` file per project into the
//! `devconcurrent-proxy-config` volume; the proxy reads them at startup. The
//! file is the merged [`ProxyOptions`] for that project — the same struct
//! the CLI builds from `customizations.devconcurrent.proxy` in
//! `devcontainer.json`. No transformation, no separate wire struct.

use std::net::{IpAddr, Ipv4Addr};

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
/// Directory inside the proxy container where the mkcert CAROOT is
/// bind-mounted read-only when TLS is enabled.
pub const PROXY_CA_DIR: &str = "/etc/proxy-ca";
/// Directory inside each sidecar container where the proxy writes the per-
/// service plan and (if TLS is enabled) cert + key.
pub const SIDECAR_PLAN_DIR: &str = "/etc/sidecar";
pub const SIDECAR_PLAN_FILE: &str = "plan.json";
pub const SIDECAR_CERT_FILE: &str = "cert.pem";
pub const SIDECAR_KEY_FILE: &str = "key.pem";

// Environment variables read by the proxy on startup.
pub const ENV_DNS_PORT: &str = "DC_PROXY_DNS_PORT";
/// Set by the CLI when a CAROOT bind-mount is present. The proxy loads
/// `rootCA.pem` + `rootCA-key.pem` from this directory.
pub const ENV_CA_DIR: &str = "DC_PROXY_CA_DIR";

/// Default Handlebars template for proxied hostnames.
pub const DEFAULT_HOSTNAME_TEMPLATE: &str = "{{workspace}}.{{service}}.test";

/// Per-project proxy configuration.
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct ProxyOptions {
    /// Opt in to proxy routing for this project.
    pub enable: bool,

    /// Handlebars template for the proxied hostname, used by every service
    /// that does not set its own.
    ///
    /// Available variables:
    /// - `root` (bool) — whether this is the root workspace
    /// - `project` — project name
    /// - `workspace` — workspace name
    /// - `service` — name of the service from compose
    ///
    /// Default: {{workspace}}.{{service}}.test
    pub hostname: Option<Template>,

    /// Per-compose-service configuration.
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

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct ProxyService {
    /// Handlebars template for this service's hostname. Overrides the
    /// project-level `hostname`; same variables are available.
    pub hostname: Option<Template>,

    pub ports: Vec<ProxyPort>,
}

/// Port mapping for a single (host, container) pair on a service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ProxyPort {
    /// The IP address to listen on. Defaults to 0.0.0.0, allowing traffic in
    /// from any source.
    #[serde(default = "default_ip")]
    pub ip: IpAddr,
    pub host: u16,
    pub container: u16,
    /// Terminate TLS on `host` and forward plaintext to `container`. Requires
    /// `proxy.caRoot` to be configured.
    #[serde(default)]
    pub tls: bool,
}

fn default_ip() -> IpAddr {
    IpAddr::V4(Ipv4Addr::UNSPECIFIED)
}

impl<'de> Deserialize<'de> for ProxyPort {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            #[serde(default = "default_ip")]
            ip: IpAddr,
            host: u16,
            container: u16,
            #[serde(default)]
            tls: bool,
        }

        let Raw {
            ip,
            host,
            container,
            tls,
        } = Raw::deserialize(deserializer)?;

        if tls && host == container {
            return Err(de::Error::custom(format!(
                "tls port mapping {host}:{container} has host == container; TLS termination requires a distinct host port (e.g. host: 443, container: {container})"
            )));
        }

        Ok(Self {
            ip,
            host,
            container,
            tls,
        })
    }
}

/// A Handlebars template, compiled at deserialization time so syntax errors
/// surface as config-load errors rather than at first use.
#[derive(Clone, Debug)]
pub struct Template {
    source: String,
    // TODO: Should we be using this? Currently it's used to ensure valida at deserialization time,
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
    pub ports: Vec<ProxyPort>,
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

    #[test]
    fn rejects_tls_with_same_port() {
        let err =
            serde_json::from_str::<ProxyPort>(r#"{"host": 443, "container": 443, "tls": true}"#)
                .unwrap_err()
                .to_string();
        assert!(err.contains("tls port"), "got: {err}");
    }

    #[test]
    fn accepts_tls_with_different_ports() {
        let p: ProxyPort =
            serde_json::from_str(r#"{"host": 443, "container": 3000, "tls": true}"#).unwrap();
        assert_eq!(p.host, 443);
        assert_eq!(p.container, 3000);
        assert!(p.tls);
    }

    #[test]
    fn allows_same_port_without_tls() {
        let p: ProxyPort = serde_json::from_str(r#"{"host": 3000, "container": 3000}"#).unwrap();
        assert_eq!(p.host, 3000);
        assert!(!p.tls);
    }

    fn plan(hostname: &str, ports: &[(u16, u16, bool)]) -> SidecarPlan {
        SidecarPlan {
            hostname: hostname.to_string(),
            ports: ports
                .iter()
                .map(|&(host, container, tls)| ProxyPort {
                    ip: default_ip(),
                    host,
                    container,
                    tls,
                })
                .collect(),
        }
    }

    #[test]
    fn plan_hash_is_deterministic_and_a_valid_label_value() {
        let plan = plan("feature.app.test", &[(443, 8080, true), (80, 8080, false)]);
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
        let base = plan("feature.app.test", &[(443, 8080, true)]).hash();
        assert_ne!(base, plan("other.app.test", &[(443, 8080, true)]).hash());
        assert_ne!(base, plan("feature.app.test", &[(443, 3000, true)]).hash());
        assert_ne!(base, plan("feature.app.test", &[(443, 8080, false)]).hash());
        assert_ne!(
            base,
            plan("feature.app.test", &[(443, 8080, true), (80, 8080, false)]).hash(),
        );
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
}
