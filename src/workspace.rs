use std::collections::HashMap;
use std::path::PathBuf;

use bollard::models::ContainerSummaryStateEnum;
use eyre::eyre;
use futures::future::try_join_all;
use tokio::process::Command;

use crate::cli::State;
use crate::cli::up::compose_project_name;
use crate::docker::{ContainerInfo, ExecSession, Stats};
use crate::worktree;

pub mod table;

#[derive(Debug)]
pub struct Workspace {
    pub path: PathBuf,
    pub name: String,
    pub root: bool,
    pub compose_project_name: String,
    pub containers: Vec<ContainerInfo>,
    pub dirty: bool,
    pub execs: Vec<ExecSession>,
    pub stats: Stats,
}

impl Workspace {
    /// Get the given workspace or the root workspace if no name is supplied.
    pub async fn get(state: &State, name: Option<&str>) -> eyre::Result<Workspace> {
        let groups = ContainerGroup::list(state).await?;

        let is_root = state.is_root(name);
        let group = if is_root {
            groups
                .into_iter()
                .find(|g| g.path == state.project.path)
                .ok_or_else(|| eyre!("root workspace not found"))?
        } else {
            // We can only get here if name.is_some().
            let name = name.unwrap();
            groups
                .into_iter()
                .find(|g| {
                    if let Some(f) = g.path.file_name()
                        && f == name
                    {
                        true
                    } else {
                        false
                    }
                })
                .ok_or_else(|| eyre!("no workspace found for name {name}"))?
        };
        group.into_workspace(state).await
    }

    pub async fn list(state: &State) -> eyre::Result<Vec<Workspace>> {
        let groups = ContainerGroup::list(state).await?;
        let futures = groups.into_iter().map(|g| g.into_workspace(state));

        try_join_all(futures).await
    }

    pub fn status(&self) -> ContainerSummaryStateEnum {
        self.containers
            .iter()
            .map(|c| c.state)
            .max()
            .unwrap_or(ContainerSummaryStateEnum::EMPTY)
    }

    pub fn created(&self) -> Option<i64> {
        self.containers.iter().filter_map(|c| c.created).min()
    }

    pub fn service_container_id(&self) -> eyre::Result<&str> {
        // FIXME: We need to find the correct service container.
        Ok(&self
            .containers
            .first()
            .ok_or_else(|| eyre!("no containers for workspace"))?
            .id)
    }
}

// Group of containers by worktree path
struct ContainerGroup {
    path: PathBuf,
    containers: Vec<ContainerInfo>,
}

impl ContainerGroup {
    async fn list(state: &State) -> eyre::Result<Vec<Self>> {
        let worktree_paths = worktree::list(&state.project.path).await?;
        let containers = state.docker.container_info().await?;

        let mut groups: HashMap<PathBuf, ContainerGroup> = HashMap::new();
        for c in containers {
            if c.dc_project.as_ref() != Some(&state.project_name) {
                // This is a dc-managed container for a different project.
                continue;
            }
            if c.dc_project.is_none() && !worktree_paths.contains(&c.local_folder) {
                // This is not a devcontainer for any of our worktrees.
                continue;
            }
            let group = groups
                .entry(c.local_folder.clone())
                .or_insert_with(|| ContainerGroup {
                    path: c.local_folder.clone(),
                    containers: Vec::new(),
                });
            group.containers.push(c);
        }

        // Ensure we have an entry for all of our worktrees.
        for path in worktree_paths {
            groups
                .entry(path.clone())
                .or_insert_with(|| ContainerGroup {
                    path,
                    containers: Vec::new(),
                });
        }
        Ok(groups.into_values().collect())
    }

    async fn into_workspace(self, state: &State) -> eyre::Result<Workspace> {
        let dirty = if self.path.exists() {
            !Command::new("git")
                .args(["status", "--porcelain"])
                .current_dir(&self.path)
                .output()
                .await?
                .stdout
                .is_empty()
        } else {
            false
        };

        let execs_futures = try_join_all(self.containers.iter().map(|c| state.docker.execs(&c.id)));
        let stats_futures = try_join_all(self.containers.iter().map(|c| state.docker.stats(&c.id)));
        let (execs, stats) = tokio::try_join!(execs_futures, stats_futures)?;

        let execs = execs.into_iter().flatten().collect();
        let stats = stats.into_iter().sum();

        let root = self.path == state.project.path;
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        Ok(Workspace {
            compose_project_name: compose_project_name(&self.path),
            path: self.path,
            name,
            root,
            containers: self.containers,
            dirty,
            execs,
            stats,
        })
    }
}
