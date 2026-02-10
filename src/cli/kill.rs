use crate::ansi::{RED, RESET, YELLOW};
use crate::config::Config;
use crate::devcontainer::DevContainer;
use crate::run::Runner;
use bollard::Docker;
use clap::Args;
use eyre::eyre;

use super::prune::{Cleanup, confirm};

/// Destroy a workspace by name, removing its containers and worktree.
///
/// Unlike `prune`, this does not skip dirty or in-use workspaces.
#[derive(Debug, Args)]
pub struct Kill {
    #[arg(help = "name of the workspace to destroy")]
    name: String,

    #[arg(
        short,
        long,
        help = "name of project [default: The first one configured]"
    )]
    project: Option<String>,

    #[arg(short, long, help = "force remove worktrees")]
    force: bool,
}

impl Kill {
    pub async fn run(self, docker: &Docker, config: &Config) -> eyre::Result<()> {
        let (_, project) = config.project(self.project.as_deref())?;
        let dc = DevContainer::load(project)?;
        let dc_options = dc.common.customizations.dc;
        let workspace_dir = dc_options.workspace_dir();

        let worktree_path = workspace_dir.join(&self.name);

        let is_root = worktree_path == project.path;

        if !is_root && !worktree_path.exists() {
            return Err(eyre!("no workspace named '{}' found", self.name));
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
            docker,
            repo_path: &project.path,
            path: &worktree_path,
            compose_name: super::up::compose_project_name(&worktree_path),
            remove_worktree: !is_root,
            force: self.force,
        };

        Runner::run(cleanup).await
    }
}
