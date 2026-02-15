use std::ffi::OsStr;

use clap::{CommandFactory, Parser};
use clap_complete::engine::CompletionCandidate;

use crate::cli::{Cli, Commands};
use crate::config::Config;
use crate::worktree;

fn is_completion_candidate(prefix: &str, candidate: &str) -> bool {
    candidate.starts_with(prefix) && candidate != prefix
}

pub fn complete_project(current: &OsStr) -> Vec<CompletionCandidate> {
    let prefix = current.to_string_lossy();
    let Ok(config) = Config::load() else {
        return vec![];
    };

    config
        .projects
        .keys()
        .filter(|name| is_completion_candidate(&prefix, name))
        .map(CompletionCandidate::new)
        .collect()
}

pub fn complete_workspace(current: &OsStr) -> Vec<CompletionCandidate> {
    complete_workspace_inner(current).unwrap()
}

fn complete_workspace_inner(current: &OsStr) -> eyre::Result<Vec<CompletionCandidate>> {
    let prefix = current.to_string_lossy();
    let config = Config::load()?;
    let (_, project) = config.project(parse_project_arg())?;

    let workspaces = worktree::list_sync(&project.path)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|path| path.file_name().map(|n| n.to_string_lossy().into_owned()))
        .filter(|name| is_completion_candidate(&prefix, name))
        .map(CompletionCandidate::new)
        .collect();

    Ok(workspaces)
}

fn parse_project_arg() -> Option<String> {
    // When completing, the actual args to dc are all after `--`.
    let args = std::env::args().skip_while(|arg| arg != "--").skip(1);
    Cli::command()
        .ignore_errors(true)
        .get_matches_from(args)
        .get_one::<String>("project")
        .map(ToOwned::to_owned)
}

/// Forward completions to `docker __completeNoDesc compose ...` at runtime.
///
/// Extracts the already-typed compose args from the completion command line
/// then delegates to docker's cobra-based completer.
pub fn complete_compose(_current: &OsStr) -> Vec<CompletionCandidate> {
    // NOTE: We ignore current as we already get it when parsing the args.
    complete_compose_inner().unwrap_or_default()
}

fn complete_compose_inner() -> eyre::Result<Vec<CompletionCandidate>> {
    let prior = compose_prior_args()?;

    // docker compose uses cobra, which provides a method to get its completions:
    // https://github.com/spf13/cobra/blob/main/completions.go
    let args = ["__complete".into(), "compose".into()]
        .into_iter()
        .chain(prior);

    let output = std::process::Command::new("docker").args(args).output()?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let result = stdout
        .lines()
        .take_while(|line| !line.starts_with(':'))
        .map(|line| {
            let (value, help) = line.split_once('\t').unwrap_or((line, ""));
            CompletionCandidate::new(value.to_owned()).help(Some(help.to_owned().into()))
        })
        .collect();
    Ok(result)
}

/// Use clap to parse the completion command line, then extract the trailing
/// compose args (minus the current word, which is passed separately).
fn compose_prior_args() -> eyre::Result<Vec<String>> {
    // When completing, the actual args to dc are all after `--`.
    let args = std::env::args().skip_while(|arg| arg != "--").skip(1);
    let cli = Cli::try_parse_from(args)?;
    let Commands::Compose(compose) = cli.command else {
        eyre::bail!("");
    };

    Ok(compose.args)
}
