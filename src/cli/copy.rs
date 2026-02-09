use std::borrow::Cow;
use std::path::Path;

use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptions, CreateImageOptionsBuilder, RemoveContainerOptions,
};
use clap::Args;
use eyre::eyre;
use futures::StreamExt;

use crate::config::Config;
use crate::devcontainer::DevContainer;
use crate::runner::Runnable;
use crate::workspace::Speed::Fast;
use crate::workspace::{Workspace, pick_workspace_any};

/// Copy named volumes from one workspace to another
///
/// Useful for sharing expensive-to-rebuild caches (e.g. cargo registry,
/// node_modules) between workspaces.
#[derive(Debug, Args)]
#[command(verbatim_doc_comment)]
pub struct Copy {
    #[arg(short, long)]
    project: Option<String>,

    #[arg(long)]
    from: Option<String>,

    #[arg(long)]
    to: Option<String>,

    /// Volume names to copy [default: configured defaultCopyVolumes]
    volumes: Vec<String>,
}

fn find_workspace(workspaces: Vec<Workspace>, name: &str) -> eyre::Result<Workspace> {
    workspaces
        .into_iter()
        .find(|ws| ws.path.file_name().map(|f| f == name).unwrap_or(false))
        .ok_or_else(|| eyre!("no workspace found with name: {name}"))
}

impl Copy {
    pub async fn run(self, docker: &Docker, config: &Config) -> eyre::Result<()> {
        let workspaces =
            Workspace::list_project(docker, self.project.as_deref(), config, Fast).await?;

        let from_ws = if let Some(ref name) = self.from {
            find_workspace(workspaces.clone(), name)?
        } else {
            pick_workspace_any(workspaces.clone(), "no workspaces found", "Copy from:")?
        };

        let mut remaining = workspaces;
        remaining.retain(|w| w.compose_project_name != from_ws.compose_project_name);

        let to_ws = if let Some(ref name) = self.to {
            find_workspace(remaining, name)?
        } else {
            pick_workspace_any(remaining, "no other workspaces found", "Copy to:")?
        };

        let volumes = if !self.volumes.is_empty() {
            self.volumes
        } else {
            let (_, project) = config.project(self.project.as_deref())?;
            let dc = DevContainer::load(project)?;
            dc.common
                .customizations
                .dc
                .default_copy_volumes
                .ok_or_else(|| eyre!("no volumes specified and no defaultCopyVolumes configured"))?
        };

        copy_volumes(
            docker,
            &volumes,
            &from_ws.compose_project_name,
            &to_ws.compose_project_name,
        )
        .await
    }
}

pub(crate) async fn copy_volumes(
    docker: &Docker,
    volumes: &[String],
    from_project: &str,
    to_project: &str,
) -> eyre::Result<()> {
    let batch = CopyVolumes::new(docker, volumes, from_project, to_project);
    crate::runner::run("copy volumes", &batch, None).await
}

struct CopyVolume<'a> {
    docker: &'a Docker,
    name: String,
    src: String,
    dst: String,
}

struct CopyVolumes<'a> {
    copies: Vec<CopyVolume<'a>>,
}

impl<'a> CopyVolumes<'a> {
    fn new(docker: &'a Docker, volumes: &[String], from_project: &str, to_project: &str) -> Self {
        let copies = volumes
            .iter()
            .map(|vol| CopyVolume {
                docker,
                name: vol.clone(),
                src: format!("{from_project}_{vol}"),
                dst: format!("{to_project}_{vol}"),
            })
            .collect();
        Self { copies }
    }
}

impl Runnable for CopyVolume<'_> {
    fn command(&self) -> Cow<'_, str> {
        format!("{} -> {}", self.src, self.dst).into()
    }

    async fn run(&self, _dir: Option<&Path>) -> eyre::Result<()> {
        do_copy_volume(self.docker, &self.src, &self.dst).await
    }
}

impl Runnable for CopyVolumes<'_> {
    fn command(&self) -> Cow<'_, str> {
        let names: Vec<_> = self.copies.iter().map(|c| c.name.as_str()).collect();
        names.join(", ").into()
    }

    async fn run(&self, _dir: Option<&Path>) -> eyre::Result<()> {
        let labeled: Vec<_> = self.copies.iter().map(|c| (c.name.as_str(), c)).collect();
        crate::runner::run_parallel(labeled).await
    }
}

const IMAGE: &str = "docker.io/library/alpine:latest";

async fn ensure_image(docker: &Docker) -> eyre::Result<()> {
    if docker.inspect_image(IMAGE).await.is_ok() {
        return Ok(());
    }
    docker
        .create_image(
            Some(CreateImageOptionsBuilder::new().from_image(IMAGE).build()),
            None,
            None,
        )
        .collect::<Vec<_>>()
        .await;
    Ok(())
}

async fn do_copy_volume(docker: &Docker, src: &str, dst: &str) -> eyre::Result<()> {
    ensure_image(docker).await?;
    let container = docker
        .create_container(
            Some(CreateContainerOptions {
                name: None,
                ..Default::default()
            }),
            ContainerCreateBody {
                image: Some(IMAGE.to_string()),
                cmd: Some(vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "cp -a /from/. /to/".to_string(),
                ]),
                host_config: Some(HostConfig {
                    binds: Some(vec![format!("{src}:/from"), format!("{dst}:/to")]),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await?;

    let id = &container.id;
    let result = async {
        docker.start_container(id, None).await?;
        let mut stream = docker.wait_container(id, None);
        let resp = stream
            .next()
            .await
            .ok_or_else(|| eyre!("wait_container stream ended without response"))??;
        if resp.status_code != 0 {
            return Err(eyre!(
                "copy container exited with status {}",
                resp.status_code
            ));
        }
        Ok(())
    }
    .await;

    docker
        .remove_container(
            id,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await?;

    result
}
