use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::Path;

use crate::ansi::{CYAN, GREEN, RED, RESET, YELLOW};
use crate::config::Config;
use crate::run::{self, Runnable, Runner};
use crate::workspace::{Speed, Workspace, workspace_table};
use bollard::Docker;
use bollard::query_parameters::{ListContainersOptions, RemoveContainerOptions};
use clap::Args;
use tokio::process::Command;

#[derive(Debug, Args)]
pub struct Prune {
    #[arg(short, long, help = "name of project [default: all]")]
    project: Option<String>,

    #[arg(short, long, help = "skip confirmation prompt")]
    yes: bool,
}

impl Prune {
    pub async fn run(self, docker: &Docker, config: &Config) -> eyre::Result<()> {
        let workspaces =
            Workspace::list_project(docker, self.project.as_deref(), config, Speed::Slow).await?;

        let mut in_use = Vec::new();
        let mut dirty = Vec::new();
        let mut to_clean = Vec::new();

        for ws in &workspaces {
            let (_, proj) = config.project(Some(&ws.project))?;
            if ws.path == proj.path || !ws.execs.is_empty() {
                in_use.push(ws);
            } else if ws.dirty {
                dirty.push(ws);
            } else {
                to_clean.push(ws);
            }
        }

        if !in_use.is_empty() {
            eprintln!("{GREEN}In Use{RESET} ({CYAN}skipping{RESET}):");
            eprint!("{}", workspace_table(in_use.iter().copied())?);
            eprintln!();
        }
        if !dirty.is_empty() {
            eprintln!("{RED}Dirty{RESET} ({CYAN}skipping{RESET}):");
            eprint!("{}", workspace_table(dirty.iter().copied())?);
            eprintln!();
        }

        if to_clean.is_empty() {
            return Ok(());
        }

        eprintln!("{YELLOW}Will Remove - DATA WILL BE LOST{RESET}:");
        eprint!("{}", workspace_table(to_clean.iter().copied())?);
        eprintln!();

        if !self.yes && !confirm()? {
            eprintln!("Aborted.");
            return Ok(());
        }

        let cleanups: Vec<Cleanup> = to_clean
            .iter()
            .map(|ws| {
                let (_, proj) = config.project(Some(&ws.project))?;
                Ok(Cleanup {
                    docker,
                    repo_path: &proj.path,
                    path: &ws.path,
                    compose_name: super::up::compose_project_name(&ws.path),
                    remove_worktree: ws.path.exists(),
                    force: false,
                })
            })
            .collect::<eyre::Result<Vec<_>>>()?;

        Runner::run_parallel("prune", cleanups).await
    }
}

pub(super) struct Cleanup<'a> {
    pub(super) docker: &'a Docker,
    pub(super) repo_path: &'a Path,
    pub(super) path: &'a Path,
    pub(super) compose_name: String,
    pub(super) remove_worktree: bool,
    pub(super) force: bool,
}

impl Runnable for Cleanup<'_> {
    fn name(&self) -> Cow<'_, str> {
        self.path.display().to_string().into()
    }

    fn description(&self) -> Cow<'_, str> {
        format!("prune {}", self.path.display()).into()
    }

    async fn run(self, _: run::Token) -> eyre::Result<()> {
        let down_result = Command::new("docker")
            .args([
                "compose",
                "-p",
                &self.compose_name,
                "down",
                "-v",
                "--remove-orphans",
            ])
            .status()
            .await;

        down_result?;

        let override_file =
            std::env::temp_dir().join(format!("{}-override.yml", self.compose_name));
        if override_file.exists() {
            std::fs::remove_file(&override_file)?;
        }

        // Remove any port-forward sidecar targeting this workspace
        let mut filters = HashMap::new();
        filters.insert(
            "label".into(),
            vec![format!("dev.dc.fwd.workspace={}", self.compose_name)],
        );
        if let Ok(containers) = self
            .docker
            .list_containers(Some(ListContainersOptions {
                all: true,
                filters: Some(filters),
                ..Default::default()
            }))
            .await
        {
            for c in containers {
                if let Some(id) = c.id {
                    let _ = self
                        .docker
                        .remove_container(
                            &id,
                            Some(RemoveContainerOptions {
                                force: true,
                                ..Default::default()
                            }),
                        )
                        .await;
                }
            }
        }

        if self.remove_worktree {
            let mut args = vec!["worktree", "remove"];
            if self.force {
                args.push("--force");
            }
            let path_str = self.path.to_string_lossy();
            args.push(&path_str);

            let status = Command::new("git")
                .args(&args)
                .current_dir(self.repo_path)
                .status()
                .await?;
            eyre::ensure!(status.success(), "git worktree remove failed");
        }

        eprintln!("Removed {}", self.path.display());
        Ok(())
    }
}

pub(super) fn confirm() -> eyre::Result<bool> {
    eprint!("Proceed? [y/N] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().eq_ignore_ascii_case("y"))
}
