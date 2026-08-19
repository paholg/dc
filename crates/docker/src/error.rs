use std::path::PathBuf;

use snafu::Snafu;

use crate::types::ApiVersion;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum Error {
    #[snafu(display("could not find docker/podman socket: tried {tried:?}"))]
    SocketNotFound { tried: Vec<PathBuf> },

    #[snafu(display("DOCKER_HOST {host:?} is not a unix:// URI"))]
    NonUnixHost { host: String },

    #[snafu(display(
        "incompatible API version: this client wants v{our_max}, daemon supports v{daemon_min} through v{daemon_max}"
    ))]
    IncompatibleApiVersion {
        our_max: ApiVersion,
        daemon_min: ApiVersion,
        daemon_max: ApiVersion,
    },

    #[snafu(display("could not parse API version {input:?}: {reason}"))]
    InvalidApiVersion { input: String, reason: String },

    #[snafu(display("HTTP transport"))]
    Transport { source: reqwest::Error },

    #[snafu(display("docker API returned {status}: {message}"))]
    Api { status: u16, message: String },

    #[snafu(display("not found"))]
    NotFound,

    #[snafu(display("{segment:?} cannot be used as a URL path segment"))]
    InvalidPathSegment { segment: String },

    #[snafu(display("failed to decode JSON response: {body}"))]
    Json {
        source: serde_json::Error,
        body: String,
    },

    #[snafu(display("io error"))]
    Io { source: std::io::Error },
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<reqwest::Error> for Error {
    fn from(source: reqwest::Error) -> Self {
        Self::Transport { source }
    }
}
