use std::fmt::Display;
use std::net::IpAddr;

use bon::bon;
use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::MANAGED_LABEL;
use crate::client::Docker;
use crate::error::Result;
use crate::filter::{Filter, FilterSliceExt};
use crate::request_ext::ReqwestExt;

/// Treat a JSON `null` as the type's `Default`. Docker is written in Go, whose
/// `encoding/json` marshals a nil map or slice as `null` rather than `{}` or
/// `[]`, so any collection in a response can arrive as `null`. Bare `default`
/// only handles missing fields.
pub(crate) fn null_as_default<'de, T, D>(d: D) -> std::result::Result<T, D::Error>
where
    T: Default + Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(d).map(Option::unwrap_or_default)
}

/// Treat a JSON empty string as `None`. Docker reports an unbound port's `IP`
/// as `""` rather than omitting the field, which would otherwise fail to parse
/// as an `IpAddr`.
fn empty_string_as_none<'de, T, D>(d: D) -> std::result::Result<Option<T>, D::Error>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
    D: Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(d)?;
    match opt.as_deref() {
        None | Some("") => Ok(None),
        Some(s) => s.parse().map(Some).map_err(serde::de::Error::custom),
    }
}

/// Serialize a sequence as a Go set: `map[T]struct{}`. JSON has no set, so Go
/// encodes one as an object whose values are empty objects; only the keys carry
/// meaning.
///
/// Keys must serialize as strings, since JSON has no other kind of key.
fn as_go_set<T, S>(items: &[T], serializer: S) -> std::result::Result<S::Ok, S::Error>
where
    T: Serialize,
    S: Serializer,
{
    #[derive(Serialize)]
    struct NoValue {}

    serializer.collect_map(items.iter().map(|item| (item, NoValue {})))
}

/// Result of `GET /containers/{id}/json` — i.e. `docker inspect`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerDetails {
    pub id: String,
    /// Id of the image the container is *running*, which is not the same thing
    /// as [`ContainerConfig::image`]: that is the reference as given at create
    /// time, and the tag it names may since have moved to a newer image.
    #[serde(default)]
    pub image: String,
    pub created: String,
    pub state: ContainerState,
    pub config: ContainerConfig,
    #[serde(default, deserialize_with = "null_as_default")]
    pub network_settings: NetworkSettings,
    #[serde(rename = "ExecIDs", default, deserialize_with = "null_as_default")]
    pub exec_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerState {
    pub status: ContainerStatus,
    pub running: bool,
    pub exit_code: i64,
}

/// Container state values as reported by Docker. Ordering reflects "liveness":
/// `Running` is highest, `Dead` is lowest. Callers that summarise across
/// several containers (e.g. workspace status) can rely on `Ord` to pick the
/// most-alive state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, strum::Display)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ContainerStatus {
    Created,
    Dead,
    Exited,
    Paused,
    Removing,
    Restarting,
    Running,
    Stopping,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerConfig {
    /// Image reference as given at create time (e.g. `ghcr.io/foo/bar:1.2.3`).
    #[serde(default)]
    pub image: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub env: Vec<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub labels: IndexMap<String, String>,
}

