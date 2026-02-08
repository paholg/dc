use std::path::PathBuf;

use clap::Args;

/// Spin up a devcontainer
#[derive(Debug, Args)]
pub struct Up {
    #[arg(
        short,
        long,
        help = "name of project; leave blank to use your default one"
    )]
    project: Option<String>,

    #[arg(
        short,
        long,
        help = "name of new workspace, leave blank for it to be generated"
    )]
    name: Option<PathBuf>,

    #[arg(
        short = 'x',
        long,
        num_args = 0..,
        allow_hyphen_values = true,
        help = "exec into it once up with the given command, or leave blank to run your default shell"
    )]
    exec: Option<Vec<String>>,
}

impl Up {
    pub fn run(self) -> eyre::Result<()> {
        Ok(())
    }
}
