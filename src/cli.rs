use bollard::Docker;
use clap::{Parser, Subcommand};

use crate::config::Config;

mod copy;
mod exec;
mod fwd;
mod list;
mod prune;
pub(crate) mod up;

const ABOUT: &str = "TODO";

#[derive(Debug, Parser)]
#[command(version, about = ABOUT, flatten_help = false)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

impl Cli {
    pub async fn run(self, docker: &Docker, config: &Config) -> eyre::Result<()> {
        match self.command {
            Commands::Up(up) => up.run(docker, config).await,
            Commands::Exec(exec) => exec.run(docker, config).await,
            Commands::Fwd(fwd) => fwd.run(docker, config).await,
            Commands::List(list) => list.run(docker, config).await,
            Commands::Prune(prune) => prune.run(docker, config).await,
            Commands::Copy(copy) => copy.run(docker, config).await,
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
    #[command()]
    Copy(copy::Copy),
}
