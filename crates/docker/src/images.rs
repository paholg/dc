use bon::bon;
use futures_util::StreamExt;
use indexmap::IndexMap;
use serde::Deserialize;
use snafu::ResultExt;

use crate::client::Docker;
use crate::container::null_as_default;
use crate::error::{ApiSnafu, JsonSnafu, Result};
use crate::filter::{Filter, FilterSliceExt};
use crate::request_ext::ReqwestExt;

/// Subset of `GET /images/{name}/json`
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ImageDetails {
    pub id: String,
    #[serde(default)]
    pub repo_tags: Vec<String>,
    #[serde(default)]
    pub config: ImageConfig,
    /// OS the image was built for, e.g. `linux`.
    #[serde(default)]
    pub os: String,
    /// CPU architecture, e.g. `amd64`, `arm64`.
    #[serde(default)]
    pub architecture: String,
    /// Architecture variant, e.g. `v8` for `arm64`. Usually absent.
    #[serde(default)]
    pub variant: Option<String>,
}

impl ImageDetails {
    /// `os/architecture[/variant]`, the form `docker build --platform` wants.
    /// `None` if the daemon reported neither os nor architecture.
    #[must_use]
    pub fn platform(&self) -> Option<String> {
        let parts: Vec<&str> = [
            self.os.as_str(),
            self.architecture.as_str(),
            self.variant.as_deref().unwrap_or_default(),
        ]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect();

        (!parts.is_empty()).then(|| parts.join("/"))
    }
}

/// The `Config` block of an image inspect — the image's own defaults, which a
/// container inherits unless its create options override them.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ImageConfig {
    /// The image's default user, as written in the Dockerfile's `USER`. Empty
    /// when the image doesn't set one, which the daemon reports as root.
    #[serde(default, deserialize_with = "null_as_default")]
    pub user: String,
    #[serde(default, deserialize_with = "null_as_default")]
    pub env: Vec<String>,
}

impl ImageConfig {
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

/// An image entry from `GET /images/json`.
///
/// Note the naming asymmetry with [`ImageDetails`]: the list endpoint reports
/// labels at the top level, while inspect nests them under `Config`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ImageSummary {
    pub id: String,
    /// `repo:tag` for each name the image answers to. An image whose tags have
    /// all been reassigned reports `<none>:<none>` here rather than an empty
    /// list.
    #[serde(default, deserialize_with = "null_as_default")]
    pub repo_tags: Vec<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub labels: IndexMap<String, String>,
}

/// One progress event in the NDJSON stream returned by `POST /images/create`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullEvent {
    status: Option<String>,
    error: Option<String>,
    error_detail: Option<ErrorDetail>,
}

#[derive(Debug, Clone, Deserialize)]
struct ErrorDetail {
    message: String,
}

/// One event in the NDJSON stream returned by `POST /build`. Build output
/// arrives as `stream` lines; a failed build reports the reason here rather
/// than in the HTTP status, which is 200 either way.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuildEvent {
    stream: Option<String>,
    error: Option<String>,
    error_detail: Option<ErrorDetail>,
}

impl Docker {
    /// Pull the image if it isn't already present locally. No-op if it is.
    pub async fn ensure_image(&self, name: &str) -> Result<()> {
        match self.inspect_image(name).await {
            Ok(_) => Ok(()),
            Err(crate::Error::NotFound { .. }) => self.pull_image(name).await,
            Err(e) => Err(e),
        }
    }

    /// `GET /images/{name}/json` — inspect an image.
    ///
    /// Returns [`crate::Error::NotFound`] if the image isn't locally available.
    pub async fn inspect_image(&self, name: &str) -> Result<ImageDetails> {
        let url = self.url(["images", name, "json"])?;
        self.http().get(url).try_send().await
    }

    /// `POST /images/create?fromImage=<name>` — pull an image.
    ///
    /// Drains the daemon's NDJSON progress stream and only reports the final
    /// outcome; per-layer progress is dropped. If any line in the stream
    /// carries an error event, surface it as [`crate::Error::Api`].
    pub async fn pull_image(&self, name: &str) -> Result<()> {
        let mut url = self.url(["images", "create"])?;
        url.query_pairs_mut().append_pair("fromImage", name);

        let events: Vec<PullEvent> = self.http().post(url).try_send_ndjson().await?;
        for event in events {
            if event.error.is_some() || event.error_detail.is_some() {
                let message = event
                    .error_detail
                    .map(|d| d.message)
                    .or(event.error)
                    .unwrap_or_else(|| event.status.unwrap_or_default());
                return ApiSnafu {
                    status: 0u16,
                    message,
                }
                .fail();
            }
        }
        Ok(())
    }
}

