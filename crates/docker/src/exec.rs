use serde::{Deserialize, Deserializer};

use crate::client::Docker;
use crate::error::Result;
use crate::request_ext::ReqwestExt;

/// Result of `GET /exec/{id}/json` — i.e. `docker exec inspect`.
#[derive(Debug, Clone)]
pub struct ExecDetails {
    pub id: String,
    pub running: bool,
    /// Exit code; `None` while still running.
    pub exit_code: Option<i64>,
}

impl<'de> Deserialize<'de> for ExecDetails {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Raw {
            #[serde(rename = "ID")]
            id: String,
            running: bool,
            exit_code: Option<i64>,
        }

        let raw = Raw::deserialize(d)?;
        Ok(Self {
            id: raw.id,
            running: raw.running,
            // Podman reports an exit code on an exec that is still running,
            // where Docker sends null. Neither is meaningful until it exits.
            exit_code: (!raw.running).then_some(raw.exit_code).flatten(),
        })
    }
}

impl Docker {
    /// `GET /exec/{id}/json` — inspect an exec instance.
    ///
    /// Returns [`crate::Error::NotFound`] if the exec doesn't exist.
    pub async fn inspect_exec(&self, id: &str) -> Result<ExecDetails> {
        let url = self.url(&format!("exec/{id}/json"));
        self.http().get(url).try_send().await
    }
}
