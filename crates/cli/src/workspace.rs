use std::path::PathBuf;

use docker::{ContainerStatus, FORWARD_LABEL, PROJECT_LABEL, WORKSPACE_LABEL};
use eyre::eyre;

use crate::docker::ContainerInfo;
use crate::state::{DevcontainerState, State};
use crate::worktree;

pub(crate) mod git_status;

pub(crate) struct Workspace<'a> {
    pub(crate) state: &'a State<'a>,
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) is_root: bool,
}

impl<'a> Workspace<'a> {
    pub(crate) async fn list(state: &'a State<'a>) -> eyre::Result<Vec<Workspace<'a>>> {
        let paths = worktree::list(&state.project.path).await?;
        Ok(paths
            .into_iter()
            .filter_map(|path| Self::from_path(path, state))
            .collect())
    }

    pub(crate) fn from_path(path: PathBuf, state: &'a State) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().to_string();
        let is_root = state.is_root(&name);

        Some(Self {
            state,
            name,
            path,
            is_root,
        })
    }

    pub(crate) async fn is_dirty(&self) -> eyre::Result<bool> {
        Ok(git_status::GitStatus::fetch(&self.path).await?.is_dirty())
    }

    pub(crate) fn project_label(&self) -> (&str, &str) {
        (PROJECT_LABEL, &self.state.project_name)
    }

    pub(crate) fn workspace_label(&self) -> (&str, &str) {
        (WORKSPACE_LABEL, &self.name)
    }

    pub(crate) fn fwd_label(&self) -> (&str, &str) {
        (FORWARD_LABEL, "true")
    }

    pub(crate) fn docker_fwd_labels(&self) -> [(&str, &str); 3] {
        [
            self.project_label(),
            self.workspace_label(),
            self.fwd_label(),
        ]
    }

    pub(crate) async fn devcontainer(
        &self,
        devcontainer: &DevcontainerState,
    ) -> eyre::Result<WorkspaceDevcontainer> {
        let containers = devcontainer
            .docker()
            .await?
            .workspace_container_info(self)
            .await?;
        Ok(WorkspaceDevcontainer { containers })
    }
}

pub(crate) struct WorkspaceDevcontainer {
    containers: Vec<ContainerInfo>,
}

impl WorkspaceDevcontainer {
    /// Highest "liveness" state across the workspace's containers, or `None`
    /// if there are no containers at all.
    pub(crate) fn status(&self) -> Option<ContainerStatus> {
        self.containers.iter().map(|c| c.state).max()
    }

    pub(crate) fn service_container_id(&self) -> eyre::Result<&str> {
        // FIXME: We need to find the correct service container.
        Ok(&self
            .containers
            .first()
            .ok_or_else(|| eyre!("no containers for workspace"))?
            .id)
    }
}
