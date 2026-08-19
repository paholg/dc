//! Registry credentials, read the way the docker CLI stores them.
//!
//! The daemon does the pull, so it needs the credentials themselves rather
//! than a reference to them: they travel in the `X-Registry-Auth` header as
//! base64url'd JSON. Where they come from is `~/.docker/config.json`, which
//! either holds them inline under `auths` or names a credential helper that
//! does.

use std::path::PathBuf;
use std::process::Stdio;

use base64::Engine;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

/// Docker Hub's key in `config.json`, and the `serveraddress` the daemon
/// expects for it. An image reference that names no registry means this one.
const HUB: &str = "https://index.docker.io/v1/";

/// Username a credential helper uses to say "the secret is an identity token,
/// not a password".
const TOKEN_USER: &str = "<token>";

/// One registry's credentials, in the shape the daemon parses.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct RegistryAuth {
    #[serde(rename = "username", skip_serializing_if = "str::is_empty")]
    username: String,
    #[serde(rename = "password", skip_serializing_if = "str::is_empty")]
    password: String,
    #[serde(rename = "identitytoken", skip_serializing_if = "str::is_empty")]
    identity_token: String,
    #[serde(rename = "serveraddress")]
    server_address: String,
}

impl RegistryAuth {
    /// The `X-Registry-Auth` header value.
    pub(crate) fn header_value(&self) -> String {
        let json = serde_json::to_vec(self).expect("a string struct serializes");
        base64::engine::general_purpose::URL_SAFE.encode(json)
    }

    fn is_empty(&self) -> bool {
        self.username.is_empty() && self.password.is_empty() && self.identity_token.is_empty()
    }
}

/// Credentials for pulling `image`, or `None` if the config names none —
/// which is the ordinary case for a public image.
///
/// Never an error: a missing, unreadable or malformed config only means an
/// anonymous pull, which is what happened before any of this existed.
pub(crate) async fn for_image(image: &str) -> Option<RegistryAuth> {
    let registry = registry_for(image);
    let config = DockerConfig::load().await?;
    let auth = config.credentials(&registry).await?;
    (!auth.is_empty()).then_some(auth)
}

/// The registry an image reference points at.
///
/// The first path component is a registry only if it looks like a host, so
/// `alpine` and `library/alpine` are Docker Hub's while `ghcr.io/o/i` and
/// `localhost:5000/i` are not.
fn registry_for(image: &str) -> String {
    let Some((first, _rest)) = image.split_once('/') else {
        return HUB.to_owned();
    };
    if first.contains('.') || first.contains(':') || first == "localhost" {
        first.to_owned()
    } else {
        HUB.to_owned()
    }
}

/// Compare registry keys the way docker does: `config.json` keys are written
/// with or without a scheme and trailing slash, and all spellings name the
/// same registry.
fn same_registry(a: &str, b: &str) -> bool {
    fn bare(s: &str) -> &str {
        let s = s
            .strip_prefix("https://")
            .or_else(|| s.strip_prefix("http://"))
            .unwrap_or(s);
        s.trim_end_matches('/')
    }

    bare(a) == bare(b)
}

/// The parts of `~/.docker/config.json` that say where credentials live.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DockerConfig {
    #[serde(default)]
    auths: IndexMap<String, ConfigAuth>,
    /// Helper to use for every registry without its own entry in
    /// `cred_helpers`.
    #[serde(default)]
    creds_store: String,
    /// Per-registry helper, which wins over `creds_store`.
    #[serde(default)]
    cred_helpers: IndexMap<String, String>,
}

/// One `auths` entry. Credentials are inline here only when no helper is in
/// play; otherwise the entry exists but is empty.
#[derive(Debug, Default, Deserialize)]
struct ConfigAuth {
    /// base64 of `username:password`.
    #[serde(default)]
    auth: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default, rename = "identitytoken")]
    identity_token: String,
}

impl DockerConfig {
    async fn load() -> Option<Self> {
        let path = config_path()?;
        let bytes = tokio::fs::read(&path).await.ok()?;
        match serde_json::from_slice(&bytes) {
            Ok(config) => Some(config),
            Err(error) => {
                tracing::debug!("ignoring unreadable {}: {error}", path.display());
                None
            }
        }
    }

