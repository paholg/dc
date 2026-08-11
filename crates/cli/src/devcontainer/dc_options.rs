use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use shared::ProxyOptions;

use crate::helpers::deserialize_shell_path_opt;
use crate::run::cmd::CmdTemplate;

#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct DcOptions {
    pub(crate) default_exec: Option<CmdTemplate>,
    #[serde(deserialize_with = "deserialize_shell_path_opt")]
    pub(crate) worktree_folder: Option<PathBuf>,
    /// Whether to mount the project's git directory into each workspace's devcontainer.
    ///
    /// Git worktrees have a simple `.git` file that points to the actual `.git` directory. If that
    /// directory isn't available, then no git commands will work in the worktree. By mounting it
    /// at its original path in the devcontainer, we allow you to use `git` freely for the workspace,
    /// both inside and out of the devcontainer.
    ///
    /// Defaults to true, but we use Option so it can be overridden.
    mount_git: Option<bool>,
    /// Reverse-proxy configuration.
    ///
    /// Leave empty if you don't wish to use it.
    pub(crate) proxy: ProxyOptions,
}

impl DcOptions {
    pub(crate) fn mount_git(&self) -> bool {
        self.mount_git.unwrap_or(true)
    }
}
