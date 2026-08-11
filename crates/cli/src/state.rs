use std::{
    env,
    path::{Path, PathBuf},
    sync::Arc,
};

use eyre::OptionExt;

use crate::{
    config::{Config, Project, ProjectName},
    devcontainer::{DevcontainerConfig, DevcontainerLabels, dc_options::DcOptions, substitution},
    docker::DockerClient,
    workspace::Workspace,
    worktree,
};

pub(crate) struct State<'a> {
    pub(crate) project_name: ProjectName,
    pub(crate) project: &'a Project,
    pub(crate) devcontainer: Option<DevcontainerState>,
    working_dir: PathBuf,
}

pub(crate) struct DevcontainerState {
    pub(crate) config: DevcontainerConfig,
    pub(crate) docker: Arc<DockerClient>,
    pub(crate) labels: DevcontainerLabels,
    /// `workspaceFolder` with its variables resolved. It is what
    /// `${containerWorkspaceFolder}` expands to everywhere else, so it is
    /// rendered here, once, before any other field.
    pub(crate) workspace_folder: PathBuf,
}

impl DevcontainerState {
    async fn new(project: &Project) -> eyre::Result<Option<Self>> {
        let path = DevcontainerConfig::find_config(&project.path);
        let Some(config) = DevcontainerConfig::load(path.as_deref(), project)? else {
            return Ok(None);
        };
        let docker = DockerClient::new().await?;

        Self::assemble(path, config, Arc::new(docker), project.path.clone()).map(Some)
    }

    fn assemble(
        path: Option<PathBuf>,
        config: DevcontainerConfig,
        docker: Arc<DockerClient>,
        local_folder: PathBuf,
    ) -> eyre::Result<Self> {
        let labels = DevcontainerLabels::new(local_folder, path);
        let workspace_folder = resolve_workspace_folder(&config, &labels)?;

        Ok(Self {
            config,
            docker,
            labels,
            workspace_folder,
        })
    }

    /// A rendering context with everything but `${containerEnv:…}`, which needs
    /// a container to read from.
    pub(crate) fn context<'a>(&'a self, local_folder: &'a Path) -> substitution::Context<'a> {
        substitution::Context::new(local_folder, &self.labels)
            .with_container_workspace_folder(&self.workspace_folder)
    }

    pub(crate) fn devconcurrent(&self) -> &DcOptions {
        &self.config.customizations.devconcurrent
    }

    pub(crate) fn proxy_enabled(&self) -> bool {
        self.devconcurrent().proxy.enable
    }
}

/// Render `workspaceFolder` in the local phase: it is what defines the
/// container workspace folder, and no container exists yet.
fn resolve_workspace_folder(
    config: &DevcontainerConfig,
    labels: &DevcontainerLabels,
) -> eyre::Result<PathBuf> {
    substitution::Context::new(labels.local_folder(), labels)
        .render_path("workspaceFolder", &config.workspace_folder)
}

impl<'a> State<'a> {
    pub(crate) async fn new(
        specified_project: Option<String>,
        config: &'a Config,
    ) -> eyre::Result<Self> {
        let (project_name, project) = config.project(specified_project)?;

        let devcontainer = DevcontainerState::new(project).await?;

        let working_dir = Self::resolve_working_dir(&project_name, project, devcontainer.as_ref())?;

        Ok(Self {
            project_name,
            project,
            devcontainer,
            working_dir,
        })
    }

    pub(crate) fn is_root(&self, name: &str) -> bool {
        self.project
            .path
            .file_name()
            .is_some_and(|root| name == root)
    }

    /// The directory we use to create git worktrees and docker override files.
    pub(crate) fn project_working_dir(&self) -> &Path {
        &self.working_dir
    }

    /// Resolve the working directory, in priority:
    ///
    /// * Read from devconcurrent config file for the project
    /// * Read from customizations.devconcurrent in devcontainer.json
    /// * Defaults to the XDG data dir, e.g. `~/.local/share/devconcurrent/<PROJECT_NAME>/`
    fn resolve_working_dir(
        project_name: &str,
        project: &Project,
        devcontainer: Option<&DevcontainerState>,
    ) -> eyre::Result<PathBuf> {
        let dir = match project.worktree_folder.clone().or_else(|| {
            devcontainer.and_then(|dc| {
                dc.config
                    .customizations
                    .devconcurrent
                    .worktree_folder
                    .clone()
            })
        }) {
            Some(dir) => dir,
            None => directories::ProjectDirs::from("", "", "devconcurrent")
                .ok_or_eyre("could not determine data directory")?
                .data_dir()
                .join(project_name),
        };

        Ok(if dir.is_relative() {
            project.path.join(dir)
        } else {
            dir
        })
    }

