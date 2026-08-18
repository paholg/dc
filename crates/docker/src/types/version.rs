use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::error::{Error, IncompatibleApiVersionSnafu, Result};
use crate::request_ext::ReqwestExt;

/// A Docker Engine API version, e.g. `1.44`.
///
/// All API endpoints (other than `/version` itself) are prefixed by the
/// version: `/v1.44/containers/json` etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ApiVersion {
    pub major: u8,
    pub minor: u8,
}

impl ApiVersion {
    #[must_use]
    pub const fn new(major: u8, minor: u8) -> Self {
        Self { major, minor }
    }

    /// Pick the highest mutually-supported API version against `daemon`, or
    /// error if the result falls below the daemon's minimum.
    ///
    /// Concretely: `min(self, daemon.api_version)`, checked against
    /// `daemon.min_api_version`.
    pub fn negotiate(self, daemon: &DaemonVersion) -> Result<Self> {
        let chosen = self.min(daemon.api_version);
        if chosen < daemon.min_api_version {
            return IncompatibleApiVersionSnafu {
                our_max: self,
                daemon_min: daemon.min_api_version,
                daemon_max: daemon.api_version,
            }
            .fail();
        }
        Ok(chosen)
    }
}

/// Daemon's reported supported API range, returned by `GET /version`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct DaemonVersion {
    pub api_version: ApiVersion,
    // PascalCase would give `MinApiVersion`; field is `MinAPIVersion`.
    #[serde(rename = "MinAPIVersion")]
    pub min_api_version: ApiVersion,
    /// Docker reports an `Engine` component here; podman a `Podman Engine`
    /// one. Defaulted because the field postdates the endpoint itself.
    #[serde(default)]
    pub components: Vec<Component>,
}

/// One entry of `/version`'s `Components` list.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Component {
    pub name: String,
}

impl DaemonVersion {
    /// `GET /version` (unversioned) on the daemon.
    pub async fn probe(http: &reqwest::Client, base: &reqwest::Url) -> Result<Self> {
        let url = base.join("version").expect("base URL valid");
        http.get(url).try_send().await
    }

    /// Whether the daemon is podman serving the Docker-compatible API.
    ///
    /// Podman has named its engine component `Podman Engine` since the compat
    /// API existed, so a missing or unrecognized list means docker.
    #[must_use]
    pub fn is_podman(&self) -> bool {
        self.components
            .iter()
            .any(|c| c.name.to_lowercase().contains("podman"))
    }
}

impl fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

impl FromStr for ApiVersion {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let invalid = |reason: &str| Error::InvalidApiVersion {
            input: s.to_owned(),
            reason: reason.to_owned(),
        };
        let (maj, min) = s.split_once('.').ok_or_else(|| invalid("missing '.'"))?;
        let major = maj.parse().map_err(|_| invalid("major is not a u8"))?;
        let minor = min.parse().map_err(|_| invalid("minor is not a u8"))?;
        Ok(Self { major, minor })
    }
}

impl<'de> Deserialize<'de> for ApiVersion {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(de::Error::custom)
    }
}

impl Serialize for ApiVersion {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_str(&self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_well_formed() {
        assert_eq!(
            "1.44".parse::<ApiVersion>().unwrap(),
            ApiVersion::new(1, 44)
        );
        assert_eq!("0.0".parse::<ApiVersion>().unwrap(), ApiVersion::new(0, 0));
    }

    #[test]
    fn rejects_missing_dot() {
        assert!("144".parse::<ApiVersion>().is_err());
    }

    #[test]
    fn rejects_non_numeric() {
        assert!("a.b".parse::<ApiVersion>().is_err());
    }

    #[test]
    fn ordering() {
        assert!(ApiVersion::new(1, 41) < ApiVersion::new(1, 44));
        assert!(ApiVersion::new(2, 0) > ApiVersion::new(1, 99));
    }

    #[test]
    fn display() {
        assert_eq!(ApiVersion::new(1, 44).to_string(), "1.44");
    }

    fn dv(min: (u8, u8), max: (u8, u8)) -> DaemonVersion {
        DaemonVersion {
            api_version: ApiVersion::new(max.0, max.1),
            min_api_version: ApiVersion::new(min.0, min.1),
            components: Vec::new(),
        }
    }

    /// Trimmed from real `/version` responses of each daemon.
    #[test]
    fn detects_the_daemon_flavor() {
        let docker: DaemonVersion = serde_json::from_str(
            r#"{"ApiVersion":"1.55","MinAPIVersion":"1.40",
                "Components":[{"Name":"Engine","Version":"29.6.2"},{"Name":"containerd","Version":"v2.3.3"}]}"#,
        )
        .unwrap();
        assert!(!docker.is_podman());

        let podman: DaemonVersion = serde_json::from_str(
            r#"{"ApiVersion":"1.41","MinAPIVersion":"1.24",
                "Components":[{"Name":"Podman Engine","Version":"5.2.2"},{"Name":"Conmon","Version":"conmon version 2.1.12"}]}"#,
        )
        .unwrap();
        assert!(podman.is_podman());
    }

    /// A daemon predating `Components` is treated as docker.
    #[test]
    fn missing_components_means_docker() {
        let bare: DaemonVersion =
            serde_json::from_str(r#"{"ApiVersion":"1.41","MinAPIVersion":"1.24"}"#).unwrap();
        assert!(!bare.is_podman());
    }

    #[test]
    fn negotiate_picks_our_max_when_daemon_is_newer() {
        let chosen = ApiVersion::new(1, 44)
            .negotiate(&dv((1, 40), (1, 54)))
            .unwrap();
        assert_eq!(chosen, ApiVersion::new(1, 44));
    }

    #[test]
    fn negotiate_steps_down_when_daemon_is_older() {
        let chosen = ApiVersion::new(1, 44)
            .negotiate(&dv((1, 24), (1, 41)))
            .unwrap();
        assert_eq!(chosen, ApiVersion::new(1, 41));
    }

    #[test]
    fn negotiate_rejects_daemon_too_new_for_us() {
        // Daemon requires at least v1.50, we cap at v1.44.
        let err = ApiVersion::new(1, 44)
            .negotiate(&dv((1, 50), (1, 60)))
            .unwrap_err();
        assert!(matches!(err, Error::IncompatibleApiVersion { .. }));
    }
}
