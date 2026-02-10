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

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // Ensure help/version/error output goes to stderr, not stdout
            eprintln!("{}", e.render().ansi());
            std::process::exit(e.exit_code());
        }
    };
    let docker = dc::preflight::check().await?;
    let config = Config::load()?;
    cli.run(&docker, &config).await
}
