use std::path::PathBuf;

use indexmap::IndexMap;
use serde::Deserialize;
use serde_inline_default::serde_inline_default;

use crate::runner::cmd::Cmd;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub default_cmd: Cmd,

    #[serde(default)]
    pub projects: IndexMap<String, Project>,
}

#[serde_inline_default]
#[derive(Debug, Deserialize)]
pub struct Project {
    #[serde(default)]
    pub default_cmd: Cmd,
    pub path: PathBuf,
    /// Directory to create workspaces in (default /tmp/).
    #[serde_inline_default("/tmp/".into())]
    pub workspace_dir: PathBuf,

    /// If set, this port will be used automatically by the `dc fwd` command, to
    /// map a static host port to the container of your choice.
    pub fwd_port: Option<u16>,

    /// If supplied, this command will be run when updating the container to
    /// port forward to.
    #[serde(default)]
    pub fwd_cmd: Cmd,
}

#[derive(Debug, Deserialize)]
pub struct ProjectOverride {
    #[serde(default)]
    pub default_cmd: Cmd,
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub workspace_dir: Option<PathBuf>,
}