impl ContainerConfig {
    /// Parse [`Self::env`] entries (`"KEY=VALUE"`) into a map. Entries missing
    /// `=` are skipped.
    #[must_use]
    pub fn parsed_env(&self) -> IndexMap<String, String> {
        self.env
            .iter()
            .filter_map(|pair| {
                let (key, value) = pair.split_once('=')?;
                Some((key.to_string(), value.to_string()))
            })
            .collect()
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct NetworkSettings {
    #[serde(default, deserialize_with = "null_as_default")]
    pub networks: IndexMap<String, EndpointSettings>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct EndpointSettings {
    #[serde(rename = "IPAddress")]
    pub ip_address: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerSummary {
    pub id: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub names: Vec<String>,
    pub image: String,
    pub state: ContainerStatus,
    pub created: i64,
    #[serde(default, deserialize_with = "null_as_default")]
    pub labels: IndexMap<String, String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub ports: Vec<Port>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub network_settings: NetworkSettings,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Port {
    #[serde(rename = "IP", default, deserialize_with = "empty_string_as_none")]
    pub ip: Option<IpAddr>,
    pub private_port: u16,
    pub public_port: Option<u16>,
    #[serde(rename = "Type")]
    pub kind: PortType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PortType {
    Tcp,
    Udp,
    Sctp,
}

impl Docker {
    /// `GET /containers/{id}/json` — inspect a container.
    ///
    /// Returns [`Error::NotFound`] if the container doesn't exist (so callers
    /// can `match` on it).
    pub async fn inspect_container(&self, id: &str) -> Result<ContainerDetails> {
        let url = self.url(["containers", id, "json"])?;
        self.http().get(url).try_send().await
    }

    /// `POST /containers/{id}/start` — start a stopped container.
    ///
    /// Starting a container that is already running is not an error: the
    /// daemon answers 304, which this reports as success.
    pub async fn start_container(&self, id: &str) -> Result<()> {
        let url = self.url(["containers", id, "start"])?;
        self.http().post(url).try_send_empty_or_unchanged().await
    }
}

#[bon]
impl Docker {
    /// `GET /containers/json` — list containers, with optional filters.
    ///
    /// `all = false` (the default) returns only running containers, matching
    /// the `docker ps` default. Filters are added via [`.with_label()`],
    /// [`.with_status()`], etc. on the returned builder.
    ///
    /// [`.with_label()`]: DockerListContainersBuilder::with_label
    /// [`.with_status()`]: DockerListContainersBuilder::with_status
    #[builder]
    pub async fn list_containers(
        &self,
        #[builder(field)] filters: Vec<Filter>,
        #[builder(default)] all: bool,
    ) -> Result<Vec<ContainerSummary>> {
        let mut url = self.url(["containers", "json"])?;
        {
            let mut pairs = url.query_pairs_mut();
            if all {
                pairs.append_pair("all", "true");
            }
            if !filters.is_empty() {
                pairs.append_pair("filters", &filters.to_docker_query());
            }
        }
        self.http().get(url).try_send().await
    }
}

#[bon]
impl Docker {
    /// `DELETE /containers/{id}` — remove a container.
    ///
    /// Returns [`Error::NotFound`] if the container doesn't exist.
    #[builder]
    pub async fn remove_container(
        &self,
        #[builder(start_fn)] id: &str,
        #[builder(default)] force: bool,
        /// Remove anonymous volumes associated with the container.
        #[builder(default)]
        volumes: bool,
        /// Remove the specified link associated with the container.
        #[builder(default)]
        link: bool,
    ) -> Result<()> {
        let mut url = self.url(["containers", id])?;
        {
            let mut pairs = url.query_pairs_mut();
            if force {
                pairs.append_pair("force", "true");
            }
            if volumes {
                pairs.append_pair("v", "true");
            }
            if link {
                pairs.append_pair("link", "true");
            }
        }
        self.http().delete(url).try_send_empty().await
    }
}

impl<S: docker_list_containers_builder::State> DockerListContainersBuilder<'_, S> {
    pub fn with_label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.filters.push(Filter::Label {
            key: key.into(),
            value: Some(value.into()),
        });
        self
    }

    pub fn with_label_key(mut self, key: impl Into<String>) -> Self {
        self.filters.push(Filter::Label {
            key: key.into(),
            value: None,
        });
        self
    }

    pub fn with_status(mut self, status: ContainerStatus) -> Self {
        self.filters.push(Filter::Status(status));
        self
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.filters.push(Filter::Id(id.into()));
        self
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.filters.push(Filter::Name(name.into()));
        self
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct PortBindingEntry {
    host_ip: IpAddr,
    // Docker expects this as a string :(
    #[serde(serialize_with = "as_display_string")]
    host_port: u16,
}

fn as_display_string<T: std::fmt::Display, S: Serializer>(
    value: &T,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct CreateRequest<'a> {
    image: &'a str,
    labels: &'a IndexMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    entrypoint: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cmd: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    env: Option<&'a [String]>,
    /// The `port/protocol` of every port we bind.
    #[serde(skip_serializing_if = "<[_]>::is_empty", serialize_with = "as_go_set")]
    exposed_ports: Vec<&'a str>,
    host_config: HostConfig<'a>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct HostConfig<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    binds: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    port_bindings: Option<&'a IndexMap<String, Vec<PortBindingEntry>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    network_mode: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct CreateResponse {
    id: String,
    #[serde(default, deserialize_with = "null_as_default")]
    warnings: Vec<String>,
}

#[bon]
impl Docker {
    /// `POST /containers/create?name=<name>` — create (but don't start) a
    /// container.
    ///
    /// Returns the new container ID. Surface-area is intentionally narrow:
    /// only the options dc actually uses today (labels, binds, port bindings,
    /// network mode, entrypoint, cmd). Add more as needed.
    #[builder]
    pub async fn create_container(
        &self,
        #[builder(start_fn)] name: &str,
        #[builder(field)] mut labels: IndexMap<String, String>,
        #[builder(field)] binds: Vec<String>,
        #[builder(field)] env: Vec<String>,
        #[builder(field)] port_bindings: IndexMap<String, Vec<PortBindingEntry>>,
        image: &str,
        #[builder(default)] entrypoint: Vec<String>,
        #[builder(default)] cmd: Vec<String>,
        network_mode: Option<&str>,
    ) -> Result<String> {
        let mut url = self.url(["containers", "create"])?;
        url.query_pairs_mut().append_pair("name", name);

        labels.insert(MANAGED_LABEL.to_string(), "true".to_string());

        let body = CreateRequest {
            image,
            labels: &labels,
            entrypoint: (!entrypoint.is_empty()).then_some(&entrypoint),
            cmd: (!cmd.is_empty()).then_some(&cmd),
            env: (!env.is_empty()).then_some(&env),
            exposed_ports: port_bindings.keys().map(String::as_str).collect(),
            host_config: HostConfig {
                binds: (!binds.is_empty()).then_some(&binds),
                port_bindings: (!port_bindings.is_empty()).then_some(&port_bindings),
                network_mode,
            },
        };

        let resp: CreateResponse = self.http().post(url).json(&body).try_send().await?;
        for warning in resp.warnings {
            tracing::warn!(name = %name, "docker container create: {warning}");
        }
        Ok(resp.id)
    }
}

impl<S: docker_create_container_builder::State> DockerCreateContainerBuilder<'_, '_, '_, '_, S> {
    pub fn with_label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(key.into(), value.into());
        self
    }

    pub fn with_bind(mut self, source: impl Display, dest: impl Display) -> Self {
        self.binds.push(format!("{source}:{dest}"));
        self
    }

    pub fn with_ro_bind(mut self, source: impl Display, dest: impl Display) -> Self {
        self.binds.push(format!("{source}:{dest}:ro"));
        self
    }

    pub fn with_env(mut self, key: impl Display, value: impl Display) -> Self {
        self.env.push(format!("{key}={value}"));
        self
    }

    /// Publish `container_port/tcp` to `host_ip:host_port` on the host.
    pub fn with_tcp_port_binding(
        self,
        container_port: u16,
        host_ip: IpAddr,
        host_port: u16,
    ) -> Self {
        self.with_port_binding(container_port, "tcp", host_ip, host_port)
    }

    /// Publish `container_port/udp` to `host_ip:host_port` on the host.
    pub fn with_udp_port_binding(
        self,
        container_port: u16,
        host_ip: IpAddr,
        host_port: u16,
    ) -> Self {
        self.with_port_binding(container_port, "udp", host_ip, host_port)
    }

    fn with_port_binding(
        mut self,
        container_port: u16,
        protocol: &str,
        host_ip: IpAddr,
        host_port: u16,
    ) -> Self {
        self.port_bindings
            .entry(format!("{container_port}/{protocol}"))
            .or_default()
            .push(PortBindingEntry { host_ip, host_port });
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_empty_ip_is_none() {
        let port: Port =
            serde_json::from_str(r#"{"IP":"","PrivatePort":80,"PublicPort":8080,"Type":"tcp"}"#)
                .expect("deserialize");
        assert_eq!(port.ip, None);
    }

    #[test]
    fn port_missing_ip_is_none() {
        let port: Port =
            serde_json::from_str(r#"{"PrivatePort":80,"Type":"tcp"}"#).expect("deserialize");
        assert_eq!(port.ip, None);
    }

    #[test]
    fn port_parses_ip() {
        let port: Port = serde_json::from_str(
            r#"{"IP":"0.0.0.0","PrivatePort":80,"PublicPort":8080,"Type":"tcp"}"#,
        )
        .expect("deserialize");
        assert_eq!(port.ip, Some("0.0.0.0".parse().unwrap()));
    }

    /// A proxy sidecar joins its target's netns and declares no ports, so
    /// Docker reports these collections as `null`.
    #[test]
    fn container_summary_null_collections_are_empty() {
        let summary: ContainerSummary = serde_json::from_str(
            r#"{
                "Id": "a9231c73f6cc",
                "Names": null,
                "Image": "ghcr.io/paholg/devconcurrent-proxy:0.0.20",
                "State": "running",
                "Created": 1786469264,
                "Labels": null,
                "Ports": null,
                "NetworkSettings": null
            }"#,
        )
        .expect("deserialize");

        assert!(summary.names.is_empty());
        assert!(summary.labels.is_empty());
        assert!(summary.ports.is_empty());
        assert!(summary.network_settings.networks.is_empty());
    }

    /// A bound port has to be declared in `ExposedPorts` too. The proxy binds
    /// one port as both UDP and TCP, so the protocol has to survive into the
    /// set — keying on the number alone would lose one.
    #[test]
    fn bound_ports_are_also_exposed_per_protocol() {
        let labels = IndexMap::new();
        let mut port_bindings = IndexMap::new();
        for key in ["53/udp", "53/tcp"] {
            port_bindings.insert(
                key.to_string(),
                vec![PortBindingEntry {
                    host_ip: "127.0.0.1".parse().unwrap(),
                    host_port: 53,
                }],
            );
        }

        let body = serde_json::to_value(CreateRequest {
            image: "alpine",
            labels: &labels,
            entrypoint: None,
            cmd: None,
            env: None,
            exposed_ports: port_bindings.keys().map(String::as_str).collect(),
            host_config: HostConfig {
                binds: None,
                port_bindings: Some(&port_bindings),
                network_mode: None,
            },
        })
        .expect("serialize");

        assert_eq!(
            body["ExposedPorts"],
            serde_json::json!({"53/udp": {}, "53/tcp": {}}),
        );
    }

    /// A container with no bindings must not send an empty `ExposedPorts`.
    #[test]
    fn a_container_without_bindings_exposes_nothing() {
        let labels = IndexMap::new();
        let body = serde_json::to_value(CreateRequest {
            image: "alpine",
            labels: &labels,
            entrypoint: None,
            cmd: None,
            env: None,
            exposed_ports: Vec::new(),
            host_config: HostConfig {
                binds: None,
                port_bindings: None,
                network_mode: None,
            },
        })
        .expect("serialize");

        assert!(body.get("ExposedPorts").is_none());
    }
}
