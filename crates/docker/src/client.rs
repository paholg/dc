use std::path::{Path, PathBuf};

use reqwest::Url;
use reqwest::redirect::Policy;
use snafu::ResultExt;

use crate::error::{InvalidPathSegmentSnafu, Result, TransportSnafu};
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
            // The daemon path-cleans before routing, so a request it considers
            // malformed comes back as a 301 to a *different* endpoint. Never
            // follow that: report it instead of silently calling something the
            // caller didn't ask for.
            .redirect(Policy::none())
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

    /// Build a URL under the negotiated API prefix from its path segments.
    ///
    /// Segments are taken one at a time and percent-encoded, so an id or name
    /// carrying `/`, `?` or `#` names a single path element instead of
    /// re-routing the request to another endpoint.
    pub(crate) fn url<'a>(&self, segments: impl IntoIterator<Item = &'a str>) -> Result<Url> {
        encoded_url(&self.base_url, segments)
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.http
    }
}

/// Append `segments` to `base`, one percent-encoded path element each.
///
/// Dot segments are rejected rather than encoded. `url` resolves them the way
/// a browser would, and `%2E` is a dot segment as far as the URL spec is
/// concerned, so there is no spelling of `..` that survives as an ordinary
/// name. No docker id, name or tag is `.` or `..`, and an empty segment
/// collapses the same way, so refusing all three costs nothing.
fn encoded_url<'a>(base: &Url, segments: impl IntoIterator<Item = &'a str>) -> Result<Url> {
    let mut url = base.clone();
    {
        let mut path = url
            .path_segments_mut()
            .expect("the base URL has a host, so it can be a base");
        // The base ends in `/`, i.e. one empty trailing segment.
        path.pop_if_empty();
        for segment in segments {
            if matches!(segment, "" | "." | "..") {
                return InvalidPathSegmentSnafu { segment }.fail();
            }
            path.push(segment);
        }
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        "http://docker/v1.44/".parse().expect("static URL is valid")
    }

    fn url<'a>(segments: impl IntoIterator<Item = &'a str>) -> String {
        encoded_url(&base(), segments)
            .expect("segments are valid")
            .to_string()
    }

    #[test]
    fn plain_segments_join() {
        assert_eq!(
            url(["containers", "abc123", "json"]),
            "http://docker/v1.44/containers/abc123/json"
        );
    }

    /// An image reference is one segment, slashes and all.
    #[test]
    fn slashes_stay_inside_the_segment() {
        assert_eq!(
            url(["images", "ghcr.io/paholg/proxy:1.2.3", "json"]),
            "http://docker/v1.44/images/ghcr.io%2Fpaholg%2Fproxy:1.2.3/json"
        );
    }

    /// The bug this encoding exists for: an unescaped name could add query
    /// parameters, or point the request at another endpoint entirely.
    #[test]
    fn a_hostile_name_cannot_reroute_the_request() {
        assert_eq!(
            url(["containers", "../volumes/x?force=true"]),
            "http://docker/v1.44/containers/..%2Fvolumes%2Fx%3Fforce=true"
        );
        assert_eq!(
            url(["containers", "abc#frag"]),
            "http://docker/v1.44/containers/abc%23frag"
        );
        assert_eq!(
            url(["containers", "already%2Fencoded"]),
            "http://docker/v1.44/containers/already%252Fencoded"
        );
    }

    #[test]
    fn dot_segments_are_refused() {
        for segment in ["", ".", ".."] {
            let err = encoded_url(&base(), ["containers", segment, "json"])
                .expect_err("dot segment rejected");
            assert!(
                matches!(err, crate::Error::InvalidPathSegment { .. }),
                "got {err:?}"
            );
        }
    }
}