    pub(crate) fn ensure_project_working_dir(&self) -> eyre::Result<()> {
        std::fs::create_dir_all(self.project_working_dir())?;
        Ok(())
    }

    fn worktree_path(&self, workspace_name: &str) -> PathBuf {
        self.project_working_dir().join(workspace_name)
    }

    /// Find the workspace, erroring if no name is given and the current
    /// working directory isn't inside a worktree.
    pub(crate) async fn resolve_workspace(
        &self,
        name: Option<String>,
    ) -> eyre::Result<Workspace<'_>> {
        self.try_resolve_workspace(name).await?.ok_or_else(|| {
            eyre::eyre!(
                "no workspace specified and not inside a worktree of project '{}'",
                self.project_name
            )
        })
    }

    /// Find the workspace. A given name (other than ".") always resolves;
    /// otherwise we derive it from the current working directory, returning
    /// `None` when the cwd isn't inside a worktree.
    pub(crate) async fn try_resolve_workspace(
        &self,
        name: Option<String>,
    ) -> eyre::Result<Option<Workspace<'_>>> {
        let worktrees = worktree::list(&self.project.path).await?;

        if let Some(workspace_name) = name
            && workspace_name != "."
        {
            let path = worktrees
                .into_iter()
                .find(|wt| wt.file_name() == Some(workspace_name.as_ref()))
                .unwrap_or_else(|| self.worktree_path(&workspace_name));
            let is_root = self.is_root(&workspace_name);
            return Ok(Some(Workspace {
                state: self,
                name: workspace_name,
                path,
                is_root,
            }));
        }

        let cwd = env::current_dir()?;

        let Some(path) = worktrees
            .into_iter()
            .filter(|wt| cwd.starts_with(wt))
            .max_by_key(|wt| wt.as_os_str().len())
        else {
            return Ok(None);
        };

        let name = path
            .file_name()
            .ok_or_eyre("worktree path has no basename")?
            .to_string_lossy()
            .to_string();

        let is_root = self.is_root(&name);

        Ok(Some(Workspace {
            state: self,
            name,
            path,
            is_root,
        }))
    }

    pub(crate) fn try_devcontainer(&self) -> eyre::Result<&DevcontainerState> {
        self.devcontainer.as_ref().ok_or_else(|| eyre::eyre!("no devcontainer.json found for this project; devcontainer functionality is disabled"))
    }

    pub(crate) fn has_devcontainer(&self) -> bool {
        self.devcontainer.is_some()
    }

    /// Load the devcontainer config for a specific workspace directory.
    pub(crate) fn devcontainer_for(
        &self,
        workspace_path: &Path,
    ) -> eyre::Result<DevcontainerState> {
        let root = self.try_devcontainer()?;
        let path = DevcontainerConfig::find_config(workspace_path);
        let config = DevcontainerConfig::load(path.as_deref(), self.project)?.ok_or_else(|| {
            eyre::eyre!(
                "no devcontainer.json found in workspace {}",
                workspace_path.display()
            )
        })?;

        DevcontainerState::assemble(
            path,
            config,
            root.docker.clone(),
            workspace_path.to_path_buf(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devcontainer::DevcontainerConfig;

    fn config(workspace_folder: &str) -> DevcontainerConfig {
        serde_json::from_value(serde_json::json!({
            "dockerComposeFile": "docker-compose.yml",
            "service": "app",
            "workspaceFolder": workspace_folder,
        }))
        .expect("valid devcontainer config")
    }

    fn labels() -> DevcontainerLabels {
        DevcontainerLabels::new(PathBuf::from("/host/worktrees/feature3"), None)
    }

    /// The whole point for worktrees: one config, a folder per workspace.
    #[test]
    fn workspace_folder_takes_the_worktree_name() {
        let folder = resolve_workspace_folder(
            &config("/workspaces/${localWorkspaceFolderBasename}"),
            &labels(),
        )
        .expect("variables are available");
        assert_eq!(folder, PathBuf::from("/workspaces/feature3"));
    }

    #[test]
    fn workspace_folder_cannot_refer_to_itself() {
        let err = resolve_workspace_folder(&config("${containerWorkspaceFolder}/sub"), &labels())
            .expect_err("the field defines the variable");
        let err = format!("{err:#}");
        assert!(err.contains("workspaceFolder"), "{err}");
        assert!(err.contains("cannot refer to itself"), "{err}");
    }

    #[test]
    fn workspace_folder_cannot_read_container_env() {
        let err = resolve_workspace_folder(&config("${containerEnv:HOME}"), &labels())
            .expect_err("no container exists yet");
        let err = format!("{err:#}");
        assert!(err.contains("workspaceFolder"), "{err}");
        assert!(err.contains("no container exists"), "{err}");
    }
}
