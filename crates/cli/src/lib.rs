#![forbid(unsafe_code)]

use clap::{CommandFactory, Parser};
use clap_complete::env::Shells;
use clap_complete::{CompleteEnv, Shell};
use color_eyre::config::HookBuilder;
use eyre::eyre;

use crate::cli::Cli;
use crate::subscriber::init_subscriber;

mod ansi;
mod bytes;
mod cli;
mod complete;
pub mod config;
pub mod devcontainer;
mod docker;
mod helpers;
pub mod run;
mod state;
mod subscriber;
mod table;
mod workspace;
mod worktree;

pub async fn cli_main() -> eyre::Result<()> {
    HookBuilder::default()
        .display_env_section(false)
        .install()?;
    init_subscriber();

    let shell_str = std::env::var("COMPLETE").ok();

    let completer = CompleteEnv::with_factory(Cli::command);
    let args = std::env::args_os();
    let current_dir = std::env::current_dir().ok();
    let completion = completer
        .try_complete(args, current_dir.as_deref())
        .unwrap_or_else(|e| e.exit());

    if completion {
        // When completion is triggered with no arguments, we're running the
        // initial shell registration.
        if std::env::args_os().len() == 1 {
            // Inject our `dc` wrapper function and register completions for the
            // `dc` alias too.
            if let Some(ref shell_str) = shell_str
                && let Err(e) = register_shell_function(shell_str)
            {
                tracing::warn!("Failed to generate shell wrapper: {e}");
            }
        }

        return Ok(());
    }

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // Ensure help/version/error output goes to stderr, not stdout
            eprintln!("{}", e.render().ansi());
            std::process::exit(e.exit_code());
        }
    };
    cli.run().await
}

fn register_shell_function(shell_str: &str) -> eyre::Result<()> {
    let shell = shell_str.parse::<Shell>().map_err(|e| eyre!("{e}"))?;
    let function = shell_function(shell)?;
    println!("{function}");

    let shells = Shells::builtins();
    let Some(completer) = shells.completer(shell_str) else {
        eyre::bail!("unsupported shell {shell_str}");
    };

    // Now, register completions for the `dc` wrapper function too.
    completer.write_registration("COMPLETE", "dc", "dc", "dc", &mut std::io::stdout())?;

    Ok(())
}

fn shell_function(shell: Shell) -> eyre::Result<String> {
    let bin_os = std::env::args_os()
        .next()
        .unwrap_or_else(|| "devconcurrent".into());
    let bin = bin_os.to_string_lossy();

    // A missing or broken config must not cost you the `dc` function at shell
    // startup; the opt-in hook just stays off, and the next real command will
    // report the problem.
    let export_env = crate::config::Config::load().is_ok_and(|config| config.shell.export_env);

    let func = complete::shell_function(shell, &bin, export_env)?;
    Ok(func)
}

/// Produce the JSON schema for [`config::Config`].
#[must_use]
pub fn schema() -> schemars::Schema {
    schemars::schema_for!(config::Config)
}
