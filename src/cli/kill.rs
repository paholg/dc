use clap::Args;
use eyre::eyre;

use crate::ansi::{RED, RESET, YELLOW};
use crate::cli::State;
use crate::run::Runner;
use crate::workspace::Workspace;

use super::prune::{Cleanup, confirm};

/// Destroy a workspace by name, removing its containers and worktree
#[derive(Debug, Args)]
pub struct Kill {
    /// force remove worktrees
    #[arg(short, long)]
    force: bool,
}

impl Kill {
    pub async fn run(self, state: State) -> eyre::Result<()> {
        let name = state.resolve_workspace().await?;
        let workspace = Workspace::get(&state, &name).await?;

        let is_root = workspace.path == state.project.path;

        if !workspace.path.exists() {
            return Err(eyre!("no workspace named '{}' found", name));
        }

        if is_root {
            eprintln!(
                "{YELLOW}Will destroy {RED}root{YELLOW} workspace — DATA WILL BE LOST{RESET}",
            );
            if !confirm()? {
                eprintln!("Aborted.");
                return Ok(());
            }
        }

        let cleanup = Cleanup {
            docker: &state.docker.docker,
            repo_path: &state.project.path,
            path: &workspace.path,
            compose_name: super::up::compose_project_name(&workspace.path),
            remove_worktree: !is_root,
            force: self.force,
        };

        Runner::run(cleanup).await
    }
}
