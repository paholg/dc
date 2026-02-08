#![forbid(unsafe_code)]

use clap::Parser;
use color_eyre::config::HookBuilder;
use dc::{
    self,
    cli::{Cli, Commands},
    config::Config,
};

fn main() -> eyre::Result<()> {
    HookBuilder::default()
        .display_env_section(false)
        .install()?;

    dc::subscriber::init_subscriber();

    let cli = Cli::parse();
    dc::preflight::check()?;
    let config = Config::load()?;
    match cli.command {
        Commands::Up(up) => up.run(&config)?,
        _ => todo!("{cli:?}"),
    }
    Ok(())
}
