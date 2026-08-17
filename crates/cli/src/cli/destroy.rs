use std::borrow::Cow;

use clap::Args;
use clap_complete::ArgValueCompleter;
use docker::{PROJECT_LABEL, WORKSPACE_LABEL};
use eyre::eyre;

use crate::ansi::{RED, RESET, YELLOW};
use crate::cli::go::go;
use crate::cli::{State, confirm, safety_check};
use crate::complete::complete_workspace;
use crate::config::Config;
use crate::docker::compose::{self, compose_cmd, remove_override_file};
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

        // Grab this before the worktree goes away; once it's removed, the cwd
        // no longer resolves.
        let cwd = std::env::current_dir().ok();

        let cleanup = Cleanup {
            devcontainer: devcontainer.as_ref(),
            workspace: &workspace,
            force: self.force,
        };

        Runner::run(cleanup).await?;

        // We just deleted the directory the shell is sitting in, so move to the
        // project root. Destroying the root leaves its directory in place, so
        // there's nowhere to go.
        if !workspace.is_root && cwd.is_some_and(|cwd| cwd.starts_with(&workspace.path)) {
            go(&state.project.path)?;
        }

        Ok(())
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
            let project_name = compose::project_name(devcontainer, self.workspace).await?;

            match compose::ensure_project_unclaimed(devcontainer, self.workspace, project_name)
                .await
            {
                Ok(()) => {
                    let mut down_cmd = compose_cmd(devcontainer, self.workspace).await?;
                    down_cmd.args(["down", "-v", "--rmi", "local", "--remove-orphans"]);

                    run_command(down_cmd, "docker compose down").await?;
                }
                Err(e) => tracing::warn!("skipping `docker compose down`: {e:#}"),
            }

            remove_override_file(self.workspace);

            let client = &devcontainer.docker().await?.client;

            // Remove any port-forward sidecars targeting this workspace
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

            // `compose down --rmi local` skips images with a custom `image` tag, so remove them ourselves.
            if let Ok(images) = client
                .list_images()
                .with_label(PROJECT_LABEL, self.workspace.state.project_name.as_str())
                .with_label(WORKSPACE_LABEL, self.workspace.name.as_str())
                .call()
                .await
            {
                for image in images {
                    match client.remove_image(&image.id).force(true).call().await {
                        Ok(()) | Err(docker::Error::NotFound) => {}
                        Err(e) => {
                            tracing::warn!(image = %image.id, "failed to remove image: {e}");
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

            run_command(worktree_cmd, "git worktree remove").await?;
        }

        eprintln!("Removed {}", self.workspace.path.display());
        Ok(())
    }
}
