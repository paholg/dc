use std::path::{Path, PathBuf};

use eyre::WrapErr;
use figment::{
    Figment,
    providers::{Format, Json, Serialized},
};
use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_inline_default::serde_inline_default;
use serde_with::{OneOrMany, serde_as};

pub(crate) mod dc_options;
pub(crate) mod forward_port;
pub(crate) mod lifecycle_command;
pub(crate) mod substitution;
mod unsupported;

use crate::{
    config::Project,
    devcontainer::{dc_options::DcOptions, forward_port::ForwardPort, substitution::Template},
    docker::probe,
};
use lifecycle_command::LifecycleCommandTemplate;
use unsupported::Unsupported;

/// The `devcontainer.*` labels we stamp on the primary service container.
///
/// They are what the spec's tooling identifies a dev container by, and the
/// input to `${devcontainerId}`. We write them ourselves in the compose
/// override, so the id is known before any container exists.
#[derive(Debug, Clone)]
pub(crate) struct DevcontainerLabels {
    local_folder: PathBuf,
    config_file: Option<PathBuf>,
}

impl DevcontainerLabels {
    pub(crate) fn new(local_folder: PathBuf, config_file: Option<PathBuf>) -> Self {
        Self {
            local_folder,
            config_file,
        }
    }

    pub(crate) fn local_folder(&self) -> &Path {
        &self.local_folder
    }

    pub(crate) fn config_file(&self) -> Option<&Path> {
        self.config_file.as_deref()
    }

    pub(crate) fn pairs(&self) -> Vec<(&'static str, String)> {
        let mut pairs = vec![(
            docker::LOCAL_FOLDER_LABEL,
            self.local_folder.display().to_string(),
        )];
        if let Some(config_file) = &self.config_file {
            pairs.push((docker::CONFIG_FILE_LABEL, config_file.display().to_string()));
        }
        pairs
    }

    pub(crate) fn devcontainer_id(&self) -> String {
        let pairs = self.pairs();
        probe::devcontainer_id(pairs.iter().map(|(key, value)| (*key, value.as_str())))
    }
}

