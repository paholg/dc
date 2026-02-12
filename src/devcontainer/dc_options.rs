use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_inline_default::serde_inline_default;
use serde_with::serde_as;

use crate::run::cmd::Cmd;

fn deserialize_shell_path_opt<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<PathBuf>, D::Error> {
    Option::<String>::deserialize(d)
        .map(|o| o.map(|s| PathBuf::from(shellexpand::tilde(&s).as_ref())))
}

#[serde_as]
#[serde_inline_default]
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct DcOptions {
    pub default_exec: Option<Cmd>,
    #[serde(default, deserialize_with = "deserialize_shell_path_opt")]
    worktree_folder: Option<PathBuf>,
    /// The default volumes to be copied with `dc copy` and `dc up --copy`.
    pub default_copy_volumes: Option<Vec<String>>,
    /// Whether to mount the project's git directory into each workspace's devcontainer.
    ///
    /// Git worktrees have a simple `.git` file that points to the actual `.git` directory. If that
    /// directory isn't available, then no git commands will work in the worktree. By mounting it
    /// at its original path in the devcontainer, we allow you to use `git` freely for the workspace,
    /// both inside and out of the devcontainer.
    #[serde_inline_default(true)]
    pub mount_git: bool,
}

impl DcOptions {
    pub fn workspace_dir(&self, project_path: &Path) -> PathBuf {
        let dir = self.worktree_folder.clone().unwrap_or("/tmp/".into());
        if dir.is_relative() {
            project_path.join(dir)
        } else {
            dir
        }
    }
}
