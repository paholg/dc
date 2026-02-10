use std::{fs::File, path::PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_inline_default::serde_inline_default;
use serde_with::{OneOrMany, serde_as};

pub mod lifecycle_command;
mod unsupported;

use crate::{config::Project, run::cmd::Cmd};
use lifecycle_command::LifecycleCommand;
use unsupported::Unsupported;

fn deserialize_shell_path_opt<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<PathBuf>, D::Error> {
    Option::<String>::deserialize(d)
        .map(|o| o.map(|s| PathBuf::from(shellexpand::tilde(&s).as_ref())))
}

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct DcOptions {
    pub default_exec: Option<Cmd>,
    #[serde(default, deserialize_with = "deserialize_shell_path_opt")]
    worktree_folder: Option<PathBuf>,
    /// If set, this port will be used automatically by the `dc fwd` command, to
    /// map a static host port to the container of your choice.
    pub forward_port: Option<u16>,
    /// Port inside the container to forward to. Defaults to `fwd_port` if unset.
    pub container_port: Option<u16>,
    pub default_copy_volumes: Option<Vec<String>>,
}

impl DcOptions {
    pub fn workspace_dir(&self) -> PathBuf {
        self.worktree_folder.clone().unwrap_or("/tmp/".into())
    }
}

/// Devcontainer config from devcontainer.json.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct DevContainer {
    #[serde(flatten)]
    pub common: Common,
    #[serde(flatten)]
    pub kind: Kind,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum Kind {
    Compose(Compose),
    #[serde(deserialize_with = "unsupported::Image::error")]
    Image(Image),
    #[serde(deserialize_with = "unsupported::Dockerfile::error")]
    Dockerfile(Box<Dockerfile>),
}

impl DevContainer {
    /// Load the appropriate devcontainer.json file from the given root directory.
    ///
    /// The given `dir` should be the directory containing `.devcontainer/`.
    ///
    /// From the devcontainer reference:
    ///
    /// Products using it should expect to find a devcontainer.json file in one or more of the following locations (in order of precedence):
    /// .devcontainer/devcontainer.json
    /// .devcontainer.json
    /// .devcontainer/<folder>/devcontainer.json (where <folder> is a sub-folder, one level deep)
    pub fn load(project: &Project) -> eyre::Result<Self> {
        let dir = &project.path;
        let candidates = [
            dir.join(".devcontainer/devcontainer.json"),
            dir.join(".devcontainer.json"),
        ];

        let path = candidates
            .into_iter()
            .find(|p| p.is_file())
            .or_else(|| {
                // .devcontainer/<folder>/devcontainer.json (one level deep)
                let dc_dir = dir.join(".devcontainer");
                std::fs::read_dir(&dc_dir).ok().and_then(|entries| {
                    entries
                        .filter_map(Result::ok)
                        .find(|e| {
                            e.file_type().is_ok_and(|ft| ft.is_dir())
                                && e.path().join("devcontainer.json").is_file()
                        })
                        .map(|e| e.path().join("devcontainer.json"))
                })
            })
            .ok_or_else(|| eyre::eyre!("no devcontainer.json found in {}", dir.display()))?;

        let contents = File::open(&path)?;
        let devcontainer: DevContainer = serde_json::from_reader(&contents)?;
        Ok(devcontainer)
    }
}

#[serde_as]
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Compose {
    /// The name of the docker-compose file(s) used to start the services.
    #[serde_as(as = "OneOrMany<_>")]
    pub docker_compose_file: Vec<String>,
    /// The service you want to work on. This is considered the primary container for your dev
    /// environment which your editor will connect to.
    pub service: String,
    /// An array of services that should be started and stopped.
    #[serde(default)]
    pub run_services: Option<Vec<String>>,
    /// The path of the workspace folder inside the container. This is typically the target path of
    /// a volume mount in the docker-compose.yml.
    pub workspace_folder: PathBuf,
    /// Action to take when the user disconnects from the primary container in their editor. The
    /// default is to stop all of the compose containers.
    #[serde(default)]
    pub shutdown_action: ComposeShutdownAction,
    /// Whether to overwrite the command specified in the image. The default is false.
    #[serde(default)]
    pub override_command: bool,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Image {
    pub image: String,

    #[serde(flatten)]
    pub non_compose: NonComposeProperties,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Dockerfile {
    /// The location of the Dockerfile that defines the contents of the container. The path is
    /// relative to the folder containing the `devcontainer.json` file
    pub docker_file: Option<PathBuf>,
    /// The location of the context folder for building the Docker image. The path is relative to
    /// the folder containing the `devcontainer.json` file."
    pub context: Option<PathBuf>,
    pub build: Option<BuildOptions>,

    #[serde(flatten)]
    pub non_compose: NonComposeProperties,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Common {
    /// The JSON schema of the devcontainer.json file.
    #[serde(rename = "$schema")]
    pub schema: Option<String>,
    /// A name for the dev container which can be displayed to the user.
    pub name: Option<String>,
    /// Features to add to the dev container.
    #[serde(deserialize_with = "unsupported::features::error")]
    pub features: serde_json::Value,
    /// Array consisting of the Feature id (without the semantic version) of Features in the order
    /// the user wants them to be installed.
    #[serde(deserialize_with = "unsupported::overrideFeatureInstallOrder::error")]
    pub override_feature_install_order: Vec<String>,
    #[serde(deserialize_with = "unsupported::secrets::warn")]
    pub secrets: serde_json::Value,
    #[serde(deserialize_with = "unsupported::forwardPorts::warn")]
    pub forward_ports: Vec<Port>,
    #[serde(deserialize_with = "unsupported::portsAttributes::warn")]
    pub ports_attributes: IndexMap<String, PortAttributes>,
    /// Set default properties that are applied to all ports that don't get properties from the
    /// setting `remote.portsAttributes`
    #[serde(deserialize_with = "unsupported::otherPortsAttributes::warn")]
    pub other_ports_attributes: Option<PortAttributes>,
    /// Controls whether on Linux the container's user should be updated with the local user's UID
    /// and GID. On by default when opening from a local folder.
    pub update_remote_user_uid: Option<bool>,
    /// Container environment variables.
    pub container_env: IndexMap<String, String>,
    /// The user the container will be started with. The default is the user on the Docker image.
    pub container_user: Option<String>,
    #[serde(deserialize_with = "unsupported::mounts::warn")]
    pub mounts: Vec<MountEntry>,
    /// Passes the --init flag when creating the dev container.
    pub init: Option<bool>,
    /// Passes the --privileged flag when creating the dev container.
    pub privileged: Option<bool>,
    /// Passes docker capabilities to include when creating the dev container.
    pub cap_add: Vec<String>,
    /// Passes docker security options to include when creating the dev container.
    pub security_opt: Vec<String>,
    /// Remote environment variables to set for processes spawned in the
    /// container including lifecycle scripts and any remote editor/IDE server
    /// process.
    pub remote_env: IndexMap<String, Option<String>>,
    /// The username to use for spawning processes in the container including
    /// lifecycle scripts and any remote editor/IDE server process. The default
    /// is the same user as the container.
    pub remote_user: Option<String>,

    /// A command to run locally (i.e Your host machine, cloud VM) before anything else. This
    /// command is run before "onCreateCommand".
    pub initialize_command: Option<LifecycleCommand>,
    /// A command to run when creating the container. This command is run after "initializeCommand"
    /// and before "updateContentCommand".
    pub on_create_command: Option<LifecycleCommand>,
    /// A command to run when creating the container and rerun when the workspace content was
    /// updated while creating the container. This command is run after "onCreateCommand" and before
    /// "postCreateCommand".
    pub update_content_command: Option<LifecycleCommand>,
    /// A command to run after creating the container. This command is run after
    /// "updateContentCommand" and before "postStartCommand".
    pub post_create_command: Option<LifecycleCommand>,
    /// A command to run after starting the container. This command is run after "postCreateCommand"
    /// and before "postAttachCommand".
    pub post_start_command: Option<LifecycleCommand>,
    /// A command to run when attaching to the container. This command is run after
    /// "postStartCommand".
    pub post_attach_command: Option<LifecycleCommand>,
    /// The user command to wait for before continuing execution in the background while the UI is
    /// starting up.
    pub wait_for: WaitFor,
    /// User environment probe to run.
    pub user_env_probe: UserEnvProbe,

    /// Host hardware requirements.
    pub host_requirements: Option<HostRequirements>,
    /// Tool-specific configuration. Each tool should use a JSON object subproperty with a unique
    /// name to group its customizations.
    pub customizations: Customizations,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default)]
pub struct Customizations {
    #[serde(default)]
    pub dc: DcOptions,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum Port {
    Number(u16),
    String(String),
}

#[serde_as]
#[serde_inline_default]
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct NonComposeProperties {
    /// Application ports that are exposed by the container. This can be a single port or an array
    /// of ports. Each port can be a number or a string. A number is mapped to the same port on the
    /// host. A string is passed to Docker unchanged and can be used to map ports differently, e.g.
    /// "8000:8010".
    #[serde_as(as = "OneOrMany<_>")]
    pub app_port: Vec<Port>,
    /// The arguments required when starting in the container.
    pub run_args: Vec<String>,
    /// Action to take when the user disconnects from the container in their editor. The default is
    /// to stop the container.
    pub shutdown_action: NonComposeShutdownAction,
    /// Whether to overwrite the command specified in the image. The default is true.
    #[serde_inline_default(true)]
    pub override_command: bool,
    /// The path of the workspace folder inside the container.
    pub workspace_folder: Option<PathBuf>,
    /// The --mount parameter for docker run. The default is to mount the project folder at
    /// /workspaces/$project.
    pub workspace_mount: Option<PathBuf>,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum MountEntry {
    String(String),
    Object(Mount),
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Mount {
    #[serde(rename = "type")]
    pub ty: MountType,
    #[serde(default)]
    pub source: Option<String>,
    pub target: String,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum MountType {
    Bind,
    Volume,
}

#[serde_as]
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct BuildOptions {
    pub target: Option<String>,
    pub args: IndexMap<String, String>,
    #[serde_as(as = "OneOrMany<_>")]
    pub cache_from: Vec<String>,
    pub options: Vec<String>,
}

#[serde_inline_default]
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct HostRequirements {
    /// Number of required CPUs. Minimum 1.
    #[serde_inline_default(1)]
    pub cpus: u64,
    /// Amount of required RAM in bytes. Supports units tb, gb, mb and kb.
    pub memory: Option<String>,
    /// Amount of required RAM in bytes. Supports units tb, gb, mb and kb.
    pub storage: Option<String>,
    pub gpu: GpuRequirement,
}

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum GpuRequirement {
    Bool(bool),
    String(GpuOptional),
    Object {
        /// Number of cores. Minimum 1.
        cores: Option<u64>,
        /// Amount of required RAM in bytes. Supports units tb, gb, mb and kb.
        memory: Option<String>,
    },
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum GpuOptional {
    Optional,
}

impl Default for GpuRequirement {
    fn default() -> Self {
        Self::Bool(false)
    }
}

#[serde_inline_default]
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PortAttributes {
    #[serde(default)]
    pub on_auto_forward: OnAutoForward,
    #[serde(default)]
    pub elevate_if_needed: bool,
    #[serde_inline_default(String::from("Application"))]
    pub label: String,
    pub protocol: Protocol,
    #[serde(default)]
    pub require_local_port: bool,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Protocol {
    Http,
    Https,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum OnAutoForward {
    #[default]
    Notify,
    OpenBrowser,
    OpenBrowserOnce,
    OpenPreview,
    Silent,
    Ignore,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum UserEnvProbe {
    None,
    LoginShell,
    #[default]
    LoginInteractiveShell,
    InteractiveShell,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum WaitFor {
    InitializeCommand,
    OnCreateCommand,
    #[default]
    UpdateContentCommand,
    PostCreateCommand,
    PostStartCommand,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum ComposeShutdownAction {
    None,
    #[default]
    StopCompose,
}

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum NonComposeShutdownAction {
    None,
    #[default]
    StopContainer,
}
