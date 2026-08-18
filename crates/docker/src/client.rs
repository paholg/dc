use std::path::{Path, PathBuf};

use reqwest::Url;
use snafu::ResultExt;

use crate::error::{Result, TransportSnafu};
use crate::socket::discover_socket;
use crate::types::{ApiVersion, DaemonVersion};

/// The newest API version this crate is written against.
///
/// Docker daemons supporting v1.44 are Engine 24.0 (June 2023) or newer.
/// Older daemons (e.g. podman with max v1.41) will negotiate down.
const MAX_VERSION: ApiVersion = ApiVersion::new(1, 44);

/// Synthetic host used in URLs; ignored by reqwest when `unix_socket` is set.
const HOST: &str = "docker";

/// A client for the Docker (or podman-as-docker) Engine API.
///
/// On `connect`, this discovers the daemon socket, queries `/version`, and
/// negotiates an API version. The negotiated version is cached on the client
/// and used as the URL prefix for all subsequent requests.
#[derive(Clone)]
pub struct Docker {
    socket: PathBuf,
    api_version: ApiVersion,
    podman: bool,
    http: reqwest::Client,
    base_url: Url,
}

impl Docker {
    /// Discover the daemon socket via [`discover_socket`] and connect.
    pub async fn connect() -> Result<Self> {
        let socket = discover_socket().await?;
        Self::connect_with_socket(socket).await
    }

    /// Connect via a specific Unix socket path.
    pub async fn connect_with_socket(socket: PathBuf) -> Result<Self> {
        let http = reqwest::Client::builder()
            .unix_socket(socket.clone())
            .build()
            .context(TransportSnafu)?;
        let root: Url = format!("http://{HOST}/")
            .parse()
            .expect("static URL is valid");
        let daemon = DaemonVersion::probe(&http, &root).await?;
        let api_version = MAX_VERSION.negotiate(&daemon)?;
        let base_url = root
            .join(&format!("v{api_version}/"))
            .expect("versioned URL joins onto root");
        Ok(Self {
            socket,
            api_version,
            podman: daemon.is_podman(),
            http,
            base_url,
        })
    }

    /// The socket this client is connected to.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The host path to bind-mount into a container so it can reach this
    /// daemon.
    #[must_use]
    pub fn socket_mount_source(&self) -> PathBuf {
        crate::socket::mount_source(&self.socket)
    }

    /// The API version negotiated with the daemon.
    #[must_use]
    pub fn api_version(&self) -> ApiVersion {
        self.api_version
    }

    /// Whether the daemon is podman serving the Docker-compatible API.
    #[must_use]
    pub fn is_podman(&self) -> bool {
        self.podman
    }

    pub(crate) fn url(&self, path: &str) -> Url {
        self.base_url
            .join(path)
            .expect("relative path joins onto base URL")
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }
}
