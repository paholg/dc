use std::path::Path;

use clap::{Args, Subcommand};
use itertools::Itertools;

use crate::cli::State;
use crate::cli::up::compose_project_name;

/// Show some value
#[derive(Debug, Args)]
pub struct Show {
    #[command(subcommand)]
    command: ShowCommands,
}

#[derive(Debug, Subcommand)]
enum ShowCommands {
    /// Show currently-forwarded ports for this workspace
    Ports(Ports),
    /// Print the current workspace name, or exit 1
    Workspace(ShowWorkspace),
}

#[derive(Debug, Args)]
struct Ports;

#[derive(Debug, Args)]
struct ShowWorkspace;

impl Show {
    pub async fn run(self, state: State) -> eyre::Result<()> {
        match self.command {
            ShowCommands::Ports(ports) => ports.run(state).await,
            ShowCommands::Workspace(ws) => ws.run(state).await,
        }
    }
}

impl Ports {
    async fn run(self, state: State) -> eyre::Result<()> {
        let name = state.resolve_workspace().await?;
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

impl ShowWorkspace {
    async fn run(self, state: State) -> eyre::Result<()> {
        match state.resolve_workspace().await {
            Ok(name) => {
                println!("{name}");
                Ok(())
            }
            Err(_) => std::process::exit(1),
        }
    }
}
