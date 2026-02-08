use clap::Args;

use crate::{config::Config, workspace::Workspace};

/// List active devcontainers
#[derive(Debug, Args)]
pub struct List {
    #[arg(
        short,
        long,
        help = "name of project [default: The first one configured]"
    )]
    project: Option<Option<String>>,
}

impl List {
    pub async fn run(self, config: &Config) -> eyre::Result<()> {
        dbg!(Workspace::list_all(config).await?);
        Ok(())
    }
}
