#![forbid(unsafe_code)]

use clap::Parser;
use color_eyre::config::HookBuilder;
use dc::{self, cli::Cli, config::Config};

#[tokio::main(flavor = "current_thread")]
async fn main() -> eyre::Result<()> {
    HookBuilder::default()
        .display_env_section(false)
        .install()?;

    dc::subscriber::init_subscriber();

    let cli = Cli::parse();
    dc::preflight::check().await?;
    let config = Config::load()?;
    cli.run(&config).await
}
