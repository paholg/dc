use std::path::Path;

use clap::{Args, Subcommand};
use clap_complete::engine::ArgValueCompleter;
use itertools::Itertools;

use crate::cli::State;
use crate::cli::up::compose_project_name;
use crate::complete;

#[derive(Debug, Args)]
pub struct Show {
    #[command(subcommand)]
    command: ShowCommands,
}

#[derive(Debug, Subcommand)]
enum ShowCommands {
    Ports(Ports),
}

/// Show currently-forwarded ports for a workspace.
#[derive(Debug, Args)]
struct Ports {
    /// name of workspace [default: current working directory]
    #[arg(add = ArgValueCompleter::new(complete::complete_workspace))]
    name: Option<String>,
}

impl Show {
    pub async fn run(self, state: State) -> eyre::Result<()> {
        match self.command {
            ShowCommands::Ports(ports) => ports.run(state).await,
        }
    }
}

impl Ports {
    async fn run(self, state: State) -> eyre::Result<()> {
        let name = state.resolve_name(self.name).await?;
        let cpn = compose_project_name(Path::new(&name));
        let ports = state
            .docker
            .workspace_forwarded_ports(&state.project_name, &cpn)
            .await?
            .into_iter()
            .join(",");
        println!("{ports}");
        Ok(())
    }
}