/// Devcontainer config from devcontainer.json.
#[serde_as]
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct DevcontainerConfig {
    // -------------------------------------------------------------------------
    // Compose section
    /// The name of the docker-compose file(s) used to start the services.
    #[serde_as(as = "OneOrMany<_>")]
    pub(crate) docker_compose_file: Vec<Template>,
    /// The service you want to work on. This is considered the primary container for your dev
    /// environment which your editor will connect to.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub(crate) service: String,
    /// An array of services that should be started and stopped.
    #[serde(default)]
    pub(crate) run_services: Option<Vec<String>>,
    /// The path of the workspace folder inside the container. This is typically the target path of
    /// a volume mount in the docker-compose.yml.
    #[serde(skip_serializing_if = "Template::is_empty")]
    pub(crate) workspace_folder: Template,
    /// Action to take when the user disconnects from the primary container in their editor. The
    /// default is to stop all of the compose containers.
    #[serde(default)]
    pub(crate) shutdown_action: ComposeShutdownAction,
    /// Whether to overwrite the command specified in the image. The default is false.
    #[serde(default)]
    pub(crate) override_command: bool,
    // -------------------------------------------------------------------------
    // Common section
    /// The JSON schema of the devcontainer.json file.
    #[serde(rename = "$schema")]
    pub(crate) schema: Option<String>,
    /// A name for the dev container which can be displayed to the user.
    pub(crate) name: Option<Template>,
    /// Features to add to the dev container. Ignored by devconcurrent.
    #[serde(deserialize_with = "unsupported::features::warn")]
    pub(crate) features: serde_json::Map<String, serde_json::Value>,
    /// Array consisting of the Feature id (without the semantic version) of Features in the order
    /// the user wants them to be installed. Ignored by devconcurrent.
    #[serde(deserialize_with = "unsupported::overrideFeatureInstallOrder::warn")]
    pub(crate) override_feature_install_order: Vec<String>,
    /// Recommended secrets for this dev container. Ignored by devconcurrent.
    pub(crate) secrets: serde_json::Map<String, serde_json::Value>,
    /// Ports that can be forwarded from the host to the container.
    pub(crate) forward_ports: Vec<ForwardPort>,
    /// Settings to apply to forwardPorts. Ignored by devconcurrent.
    pub(crate) ports_attributes: IndexMap<String, PortAttributes>,
    /// Default settings for forwardPorts. Ignored by devconcurrent.
    pub(crate) other_ports_attributes: Option<PortAttributes>,
    /// Controls whether on Linux the container's user should be updated with the local user's UID
    /// and GID. On by default when opening from a local folder.
    #[serde(rename = "updateRemoteUserUID")]
    pub(crate) update_remote_user_uid: Option<bool>,
    /// Container environment variables.
    pub(crate) container_env: IndexMap<String, Template>,
    /// The user the container will be started with. The default is the user on the Docker image.
    pub(crate) container_user: Option<Template>,
    /// Mounts to setup on container create.
    pub(crate) mounts: Vec<MountEntry>,
    /// Passes the --init flag when creating the dev container.
    pub(crate) init: Option<bool>,
    /// Passes the --privileged flag when creating the dev container.
    pub(crate) privileged: Option<bool>,
    /// Passes docker capabilities to include when creating the dev container.
    pub(crate) cap_add: Vec<String>,
    /// Passes docker security options to include when creating the dev container.
    pub(crate) security_opt: Vec<String>,
    /// Remote environment variables to set for processes spawned in the
    /// container including lifecycle scripts and any remote editor/IDE server
    /// process.
    pub(crate) remote_env: IndexMap<String, Option<Template>>,
    /// The username to use for spawning processes in the container including
    /// lifecycle scripts and any remote editor/IDE server process. The default
    /// is the same user as the container.
    pub(crate) remote_user: Option<Template>,

    /// A command to run locally (i.e Your host machine, cloud VM) before anything else. This
    /// command is run before "onCreateCommand".
    pub(crate) initialize_command: Option<LifecycleCommandTemplate>,
    /// A command to run when creating the container. This command is run after "initializeCommand"
    /// and before "updateContentCommand".
    pub(crate) on_create_command: Option<LifecycleCommandTemplate>,
    /// A command to run when creating the container and rerun when the workspace content was
    /// updated while creating the container. This command is run after "onCreateCommand" and before
    /// "postCreateCommand".
    pub(crate) update_content_command: Option<LifecycleCommandTemplate>,
    /// A command to run after creating the container. This command is run after
    /// "updateContentCommand" and before "postStartCommand".
    pub(crate) post_create_command: Option<LifecycleCommandTemplate>,
    /// A command to run after starting the container. This command is run after "postCreateCommand"
    /// and before "postAttachCommand".
    pub(crate) post_start_command: Option<LifecycleCommandTemplate>,
    /// A command to run when attaching to the container. This command is run after
    /// "postStartCommand".
    pub(crate) post_attach_command: Option<LifecycleCommandTemplate>,
    /// The user command to wait for before continuing execution in the background while the UI is
    /// starting up.
    pub(crate) wait_for: WaitFor,
    /// User environment probe to run.
    pub(crate) user_env_probe: UserEnvProbe,

    /// Host hardware requirements.
    pub(crate) host_requirements: Option<HostRequirements>,
    /// Tool-specific configuration. Each tool should use a JSON object subproperty with a unique
    /// name to group its customizations.
    pub(crate) customizations: Customizations,
}

impl DevcontainerConfig {
    /// Whether to remap the remote user's uid/gid to the host user's. Per spec,
    /// on by default when opening from a local folder — which is the only thing
    /// devconcurrent does.
    pub(crate) fn update_remote_user_uid(&self) -> bool {
        self.update_remote_user_uid.unwrap_or(true)
    }

