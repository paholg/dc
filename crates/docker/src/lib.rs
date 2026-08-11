//! Minimal client for the Docker / Podman Engine API.
//!
//! Talks to a local Unix socket via the versioned HTTP API. Pinned to API
//! v1.44 with negotiation: on connect we read `/version` from the daemon and
//! pick `min(OUR_MAX, daemon.ApiVersion)`. Both Docker and Podman's
//! Docker-compat endpoint are supported.

mod archive;
mod client;
mod container;
mod error;
mod events;
mod exec;
mod filter;
mod images;
mod request_ext;
mod socket;
mod stats;
mod types;
mod volumes;

#[cfg(feature = "docker-tests")]
pub mod test_support;

pub use archive::{build_archive, build_single_file_tar};
pub use client::Docker;
pub use container::{
    ContainerConfig, ContainerDetails, ContainerState, ContainerStatus, ContainerSummary,
    EndpointSettings, NetworkSettings, Port, PortType,
};
pub use error::{Error, Result};
pub use events::{EventActor, EventMessage, EventsBuilder};
pub use exec::ExecDetails;
pub use filter::Filter;
pub use images::ImageDetails;
pub use socket::discover_socket;
pub use stats::{ContainerStats, CpuStats, CpuUsage, MemoryStats};
pub use types::ApiVersion;
pub use volumes::Volume;

pub const LOCAL_FOLDER_LABEL: &str = "devcontainer.local_folder";
pub const CONFIG_FILE_LABEL: &str = "devcontainer.config_file";

pub const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";
pub const COMPOSE_SERVICE_LABEL: &str = "com.docker.compose.service";

// All containers started by devconcurrent should have this label.
pub const MANAGED_LABEL: &str = "com.paholg.devconcurrent.managed";

// Project labels.
pub const PROJECT_LABEL: &str = "com.paholg.devconcurrent.project";
pub const WORKSPACE_LABEL: &str = "com.paholg.devconcurrent.workspace";

// Forward sidecar labels.
pub const FORWARD_LABEL: &str = "com.paholg.devconcurrent.fwd";
pub const FORWARD_TARGET_LABEL: &str = "com.paholg.devconcurrent.fwd.target";

// Proxy labels
/// Label for all proxy containers (primary + sidecars).
pub const PROXY_GROUP_LABEL: &str = "com.paholg.devconcurrent.proxy.group";
pub const PROXY_LABEL: &str = "com.paholg.devconcurrent.proxy";
pub const PROXY_SIDECAR_LABEL: &str = "com.paholg.devconcurrent.proxy.sidecar";
/// Present on sidecars only. Value is the container id of the service the
/// sidecar is net-joined to.
pub const PROXY_TARGET_LABEL: &str = "com.paholg.devconcurrent.proxy.target";
/// Present on sidecars only. Value is the compose service name.
pub const PROXY_SERVICE_LABEL: &str = "com.paholg.devconcurrent.proxy.service";
/// Present on the primary proxy only. Value is a hash of everything the proxy
/// was created from; a mismatch means the proxy is stale and should be
/// recreated.
pub const PROXY_CONFIG_HASH_LABEL: &str = "com.paholg.devconcurrent.proxy.config-hash";
