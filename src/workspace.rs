use std::collections::HashMap;
use std::path::PathBuf;

use bollard::models::ContainerSummaryStateEnum;
use eyre::eyre;
use futures::future::try_join_all;

use crate::cli::State;
use crate::cli::up::compose_project_name;
use crate::docker::{ContainerInfo, ExecSession, Stats};
use crate::worktree;

pub mod git_status;
pub mod table;

#[derive(Debug)]
pub struct Workspace {
    pub path: PathBuf,
    pub name: String,
    pub root: bool,
    pub compose_project_name: String,
    pub containers: Vec<ContainerInfo>,
    pub git_status: git_status::GitStatus,
    pub execs: Vec<ExecSession>,
    pub stats: Stats,
    pub fwd_ports: Vec<u16>,
    pub docker_ports: Vec<u16>,
    pub dc_managed: bool,
}

impl Workspace {
    pub async fn get(state: &State, name: &str) -> eyre::Result<Workspace> {
        let (groups, fwd_ports) = ContainerGroup::list(state).await?;

        let group = if state.is_root(name) {
            groups
                .into_iter()
                .find(|g| g.path == state.project.path)
                .ok_or_else(|| eyre!("root workspace not found"))?
        } else {
            groups
                .into_iter()
                .find(|g| g.path.file_name().is_some_and(|f| f == name))
                .ok_or_else(|| eyre!("no workspace found for name {name}"))?
        };
        group.into_workspace(state, &fwd_ports).await
    }

    pub async fn list(state: &State) -> eyre::Result<Vec<Workspace>> {
        let (groups, fwd_ports) = ContainerGroup::list(state).await?;
        let futures = groups
            .into_iter()
            .map(|g| g.into_workspace(state, &fwd_ports));

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

    pub fn is_dirty(&self) -> bool {
        self.git_status.is_dirty()
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
    async fn list(state: &State) -> eyre::Result<(Vec<Self>, HashMap<String, Vec<u16>>)> {
        let worktree_paths = worktree::list(&state.project.path).await?;
        let (containers, fwd_ports) = tokio::try_join!(
            state.docker.container_info(),
            state.docker.forwarded_ports(&state.project_name),
        )?;

        let mut groups: HashMap<PathBuf, ContainerGroup> = HashMap::new();
        for c in containers {
            if c.dc_project
                .as_ref()
                .is_some_and(|p| p != &state.project_name)
            {
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
        Ok((groups.into_values().collect(), fwd_ports))
    }

    async fn into_workspace(
        self,
        state: &State,
        fwd_ports: &HashMap<String, Vec<u16>>,
    ) -> eyre::Result<Workspace> {
        let git_future = git_status::GitStatus::fetch(&self.path);
        let execs_futures = try_join_all(self.containers.iter().map(|c| state.docker.execs(&c.id)));
        let stats_futures = try_join_all(self.containers.iter().map(|c| state.docker.stats(&c.id)));
        let (git_status, execs, stats) =
            tokio::try_join!(git_future, execs_futures, stats_futures)?;

        let execs = execs.into_iter().flatten().collect();
        let stats = stats.into_iter().sum();

        let root = self.path == state.project.path;
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let compose_project_name = compose_project_name(&self.path);
        let mut fwd_ports = fwd_ports
            .get(&compose_project_name)
            .cloned()
            .unwrap_or_default();
        fwd_ports.sort();
        fwd_ports.dedup();

        let mut docker_ports: Vec<u16> = self
            .containers
            .iter()
            .flat_map(|c| &c.host_ports)
            .copied()
            .collect();
        docker_ports.sort();
        docker_ports.dedup();

        let dc_managed = self.containers.iter().any(|c| c.dc_project.is_some());

        Ok(Workspace {
            compose_project_name,
            path: self.path,
            name,
            root,
            containers: self.containers,
            git_status,
            execs,
            stats,
            fwd_ports,
            docker_ports,
            dc_managed,
        })
    }
}
