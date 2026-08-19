use bon::bon;
use indexmap::IndexMap;
use serde::Deserialize;

use crate::client::Docker;
use crate::error::Result;
use crate::filter::{Filter, FilterSliceExt};
use crate::request_ext::ReqwestExt;

/// A volume entry from `GET /volumes/{name}` or `GET /volumes`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Volume {
    pub name: String,
    pub driver: String,
    pub mountpoint: String,
    #[serde(default)]
    pub labels: IndexMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct VolumesResponse {
    #[serde(default)]
    volumes: Vec<Volume>,
}

#[bon]
impl Docker {
    /// `POST /volumes/create` — create a named volume.
    #[builder]
    pub async fn create_volume(
        &self,
        #[builder(start_fn)] name: &str,
        #[builder(field)] labels: IndexMap<String, String>,
    ) -> Result<Volume> {
        let url = self.url(["volumes", "create"])?;
        let body = serde_json::json!({ "Name": name, "Labels": labels });
        self.http().post(url).json(&body).try_send().await
    }

    /// `GET /volumes` — list volumes, optionally narrowed by label filters.
    #[builder]
    pub async fn list_volumes(
        &self,
        #[builder(field)] filters: Vec<Filter>,
    ) -> Result<Vec<Volume>> {
        let mut url = self.url(["volumes"])?;
        if !filters.is_empty() {
            url.query_pairs_mut()
                .append_pair("filters", &filters.to_docker_query());
        }
        let resp: VolumesResponse = self.http().get(url).try_send().await?;
        Ok(resp.volumes)
    }

    /// `DELETE /volumes/{name}` — remove a volume.
    #[builder]
    pub async fn remove_volume(
        &self,
        #[builder(start_fn)] name: &str,
        #[builder(default)] force: bool,
    ) -> Result<()> {
        let mut url = self.url(["volumes", name])?;
        if force {
            url.query_pairs_mut().append_pair("force", "true");
        }
        self.http().delete(url).try_send_empty().await
    }
}

impl<S: docker_create_volume_builder::State> DockerCreateVolumeBuilder<'_, '_, S> {
    pub fn with_label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(key.into(), value.into());
        self
    }
}

impl<S: docker_list_volumes_builder::State> DockerListVolumesBuilder<'_, S> {
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
}