    /// Find the appropriate devcontainer.json file from the given root directory.
    ///
    /// Return None if there is no devcontainer.json file, and treat the project as one that
    /// does not use devcontainers.
    ///
    /// From the devcontainer reference:
    /// <https://containers.dev/implementors/spec/#devcontainerjson>
    ///
    /// Products using it should expect to find a devcontainer.json file in one or more of the
    /// following locations (in order of precedence):
    ///
    /// * .devcontainer/devcontainer.json
    /// * .devcontainer.json
    /// * .devcontainer/<folder>/devcontainer.json (where <folder> is a sub-folder, one level deep)
    ///
    /// It is valid that these files may exist in more than one location, so consider providing a
    /// mechanism for users to select one when appropriate.
    pub(crate) fn find_config(dir: &Path) -> Option<PathBuf> {
        let candidates = [
            dir.join(".devcontainer/devcontainer.json"),
            dir.join(".devcontainer.json"),
        ];

        candidates.into_iter().find(|p| p.is_file()).or_else(|| {
            // .devcontainer/<folder>/devcontainer.json
            let devcontainer_dir = dir.join(".devcontainer");
            std::fs::read_dir(&devcontainer_dir)
                .ok()
                .and_then(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .find(|e| {
                            e.file_type().is_ok_and(|ft| ft.is_dir())
                                && e.path().join("devcontainer.json").is_file()
                        })
                        .map(|e| e.path().join("devcontainer.json"))
                })
        })
    }

    /// Load the merged devcontainer config from the given path (if any) and the project's
    /// overrides. Returns `Ok(None)` if neither source provides any config.
    pub(crate) fn load(path: Option<&Path>, project: &Project) -> eyre::Result<Option<Self>> {
        if path.is_none() && project.devcontainer.is_none() {
            return Ok(None);
        }

        let mut figment = Figment::new();

        if let Some(path) = path {
            figment = figment.admerge(Json::file(path));
        }

        if let Some(overrides) = &project.devcontainer {
            figment = figment.admerge(Serialized::defaults(overrides));
        }

        let config: Self = figment
            .extract()
            .wrap_err("failed to merge devcontainer config")?;
        config.check_proxy_container_ports()?;
        Ok(Some(config))
    }

    /// The proxy binds 80 and 443 in the service's own network namespace, so a
    /// service listening on either has already taken a port the proxy needs.
    fn check_proxy_container_ports(&self) -> eyre::Result<()> {
        for (svc_name, svc) in &self.customizations.devconcurrent.proxy.services {
            let Some(port) = svc.container_port else {
                continue;
            };
            if port == shared::HTTP_PORT || port == shared::HTTPS_PORT {
                eyre::bail!(
                    "service {svc_name:?}: `containerPort` cannot be {port}, because that is a \
                     port the proxy serves the service on. Have the service listen on another \
                     port and speak plain http; the proxy handles TLS.",
                );
            }
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
pub(crate) struct Customizations {
    #[serde(default)]
    pub(crate) devconcurrent: DcOptions,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(untagged)]
pub(crate) enum MountEntry {
    /// Docker `--mount` short form: `type=bind,source=...,target=...`.
    String(Template),
    Object(Mount),
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Mount {
    #[serde(rename = "type")]
    pub(crate) ty: MountType,
    #[serde(default)]
    pub(crate) source: Option<Template>,
    pub(crate) target: Template,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) enum MountType {
    /// A bind mount from the host filesystem.
    Bind,
    /// A named Docker volume.
    Volume,
}

impl MountEntry {
    /// Render to a compose-compatible volume entry. For bind mounts and named volumes alike,
    /// `"source:target"` short form suffices: compose treats a leading `/` as a host path (bind)
    /// and other leading characters as a named volume.
    pub(crate) fn to_compose_volume(
        &self,
        context: &substitution::Context<'_>,
    ) -> eyre::Result<String> {
        match self {
            MountEntry::String(template) => {
                Ok(Mount::parse(&context.render_field("mounts", template)?)?.render())
            }
            MountEntry::Object(mount) => mount.render_with(context),
        }
    }
}

impl Mount {
    /// Parse a docker `--mount` short form (`key=value,key=value,...`).
    fn parse(text: &str) -> eyre::Result<MountFields> {
        let mut ty = None;
        let mut source = None;
        let mut target = None;
        for pair in text.split(',') {
            let (key, value) = pair
                .split_once('=')
                .ok_or_else(|| eyre::eyre!("mount entry missing `=`: {pair}"))?;
            match key.trim() {
                "type" => {
                    ty = Some(match value {
                        "bind" => MountType::Bind,
                        "volume" => MountType::Volume,
                        other => eyre::bail!("unsupported mount type: {other}"),
                    });
                }
                "source" | "src" => source = Some(value.to_string()),
                "target" | "dst" | "destination" => target = Some(value.to_string()),
                _ => {} // ignore `readonly`, `consistency`, etc. — extending later if needed.
            }
        }
        Ok(MountFields {
            ty: ty.ok_or_else(|| eyre::eyre!("mount entry missing `type`: {text}"))?,
            source,
            target: target.ok_or_else(|| eyre::eyre!("mount entry missing `target`: {text}"))?,
        })
    }

    fn render_with(&self, context: &substitution::Context<'_>) -> eyre::Result<String> {
        let source = self
            .source
            .as_ref()
            .map(|t| context.render_field("mounts.source", t))
            .transpose()?;
        Ok(MountFields {
            ty: self.ty,
            source,
            target: context.render_field("mounts.target", &self.target)?,
        }
        .render())
    }
}

/// Post-rendering / post-parsing intermediate: all fields are plain strings, ready to format
/// into a compose volume entry.
struct MountFields {
    ty: MountType,
    source: Option<String>,
    target: String,
}

impl MountFields {
    fn render(self) -> String {
        match (self.ty, self.source) {
            (_, Some(source)) => format!("{source}:{}", self.target),
            // Anonymous volume: compose accepts just the target.
            (MountType::Volume, None) => self.target,
            (MountType::Bind, None) => self.target, // unusual but pass through.
        }
    }
}

#[serde_inline_default]
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct HostRequirements {
    /// Number of required CPUs.
    #[serde_inline_default(1)]
    #[schemars(range(min = 1))]
    pub(crate) cpus: u64,
    /// Amount of required RAM in bytes. Supports units tb, gb, mb and kb.
    #[schemars(regex(pattern = r"^\d+([tgmk]b)?$"))]
    pub(crate) memory: Option<String>,
    /// Amount of required disk space in bytes. Supports units tb, gb, mb and kb.
    #[schemars(regex(pattern = r"^\d+([tgmk]b)?$"))]
    pub(crate) storage: Option<String>,
    pub(crate) gpu: GpuRequirement,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(untagged)]
pub(crate) enum GpuRequirement {
    Bool(bool),
    String(GpuOptional),
    Object {
        /// Number of required cores.
        #[schemars(range(min = 1))]
        cores: Option<u64>,
        /// Amount of required RAM in bytes. Supports units tb, gb, mb and kb.
        #[schemars(regex(pattern = r"^\d+([tgmk]b)?$"))]
        memory: Option<String>,
    },
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) enum GpuOptional {
    Optional,
}

impl Default for GpuRequirement {
    fn default() -> Self {
        Self::Bool(false)
    }
}

#[serde_inline_default]
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PortAttributes {
    #[serde(default)]
    pub(crate) on_auto_forward: OnAutoForward,
    #[serde(default)]
    pub(crate) elevate_if_needed: bool,
    #[serde_inline_default(String::from("Application"))]
    pub(crate) label: String,
    #[serde(default)]
    pub(crate) protocol: Protocol,
    #[serde(default)]
    pub(crate) require_local_port: bool,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Protocol {
    #[default]
    Http,
    Https,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) enum OnAutoForward {
    #[default]
    Notify,
    OpenBrowser,
    OpenBrowserOnce,
    OpenPreview,
    Silent,
    Ignore,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) enum UserEnvProbe {
    /// Do not probe the user's shell for environment variables.
    None,
    /// Probe with a login shell (`-lc`).
    LoginShell,
    /// Probe with a login, interactive shell (`-lic`).
    #[default]
    LoginInteractiveShell,
    /// Probe with an interactive shell (`-ic`).
    InteractiveShell,
}

#[allow(clippy::enum_variant_names)]
#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) enum WaitFor {
    InitializeCommand,
    OnCreateCommand,
    #[default]
    UpdateContentCommand,
    PostCreateCommand,
    PostStartCommand,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ComposeShutdownAction {
    None,
    #[default]
    StopCompose,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::LazyLock;

    static LABELS: LazyLock<DevcontainerLabels> =
        LazyLock::new(|| DevcontainerLabels::new(PathBuf::from("/local"), None));

    fn ctx() -> substitution::Context<'static> {
        substitution::Context::new(Path::new("/local"), &LABELS)
            .with_container_workspace_folder(Path::new("/container"))
    }

    #[test]
    fn mount_string_bind() {
        let entry: MountEntry =
            serde_json::from_str(r#""type=bind,source=/host/path,target=/in/container""#).unwrap();
        assert_eq!(
            entry.to_compose_volume(&ctx()).unwrap(),
            "/host/path:/in/container",
        );
    }

    #[test]
    fn mount_string_named_volume() {
        let entry: MountEntry =
            serde_json::from_str(r#""type=volume,source=myvol,target=/data""#).unwrap();
        assert_eq!(entry.to_compose_volume(&ctx()).unwrap(), "myvol:/data");
    }

    #[test]
    fn mount_string_anonymous_volume() {
        let entry: MountEntry = serde_json::from_str(r#""type=volume,target=/data""#).unwrap();
        assert_eq!(entry.to_compose_volume(&ctx()).unwrap(), "/data");
    }

    #[test]
    fn mount_string_substitutes_local_workspace_folder() {
        let entry: MountEntry =
            serde_json::from_str(r#""type=bind,source=${localWorkspaceFolder}/.aws,target=/aws""#)
                .unwrap();
        assert_eq!(entry.to_compose_volume(&ctx()).unwrap(), "/local/.aws:/aws");
    }

    #[test]
    fn mount_string_accepts_src_and_dst_aliases() {
        let entry: MountEntry = serde_json::from_str(r#""type=bind,src=/host,dst=/in""#).unwrap();
        assert_eq!(entry.to_compose_volume(&ctx()).unwrap(), "/host:/in");
    }

    #[test]
    fn mount_object_form() {
        let entry: MountEntry =
            serde_json::from_str(r#"{"type":"bind","source":"/host","target":"/in"}"#).unwrap();
        assert_eq!(entry.to_compose_volume(&ctx()).unwrap(), "/host:/in");
    }

    #[test]
    fn mount_object_form_with_substitution() {
        let entry: MountEntry = serde_json::from_str(
            r#"{"type":"bind","source":"${localWorkspaceFolder}/data","target":"/data"}"#,
        )
        .unwrap();
        assert_eq!(
            entry.to_compose_volume(&ctx()).unwrap(),
            "/local/data:/data"
        );
    }

    #[test]
    fn mount_string_missing_type_errors() {
        let entry: MountEntry = serde_json::from_str(r#""source=/host,target=/in""#).unwrap();
        assert!(entry.to_compose_volume(&ctx()).is_err());
    }

    #[test]
    fn mount_string_missing_target_errors() {
        let entry: MountEntry = serde_json::from_str(r#""type=bind,source=/host""#).unwrap();
        assert!(entry.to_compose_volume(&ctx()).is_err());
    }

    /// Goes through the real key spelling, so a rename of `containerPort` can't
    /// pass this silently.
    fn config_with_container_port(port: serde_json::Value) -> DevcontainerConfig {
        serde_json::from_value(serde_json::json!({
            "customizations": {
                "devconcurrent": {
                    "proxy": {
                        "enable": true,
                        "services": {"app": {"containerPort": port}},
                    },
                },
            },
        }))
        .expect("valid devcontainer config")
    }

    /// The proxy needs 80 and 443 for itself, and the failure if it doesn't get
    /// them is a bind conflict inside a sidecar nobody is watching — so the
    /// error has to name the service, the port, and the way out.
    #[test]
    fn container_port_80_or_443_is_rejected_by_name() {
        for port in [shared::HTTP_PORT, shared::HTTPS_PORT] {
            let Err(err) = config_with_container_port(port.into()).check_proxy_container_ports()
            else {
                panic!("port {port} should be rejected");
            };
            let err = err.to_string();

            assert!(err.contains("\"app\""), "no service name in: {err}");
            assert!(err.contains(&port.to_string()), "no port in: {err}");
            assert!(err.contains("containerPort"), "no key name in: {err}");
            assert!(
                err.contains("listen on another port"),
                "no remedy in: {err}",
            );
        }
    }

    #[test]
    fn an_ordinary_container_port_is_accepted() {
        assert!(
            config_with_container_port(3000.into())
                .check_proxy_container_ports()
                .is_ok()
        );
    }

    /// A DNS-only service has no port to collide with.
    #[test]
    fn a_service_without_a_container_port_is_accepted() {
        assert!(
            config_with_container_port(serde_json::Value::Null)
                .check_proxy_container_ports()
                .is_ok()
        );
    }
}