#[bon]
impl Docker {
    /// `POST /build?t=<tag>` — build an image from a tar `context`.
    ///
    /// Both Docker and podman implement this endpoint, where `docker build`
    /// does not: the CLI reaches for buildx, whose `docker-container` driver
    /// boots a buildkit container that rootless podman refuses to start.
    ///
    /// The daemon reports progress as the build runs, so `on_output` is called
    /// with each line as it arrives rather than in one batch at the end.
    ///
    /// A build that fails reports it in that same stream, not the status code,
    /// which is 200 either way; both surface as [`crate::Error::Api`].
    #[builder]
    pub async fn build_image(
        &self,
        #[builder(start_fn)] tag: &str,
        #[builder(field)] labels: IndexMap<String, String>,
        #[builder(field)] build_args: IndexMap<String, String>,
        /// The build context, as a tar archive. For a build that needs nothing
        /// but a Dockerfile, see [`crate::build_single_file_tar`].
        context: Vec<u8>,
        /// Path of the Dockerfile within `context`.
        #[builder(default = "Dockerfile")]
        dockerfile: &str,
        /// `os/arch[/variant]`. Defaults to the daemon's own platform.
        platform: Option<&str>,
        /// Called with each line of build output, as it arrives.
        on_output: Option<&mut (dyn FnMut(&str) + Send)>,
    ) -> Result<()> {
        let mut url = self.url(["build"])?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("t", tag);
            pairs.append_pair("dockerfile", dockerfile);
            if let Some(platform) = platform {
                pairs.append_pair("platform", platform);
            }
            if !labels.is_empty() {
                pairs.append_pair("labels", &json_map(&labels));
            }
            if !build_args.is_empty() {
                pairs.append_pair("buildargs", &json_map(&build_args));
            }
        }

        let response = self
            .http()
            .post(url)
            .header("Content-Type", "application/x-tar")
            .body(context)
            .try_send_streaming()
            .await?;

        let mut on_output = on_output;
        let mut report = |line: &str| {
            if let Some(sink) = on_output.as_mut() {
                sink(line);
            }
        };

        let mut stream = response.bytes_stream();
        let mut pending = Vec::new();
        while let Some(chunk) = stream.next().await {
            pending.extend_from_slice(&chunk?);
            // The daemon emits one JSON object per line, but a chunk boundary
            // can land anywhere, so only whole lines are ready to parse.
            while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = pending.drain(..=end).collect();
                handle_build_line(&line, &mut report)?;
            }
        }
        handle_build_line(&pending, &mut report)
    }
}

impl<S: docker_build_image_builder::State> DockerBuildImageBuilder<'_, '_, '_, '_, '_, S> {
    pub fn with_label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(key.into(), value.into());
        self
    }

    pub fn with_build_arg(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.build_args.insert(key.into(), value.into());
        self
    }
}

/// Docker takes these query parameters as a JSON object rather than repeated
/// pairs.
fn json_map(map: &IndexMap<String, String>) -> String {
    serde_json::to_string(map).expect("a string map serializes")
}

/// Parse one line of the build stream, reporting its output and turning a build
/// failure into an error. Blank lines are skipped.
fn handle_build_line(line: &[u8], report: &mut impl FnMut(&str)) -> Result<()> {
    if line.iter().all(u8::is_ascii_whitespace) {
        return Ok(());
    }

    let event: BuildEvent = serde_json::from_slice(line).with_context(|_| JsonSnafu {
        body: String::from_utf8_lossy(line).into_owned(),
    })?;

    if event.error.is_some() || event.error_detail.is_some() {
        let message = event
            .error_detail
            .map(|detail| detail.message)
            .or(event.error)
            .unwrap_or_else(|| event.stream.unwrap_or_default());
        return ApiSnafu {
            status: 0u16,
            message,
        }
        .fail();
    }

    if let Some(text) = event.stream {
        for line in text.lines() {
            report(line);
        }
    }
    Ok(())
}

#[bon]
impl Docker {
    /// `GET /images/json` — list images, optionally narrowed by label filters.
    ///
    /// Only tagged, top-level images by default, matching `docker images`.
    /// Filters are added via [`.with_label()`] on the returned builder.
    ///
    /// [`.with_label()`]: DockerListImagesBuilder::with_label
    #[builder]
    pub async fn list_images(
        &self,
        #[builder(field)] filters: Vec<Filter>,
        /// Include intermediate layers and untagged images.
        #[builder(default)]
        all: bool,
    ) -> Result<Vec<ImageSummary>> {
        let mut url = self.url(["images", "json"])?;

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
    /// `DELETE /images/{name}` — remove an image.
    ///
    /// Returns [`crate::Error::NotFound`] if the image doesn't exist.
    ///
    /// Untagging is not deletion: an image with several tags loses only the
    /// named one, and the daemon still reports success.
    #[builder]
    pub async fn remove_image(
        &self,
        #[builder(start_fn)] name: &str,
        /// Remove even if the image is tagged more than once or is in use by a
        /// stopped container.
        #[builder(default)]
        force: bool,
        /// Leave untagged parent images behind.
        #[builder(default)]
        noprune: bool,
    ) -> Result<()> {
        let mut url = self.url(["images", name])?;
        {
            let mut pairs = url.query_pairs_mut();
            if force {
                pairs.append_pair("force", "true");
            }
            if noprune {
                pairs.append_pair("noprune", "true");
            }
        }
        self.http().delete(url).try_send_empty().await
    }
}

impl<S: docker_list_images_builder::State> DockerListImagesBuilder<'_, S> {
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
