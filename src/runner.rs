use std::borrow::Cow;
use std::path::Path;

use crate::ansi::{BLUE, CYAN, GREEN, MAGENTA, RED, RESET, YELLOW};

use crossterm::style::SetForegroundColor;
use tracing::Instrument;
use tracing::info_span;
use tracing_indicatif::span_ext::IndicatifSpanExt;

pub mod cmd;
pub mod docker_exec;
mod pty;

const LABEL_COLORS: &[SetForegroundColor] = &[CYAN, GREEN, YELLOW, BLUE, RED];

pub trait Runnable: Sync {
    fn command(&self) -> Cow<'_, str>;
    fn run(&self, dir: Option<&Path>) -> impl std::future::Future<Output = eyre::Result<()>> + Send;
}

pub async fn run(label: &str, runnable: &impl Runnable, dir: Option<&Path>) -> eyre::Result<()> {
    let command = runnable.command();
    let span = info_span!(
        "run",
        label,
        ?command,
        indicatif.pb_show = true,
        message = format_args!("{BLUE}Running{RESET}: {command}")
    );
    span.pb_set_message(&format!(
        "[{MAGENTA}{label}{RESET}] {BLUE}Running{RESET}: {command}"
    ));

    runnable.run(dir).instrument(span).await
}

pub async fn run_parallel<'a, I, R>(cmds: I) -> eyre::Result<()>
where
    I: IntoIterator<Item = (&'a str, &'a R)>,
    R: Runnable + 'a,
{
    let futures: Vec<_> = cmds
        .into_iter()
        .enumerate()
        .map(|(i, (label, cmd))| {
            let color = LABEL_COLORS[i % LABEL_COLORS.len()];
            let colored_label = format!("{color}{label}{RESET}");
            let command = cmd.command();
            let span = info_span!(
                "parallel",
                label = colored_label,
                indicatif.pb_show = true,
                message = format_args!("{BLUE}Running{RESET}: {command}")
            );
            {
                span.pb_set_message(&format!("{BLUE}Running{RESET}: {label}: {command}"));
                cmd.run(None).instrument(span)
            }
        })
        .collect();

    let results = futures::future::join_all(futures).await;
    match results.into_iter().find_map(|r| r.err()) {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
