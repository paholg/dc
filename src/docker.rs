use std::{collections::HashMap, path::PathBuf};

use bollard::{
    Docker,
    query_parameters::{ListContainersOptions, StatsOptions},
    secret::ContainerSummaryStateEnum,
};
use derive_more::{Add, AddAssign, Sum};
use eyre::eyre;
use futures::{StreamExt, future::try_join_all};

#[derive(Debug)]
pub struct ContainerInfo {
    pub id: String,
    pub state: ContainerSummaryStateEnum,
    pub local_folder: PathBuf,
    pub dc_project: Option<String>,
    pub created: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ExecSession {
    pub pid: u32,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Add, AddAssign, Sum)]
pub struct Stats {
    /// Current memory use in bytes.
    pub ram: u64,
}
pub struct DockerClient {
    // TODO: Instead of making this public, we should move all docker functionality we need to this
    // module.
    pub docker: Docker,
}

impl DockerClient {
    pub async fn new() -> eyre::Result<Self> {
        let docker = Docker::connect_with_local_defaults()?;
        Ok(Self { docker })
    }

    /// Return all containers labeled with `devcontainer.local_folder`.
    pub async fn container_info(&self) -> eyre::Result<Vec<ContainerInfo>> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec!["devcontainer.local_folder".to_string()],
        );
        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters: Some(filters),
                ..Default::default()
            }))
            .await?;

        let mut result = Vec::new();
        for c in containers {
            let mut labels = c.labels.ok_or_else(|| eyre!("container missing labels"))?;
            let local_folder = labels.remove("devcontainer.local_folder")
                .ok_or_else(|| eyre!("container was filtered by devcontainer.local_folder, but does not have that label"))?.into();
            let dc_project = labels.remove("dev.dc.project");
            let id = c.id.ok_or_else(|| eyre!("container missing id"))?;
            let state = c.state.ok_or_else(|| eyre!("container missing state"))?;

            result.push(ContainerInfo {
                id,
                state,
                local_folder,
                dc_project,
                created: c.created,
            });
        }

        Ok(result)
    }

    pub async fn stats(&self, container_id: &str) -> eyre::Result<Stats> {
        let mut stream = self.docker.stats(
            container_id,
            Some(StatsOptions {
                stream: false,
                one_shot: true,
            }),
        );
        match stream.next().await {
            Some(Ok(stats)) => {
                let ram = stats
                    .memory_stats
                    .as_ref()
                    .and_then(|m| m.usage)
                    .unwrap_or_else(|| {
                        tracing::warn!("missing memory stats for container {container_id}");
                        0
                    });
                Ok(Stats { ram })
            }
            Some(Err(e)) => Err(e.into()),
            None => Err(eyre!("no stats response for container {container_id}")),
        }
    }

    pub async fn execs(&self, container_id: &str) -> eyre::Result<Vec<ExecSession>> {
        let info = self.docker.inspect_container(container_id, None).await?;
        let exec_ids = info.exec_ids.unwrap_or_default();

        let futures = exec_ids
            .into_iter()
            .map(async |eid| -> eyre::Result<Option<ExecSession>> {
                let exec = self.docker.inspect_exec(&eid).await?;
                if exec.running != Some(true) {
                    return Ok(None);
                }
                let pid = exec.pid.ok_or_else(|| eyre!("running exec has no PID"))? as u32;
                let mut command = Vec::new();
                if let Some(ref pc) = exec.process_config {
                    if let Some(ref ep) = pc.entrypoint {
                        command.push(ep.clone());
                    }
                    if let Some(ref args) = pc.arguments {
                        command.extend(args.iter().cloned());
                    }
                }
                Ok(Some(ExecSession { pid, command }))
            });

        let execs = try_join_all(futures).await?.into_iter().flatten().collect();
        Ok(execs)
    }
}
