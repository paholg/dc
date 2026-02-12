use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::Path;

use eyre::WrapErr;

use crate::ansi::{CYAN, GREEN, RED, RESET, YELLOW};
use crate::cli::State;
use crate::run::pty::run_in_pty;
use crate::run::{self, Runnable, Runner};
use crate::workspace::Workspace;
use crate::workspace::table::workspace_table;
use bollard::Docker;
use bollard::query_parameters::{ListContainersOptions, RemoveContainerOptions};
use clap::Args;

#[derive(Debug, Args)]
pub struct Prune;

impl Prune {
    pub async fn run(self, state: State) -> eyre::Result<()> {
        let workspaces = Workspace::list(&state).await?;

        let mut in_use = Vec::new();
        let mut dirty = Vec::new();
        let mut to_clean = Vec::new();

        for ws in &workspaces {
            if ws.path == state.project.path || !ws.execs.is_empty() {
                in_use.push(ws);
            } else if ws.is_dirty() {
                dirty.push(ws);
            } else {
                to_clean.push(ws);
            }
        }

        if !in_use.is_empty() {
            eprintln!("{GREEN}In Use{RESET} ({CYAN}skipping{RESET}):");
            eprint!("{}", workspace_table(in_use.iter().copied()));
            eprintln!();
        }
        if !dirty.is_empty() {
            eprintln!("{RED}Dirty{RESET} ({CYAN}skipping{RESET}):");
            eprint!("{}", workspace_table(dirty.iter().copied()));
            eprintln!();
        }

        if to_clean.is_empty() {
            return Ok(());
        }

        eprintln!("{YELLOW}Will Remove - DATA WILL BE LOST{RESET}:");
        eprint!("{}", workspace_table(to_clean.iter().copied()));
        eprintln!();

        if !confirm()? {
            eprintln!("Aborted.");
            return Ok(());
        }

        let cleanups: Vec<Cleanup> = to_clean
            .iter()
            .map(|ws| {
                Ok(Cleanup {
                    docker: &state.docker.docker,
                    repo_path: &state.project.path,
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
        self.path
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or(self.path.display().to_string().into())
    }

    fn description(&self) -> Cow<'_, str> {
        format!("prune {}", self.path.display()).into()
    }

    async fn run(self, _: run::Token) -> eyre::Result<()> {
        run_in_pty(
            &[
                "docker",
                "compose",
                "--progress",
                "plain",
                "-p",
                &self.compose_name,
                "down",
                "-v",
                "--remove-orphans",
            ],
            None,
        )
        .await?;

        let override_file =
            std::env::temp_dir().join(format!("{}-override.yml", self.compose_name));
        if override_file.exists() {
            std::fs::remove_file(&override_file)
                .wrap_err_with(|| format!("failed to remove {}", override_file.display()))?;
        }

        // Remove any port-forward sidecar targeting this workspace
        let mut filters = HashMap::new();
        filters.insert(
            "label".into(),
            vec![format!("dev.dc.workspace={}", self.compose_name)],
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
            let mut args = vec!["git", "worktree", "remove"];
            if self.force {
                args.push("--force");
            }
            let path_str = self.path.to_string_lossy();
            args.push(&path_str);

            run_in_pty(&args, Some(self.repo_path)).await?;
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
