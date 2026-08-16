use std::io::{BufRead, Write};

use clap::{Parser, Subcommand};
use clap_complete::engine::ArgValueCompleter;

use crate::{complete, state::State, workspace::Workspace};

mod compose;
mod destroy;
mod exec;
pub(crate) mod fwd;
mod gen_docs;
mod go;
pub(crate) mod proxy;
mod show;
mod status;
mod up;

const ABOUT: &str =
    "A tool for managing devcontainers, especially when combined with git worktrees";

#[derive(Debug, Parser)]
#[command(version, about = ABOUT)]
pub(crate) struct Cli {
    #[arg(
        short,
        long,
        help = "name of project [default: The DC_PROJECT variable, then the first configured project]",
        add = ArgValueCompleter::new(complete::complete_project),
    )]
    pub(crate) project: Option<String>,

    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Commands {
    #[command()]
    Up(up::Up),
    #[command(visible_alias = "x")]
    Exec(exec::Exec),
    #[command(visible_alias = "f")]
    Fwd(fwd::Fwd),
    #[command(visible_alias = "c")]
    Compose(compose::Compose),
    #[command()]
    Destroy(destroy::Destroy),
    Show(show::Show),
    #[command(visible_alias = "s")]
    Status(status::Status),
    #[command()]
    Go(go::Go),
    Proxy(proxy::Proxy),
    #[command(hide = true)]
    GenDocs(gen_docs::GenDocs),
}

/// Check that the workspace is safe to tear down (clean git).
pub(crate) async fn safety_check(workspace: &Workspace<'_>, force: bool) -> eyre::Result<()> {
    if force {
        return Ok(());
    }

    if workspace.is_dirty().await? {
        eyre::bail!(
            "workspace '{}' has uncommitted changes (use --force to override)",
            workspace.name
        );
    }

    Ok(())
}

pub(crate) fn confirm() -> eyre::Result<bool> {
    eprint!("Proceed? [y/N] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().eq_ignore_ascii_case("y"))
}

impl Cli {
    pub(crate) async fn run(self) -> eyre::Result<()> {
        match self.command {
            Commands::Up(up) => up.run(self.project).await,
            Commands::Exec(exec) => exec.run(self.project).await,
            Commands::Fwd(fwd) => fwd.run(self.project).await,
            Commands::Compose(compose) => compose.run(self.project).await,
            Commands::Show(show) => show.run(self.project).await,
            Commands::Status(status) => status.run(self.project).await,
            Commands::Destroy(destroy) => destroy.run(self.project).await,
            Commands::Go(go) => go.run(self.project).await,
            Commands::Proxy(proxy) => proxy.run(self.project).await,
            Commands::GenDocs(gen_docs) => gen_docs.run(),
        }
    }
}
