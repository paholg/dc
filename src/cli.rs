use std::env;

use clap::{Parser, Subcommand};
use clap_complete::engine::ArgValueCompleter;

use crate::{
    complete,
    config::{Config, Project},
    devcontainer::DevContainer,
    docker::DockerClient,
};

mod copy;
mod exec;
mod fwd;
mod kill;
mod list;
mod prune;
pub(crate) mod up;

const ABOUT: &str = "TODO";

#[derive(Debug, Parser)]
#[command(version, about = ABOUT)]
pub struct Cli {
    #[arg(
        short,
        long,
        help = "name of project [default: The DC_PROJECT variable, falling back to the first configured project]",
        add = ArgValueCompleter::new(complete::complete_project),
    )]
    project: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

pub struct State {
    pub docker: DockerClient,
    pub project_name: String,
    pub project: Project,
}

impl State {
    // TODO: We should just load this at start.
    fn devcontainer(&self) -> eyre::Result<DevContainer> {
        DevContainer::load(&self.project)
    }

    pub fn is_root(&self, name: Option<&str>) -> bool {
        match name {
            Some(name) => self
                .project
                .path
                .file_name()
                .is_some_and(|root| name == root),
            None => true,
        }
    }
}

impl Cli {
    pub async fn run(self) -> eyre::Result<()> {
        let config = Config::load()?;
        let project_name = self.project.or_else(|| env::var("DC_PROJECT").ok());
        let (project_name, project) = config.project(project_name.as_deref())?;

        let state = State {
            docker: DockerClient::new().await?,
            project_name,
            project,
        };

        match self.command {
            Commands::Up(up) => up.run(state).await,
            Commands::Exec(exec) => exec.run(state).await,
            Commands::Fwd(fwd) => fwd.run(state).await,
            Commands::List(list) => list.run(state).await,
            Commands::Prune(prune) => prune.run(state).await,
            Commands::Kill(kill) => kill.run(state).await,
            Commands::Copy(copy) => copy.run(state).await,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    #[command(visible_alias = "u")]
    Up(up::Up),
    #[command(visible_alias = "x")]
    Exec(exec::Exec),
    #[command(visible_alias = "f")]
    Fwd(fwd::Fwd),
    #[command(visible_alias = "l")]
    List(list::List),
    /// Clean up any workspaces not actively in use.
    ///
    /// Here, "actively in use" means you have it open in vscode or a
    /// `docker exec` session, or that you have uncommited git changes -- this
    /// will other running containers and delete their data.
    #[command()]
    Prune(prune::Prune),
    /// Destroy a specific workspace by name.
    ///
    /// Unlike `prune`, this does not skip dirty or in-use workspaces.
    #[command(visible_alias = "k")]
    Kill(kill::Kill),
    #[command()]
    Copy(copy::Copy),
}
