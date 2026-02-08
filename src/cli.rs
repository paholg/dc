use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

mod up;

const ABOUT: &str = "TODO";

#[derive(Debug, Parser)]
#[command(version, about = ABOUT)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    #[command(visible_alias = "u")]
    Up(up::Up),
    #[command(visible_alias = "d")]
    Down(Down),
    #[command(visible_alias = "c")]
    Clean(Clean),
    #[command(visible_alias = "x")]
    Exec(Exec),
    #[command(visible_alias = "l")]
    List(List),
}

#[derive(Debug, Args)]
pub struct Down {}

#[derive(Debug, Args)]
pub struct Clean {}

/// Exec into a running devcontainer
///
/// Supply either path or name, or leave both blank to get a picker.
#[derive(Debug, Args)]
#[command(verbatim_doc_comment)]
pub struct Exec {
    #[arg(short, long, help = "path to workspace for devcontainer")]
    workspace: Option<PathBuf>,

    #[arg(short, long, help = "name of devcontainer")]
    name: Option<String>,

    #[arg(
        num_args = 0..,
        allow_hyphen_values = true,
        help = "run the given command, or leave blank to run your default shell"
    )]
    cmd: Option<Vec<String>>,
}

/// List active devcontainers
#[derive(Debug, Args)]
pub struct List {}
