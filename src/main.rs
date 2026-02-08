#![forbid(unsafe_code)]

use clap::Parser;
use color_eyre::config::HookBuilder;
use dc::{self, cli::Cli};

fn main() -> eyre::Result<()> {
    HookBuilder::default()
        .display_env_section(false)
        .install()?;

    dc::subscriber::init_subscriber();

    let cli = Cli::parse();
    println!("{cli:?}");
    Ok(())
}
