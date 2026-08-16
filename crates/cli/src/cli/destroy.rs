use std::borrow::Cow;

use clap::Args;
use clap_complete::ArgValueCompleter;
use docker::{PROJECT_LABEL, WORKSPACE_LABEL};
use eyre::eyre;

use crate::ansi::{RED, RESET, YELLOW};
use crate::cli::{State, confirm, safety_check};
use crate::complete::complete_workspace;
use crate::config::Config;
use crate::docker::compose::{compose_cmd, remove_override_file};
use crate::run::{self, Runnable, Runner, run_command};
use crate::state::DevcontainerState;
use crate::workspace::Workspace;

/// Fully destroy the workspace; equivalent to `docker compose down -v --rmi local --remove-orphans && git worktree remove`
#[derive(Debug, Args)]
pub(crate) struct Destroy {
    /// Workspace name
    #[arg(add = ArgValueCompleter::new(complete_workspace))]
    workspace: Option<String>,

    /// Force remove the worktree, even if dirty
    #[arg(short, long)]
    force: bool,
}

impl Destroy {
    pub(crate) async fn run(self, project: Option<String>) -> eyre::Result<()> {
        let config = Config::load()?;
        let state = State::new(project, &config).await?;
        let workspace = state.resolve_workspace(self.workspace).await?;
        // A workspace whose devcontainer.json is unusable still has to be
        // destroyable, so this is not fatal — but without the config we can't
        // compose-down, so say so rather than leaving containers behind
        // silently.
        let devcontainer = match state.devcontainer_for(&workspace.path) {
            Ok(devcontainer) => Some(devcontainer),
            Err(e) => {
                tracing::warn!(
                    "could not load the devcontainer config; containers for this workspace may \
                     be left behind: {e:#}"
                );
                None
            }
        };

        if !workspace.path.exists() {
            return Err(eyre!("workspace '{}' not found", workspace.name));
        }

        safety_check(&workspace, self.force).await?;

        if workspace.is_root {
            eprintln!(
                "{YELLOW}Will destroy {RED}root{YELLOW} workspace — DATA WILL BE LOST{RESET}",
            );
            if !confirm()? {
                eprintln!("Aborted.");
                return Ok(());
            }
        }

        let cleanup = Cleanup {
            devcontainer: devcontainer.as_ref(),
            workspace: &workspace,
            force: self.force,
        };

        Runner::run(cleanup).await
    }
}

struct Cleanup<'a> {
    devcontainer: Option<&'a DevcontainerState>,
    workspace: &'a Workspace<'a>,
    force: bool,
}

impl Runnable for Cleanup<'_> {
    fn name(&self) -> Cow<'_, str> {
        (&self.workspace.name).into()
    }

    fn description(&self) -> Cow<'_, str> {
        format!("destroy {}", self.workspace.path.display()).into()
    }

    async fn run(self, _: run::Token) -> eyre::Result<()> {
        if let Some(devcontainer) = self.devcontainer {
            let mut down_cmd = compose_cmd(devcontainer, self.workspace)?;
            down_cmd.args(["down", "-v", "--rmi", "local", "--remove-orphans"]);

            run_command(down_cmd).await?;
            remove_override_file(self.workspace);

            // Remove any port-forward sidecars targeting this workspace
            let client = &devcontainer.docker().await?.client;
            if let Ok(summaries) = client
                .list_containers()
                .all(true)
                .with_label(PROJECT_LABEL, self.workspace.state.project_name.as_str())
                .with_label(WORKSPACE_LABEL, self.workspace.name.as_str())
                .call()
                .await
            {
                for c in summaries {
                    match client.remove_container(&c.id).force(true).call().await {
                        Ok(()) | Err(docker::Error::NotFound) => {}
                        Err(e) => {
                            tracing::warn!(container = %c.id, "failed to remove sidecar: {e}");
                        }
                    }
                }
            }
        }

        if !self.workspace.is_root {
            // Swallow errors; we don't care if it was not locked.
            let _ = tokio::process::Command::new("git")
                .args(["worktree", "unlock"])
                .arg(&self.workspace.path)
                .current_dir(&self.workspace.state.project.path)
                .output()
                .await;

            let mut worktree_cmd = tokio::process::Command::new("git");
            worktree_cmd.args(["worktree", "remove"]);

            if self.force {
                worktree_cmd.arg("--force");
            }
            worktree_cmd.arg(&self.workspace.path);
            worktree_cmd.current_dir(&self.workspace.state.project.path);

            run_command(worktree_cmd).await?;
        }

        eprintln!("Removed {}", self.workspace.path.display());
        Ok(())
    }
}