    /// Credentials for `registry`, in docker's own order of preference: a
    /// helper named for this registry, then the global helper, then whatever
    /// the `auths` entry holds inline.
    async fn credentials(&self, registry: &str) -> Option<RegistryAuth> {
        if let Some(helper) = self.helper_for(registry) {
            return from_helper(helper, registry).await;
        }
        self.entry_for(registry).map(|entry| entry.decode(registry))
    }

    fn helper_for(&self, registry: &str) -> Option<&str> {
        let named = self
            .cred_helpers
            .iter()
            .find(|(key, _)| same_registry(key, registry))
            .map(|(_, helper)| helper.as_str());

        named.or((!self.creds_store.is_empty()).then_some(self.creds_store.as_str()))
    }

    fn entry_for(&self, registry: &str) -> Option<&ConfigAuth> {
        self.auths
            .iter()
            .find(|(key, _)| same_registry(key, registry))
            .map(|(_, entry)| entry)
    }
}

impl ConfigAuth {
    fn decode(&self, registry: &str) -> RegistryAuth {
        let (mut username, mut password) = (self.username.clone(), self.password.clone());
        if let Some((user, pass)) = decode_basic(&self.auth) {
            username = user;
            password = pass;
        }
        RegistryAuth {
            username,
            password,
            identity_token: self.identity_token.clone(),
            server_address: registry.to_owned(),
        }
    }
}

