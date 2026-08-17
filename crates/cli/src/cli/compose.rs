use std::os::unix::process::CommandExt;

use clap::Args;
use clap_complete::engine::ArgValueCompleter;

use crate::cli::State;
use crate::complete::{self, complete_workspace};
use crate::config::Config;
use crate::docker::compose::compose_cmd;

/// Run `docker compose` against the given workspace
#[derive(Debug, Args)]
pub(crate) struct Compose {
    /// Workspace name [default: current working directory]
    #[arg(short, long, add = ArgValueCompleter::new(complete_workspace))]
    workspace: Option<String>,

    /// Arguments to provide to `docker compose`
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, add = ArgValueCompleter::new(complete::complete_compose))]
    pub(crate) args: Vec<String>,
}

impl Compose {
    pub(crate) async fn run(self, project: Option<String>) -> eyre::Result<()> {
        let config = Config::load()?;
        let state = State::new(project, &config).await?;
        let workspace = state.resolve_workspace(self.workspace).await?;
        let devcontainer = state.devcontainer_for(&workspace.path)?;

        let mut cmd = compose_cmd(&devcontainer, &workspace).await?;
        cmd.args(&self.args);

        Err(cmd.into_std().exec().into())
    }
}
