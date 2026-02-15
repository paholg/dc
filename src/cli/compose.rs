use std::os::unix::process::CommandExt;

use clap::Args;
use clap_complete::engine::ArgValueCompleter;

use crate::cli::State;
use crate::cli::up::compose_base_args;
use crate::complete;

/// Run `docker compose` against the given workspace
#[derive(Debug, Args)]
pub struct Compose {
    /// Arguments to provide to `docker compose`
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, add = ArgValueCompleter::new(complete::complete_compose))]
    pub args: Vec<String>,
}

impl Compose {
    pub async fn run(self, state: State) -> eyre::Result<()> {
        let name = state.resolve_workspace().await?;
        let dc = state.devcontainer()?;
        let crate::devcontainer::Kind::Compose(ref compose) = dc.kind else {
            unimplemented!();
        };

        let worktree_path = if state.is_root(&name) {
            state.project.path.clone()
        } else {
            let dc_options = &dc.common.customizations.dc;
            dc_options.workspace_dir(&state.project.path).join(&name)
        };

        let mut args = compose_base_args(compose, &worktree_path, None);
        args.extend(self.args);

        Err(std::process::Command::new("docker")
            .args(&args)
            .exec()
            .into())
    }
}