/// Split an `auth` field — base64 of `username:password` — into its halves.
fn decode_basic(auth: &str) -> Option<(String, String)> {
    if auth.is_empty() {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(auth)
        .ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (user, pass) = decoded.split_once(':')?;
    Some((user.to_owned(), pass.to_owned()))
}

/// What `docker-credential-<helper> get` prints.
#[derive(Debug, Deserialize)]
struct HelperCredentials {
    #[serde(default, rename = "Username")]
    username: String,
    #[serde(default, rename = "Secret")]
    secret: String,
}

/// Ask a credential helper for `registry`'s credentials.
///
/// A helper that isn't installed, fails, or has nothing stored for this
/// registry all mean the same thing to us: pull anonymously.
async fn from_helper(helper: &str, registry: &str) -> Option<RegistryAuth> {
    let program = format!("docker-credential-{helper}");
    let mut child = tokio::process::Command::new(&program)
        .arg("get")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .inspect_err(|error| tracing::debug!("could not run {program}: {error}"))
        .ok()?;

    // The helper reads the registry to look up, then waits for EOF, so the
    // pipe has to be closed before its output can be collected.
    {
        let mut stdin = child.stdin.take()?;
        stdin.write_all(registry.as_bytes()).await.ok()?;
    }

    let output = child.wait_with_output().await.ok()?;
    if !output.status.success() {
        tracing::debug!("{program} has no credentials for {registry}");
        return None;
    }

    helper_output_to_auth(&output.stdout, registry)
}

/// Turn a helper's JSON answer into credentials.
///
/// A helper reports an OAuth identity token by naming `<token>` as the
/// username; the daemon wants that in `identitytoken`, not as a password.
fn helper_output_to_auth(stdout: &[u8], registry: &str) -> Option<RegistryAuth> {
    let credentials: HelperCredentials = serde_json::from_slice(stdout).ok()?;
    let mut auth = RegistryAuth {
        server_address: registry.to_owned(),
        ..RegistryAuth::default()
    };
    if credentials.username == TOKEN_USER {
        auth.identity_token = credentials.secret;
    } else {
        auth.username = credentials.username;
        auth.password = credentials.secret;
    }
    Some(auth)
}

fn config_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("DOCKER_CONFIG") {
        return Some(PathBuf::from(dir).join("config.json"));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".docker").join("config.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(json: &str) -> DockerConfig {
        serde_json::from_str(json).expect("deserialize")
    }

    #[test]
    fn an_unqualified_reference_is_docker_hub() {
        assert_eq!(registry_for("alpine"), HUB);
        assert_eq!(registry_for("alpine:3.20"), HUB);
        assert_eq!(registry_for("library/alpine"), HUB);
    }

    #[test]
    fn a_host_shaped_first_component_is_the_registry() {
        assert_eq!(registry_for("ghcr.io/paholg/proxy:1"), "ghcr.io");
        assert_eq!(registry_for("localhost:5000/proxy"), "localhost:5000");
        assert_eq!(registry_for("localhost/proxy"), "localhost");
    }

    #[test]
    fn registry_keys_compare_without_scheme_or_trailing_slash() {
        assert!(same_registry("https://index.docker.io/v1/", HUB));
        assert!(same_registry("ghcr.io", "https://ghcr.io/"));
        assert!(!same_registry("ghcr.io", "gcr.io"));
    }

    #[tokio::test]
    async fn inline_credentials_are_decoded() {
        // base64("me:hunter2")
        let config = config(r#"{"auths":{"ghcr.io":{"auth":"bWU6aHVudGVyMg=="}}}"#);
        let auth = config.credentials("ghcr.io").await.expect("credentials");
        assert_eq!(
            auth,
            RegistryAuth {
                username: "me".to_owned(),
                password: "hunter2".to_owned(),
                identity_token: String::new(),
                server_address: "ghcr.io".to_owned(),
            }
        );
    }

    /// Hub's entry is conventionally written as a URL; a reference to it never
    /// is.
    #[tokio::test]
    async fn the_hub_entry_is_found_under_its_url_key() {
        let config =
            config(r#"{"auths":{"https://index.docker.io/v1/":{"auth":"bWU6aHVudGVyMg=="}}}"#);
        let auth = config
            .credentials(&registry_for("alpine"))
            .await
            .expect("credentials");
        assert_eq!(auth.username, "me");
    }

    #[tokio::test]
    async fn a_registry_with_no_entry_has_no_credentials() {
        let config = config(r#"{"auths":{"ghcr.io":{"auth":"bWU6aHVudGVyMg=="}}}"#);
        assert_eq!(config.credentials("gcr.io").await, None);
    }

    #[test]
    fn a_named_helper_wins_over_the_global_store() {
        let config = config(r#"{"credsStore":"desktop","credHelpers":{"ghcr.io":"gh"}}"#);
        assert_eq!(config.helper_for("ghcr.io"), Some("gh"));
        assert_eq!(config.helper_for("gcr.io"), Some("desktop"));
    }

    #[test]
    fn no_helper_configured() {
        let config = config(r#"{"auths":{}}"#);
        assert_eq!(config.helper_for("ghcr.io"), None);
    }

    #[test]
    fn a_helper_reporting_a_password() {
        let auth = helper_output_to_auth(
            br#"{"ServerURL":"ghcr.io","Username":"me","Secret":"hunter2"}"#,
            "ghcr.io",
        )
        .expect("credentials");
        assert_eq!(
            auth,
            RegistryAuth {
                username: "me".to_owned(),
                password: "hunter2".to_owned(),
                identity_token: String::new(),
                server_address: "ghcr.io".to_owned(),
            }
        );
    }

    #[test]
    fn a_helper_reporting_an_identity_token() {
        let auth = helper_output_to_auth(
            br#"{"ServerURL":"ghcr.io","Username":"<token>","Secret":"abc.def"}"#,
            "ghcr.io",
        )
        .expect("credentials");
        assert_eq!(
            auth,
            RegistryAuth {
                username: String::new(),
                password: String::new(),
                identity_token: "abc.def".to_owned(),
                server_address: "ghcr.io".to_owned(),
            }
        );
    }

    #[test]
    fn a_helper_that_answers_with_nonsense_yields_nothing() {
        assert_eq!(
            helper_output_to_auth(b"credentials not found", "ghcr.io"),
            None
        );
    }

    #[test]
    fn the_header_is_base64url_of_the_json() {
        let auth = RegistryAuth {
            username: "me".to_owned(),
            password: "hunter2".to_owned(),
            identity_token: String::new(),
            server_address: "ghcr.io".to_owned(),
        };
        let decoded = base64::engine::general_purpose::URL_SAFE
            .decode(auth.header_value())
            .expect("base64url");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&decoded).expect("json"),
            serde_json::json!({
                "username": "me",
                "password": "hunter2",
                "serveraddress": "ghcr.io",
            }),
        );
    }
}
