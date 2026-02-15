use clap::Args;

use crate::{
    cli::State,
    workspace::{Workspace, table::workspace_table},
};

/// List all workspaces for the project
#[derive(Debug, Args)]
pub struct List;

impl List {
    pub async fn run(self, state: State) -> eyre::Result<()> {
        let workspaces = Workspace::list(&state).await?;
        eprint!("{}", workspace_table(&workspaces));
        Ok(())
    }
}
