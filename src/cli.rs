use std::env;

use clap::{Parser, Subcommand};
use clap_complete::engine::ArgValueCompleter;
use eyre::OptionExt;

use crate::{
    complete,
    config::{Config, Project},
    devcontainer::DevContainer,
    docker::DockerClient,
    worktree,
};

mod compose;
mod copy;
mod exec;
mod fwd;
mod kill;
mod list;
mod prune;
mod show;
pub(crate) mod up;

const ABOUT: &str = "TODO";

#[derive(Debug, Parser)]
#[command(version, about = ABOUT)]
pub struct Cli {
    #[arg(
        short,
        long,
        help = "name of project [default: The DC_PROJECT variable, then the first configured project]",
        add = ArgValueCompleter::new(complete::complete_project),
    )]
    pub project: Option<String>,

    /// workspace, not used for all commands [default: current working directory]
    #[arg(short, long, add = ArgValueCompleter::new(complete::complete_workspace))]
    workspace: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    #[command()]
    Up(up::Up),
    #[command(visible_alias = "x")]
    Exec(exec::Exec),
    #[command()]
    Fwd(fwd::Fwd),
    #[command()]
    List(list::List),
    #[command(visible_alias = "c")]
    Compose(compose::Compose),
    #[command()]
    Prune(prune::Prune),
    #[command()]
    Kill(kill::Kill),
    #[command()]
    Copy(copy::Copy),
    Show(show::Show),
}

pub struct State {
    pub docker: DockerClient,
    pub project_name: String,
    pub project: Project,
    pub workspace: Option<String>,
}

impl State {
    // TODO: We should just load this at start.
    fn devcontainer(&self) -> eyre::Result<DevContainer> {
        DevContainer::load(&self.project)
    }

    pub fn is_root(&self, name: &str) -> bool {
        self.project
            .path
            .file_name()
            .is_some_and(|root| name == root)
    }

    /// If a name was given, return it. Otherwise, return the name of the
    /// worktree we're currently inside.
    pub async fn resolve_workspace(&self) -> eyre::Result<String> {
        if let Some(n) = &self.workspace {
            return Ok(n.clone());
        }

        let cwd = env::current_dir()?;
        let worktrees = worktree::list(&self.project.path).await?;

        worktrees.into_iter().find(|wt| wt == &cwd).ok_or_else(|| {
            eyre::eyre!("not inside a worktree of project '{}'", self.project_name)
        })?;

        Ok(cwd
            .file_name()
            .ok_or_eyre("worktree path has no basename")?
            .to_string_lossy()
            .to_string())
    }
}

impl Cli {
    pub async fn run(self) -> eyre::Result<()> {
        let config = Config::load()?;
        let (project_name, project) = config.project(self.project)?;

        let state = State {
            docker: DockerClient::new().await?,
            project_name,
            project,
            workspace: self.workspace,
        };

        match self.command {
            Commands::Up(up) => up.run(state).await,
            Commands::Exec(exec) => exec.run(state).await,
            Commands::Fwd(fwd) => fwd.run(state).await,
            Commands::List(list) => list.run(state).await,
            Commands::Compose(compose) => compose.run(state).await,
            Commands::Prune(prune) => prune.run(state).await,
            Commands::Kill(kill) => kill.run(state).await,
            Commands::Copy(copy) => copy.run(state).await,
            Commands::Show(show) => show.run(state).await,
        }
    }
}
