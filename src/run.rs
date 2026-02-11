use std::borrow::Cow;

use color_eyre::owo_colors::OwoColorize;
use crossterm::style::SetForegroundColor;
use eyre::WrapErr;
use tracing::{Instrument, Span, info_span};
use tracing_indicatif::span_ext::IndicatifSpanExt;

use crate::ansi::{BLUE, CYAN, GREEN, RESET, YELLOW};

pub mod cmd;
pub mod docker_exec;
pub mod pty;

/// A token required to call `Runnable::run`.
///
/// Can only be constructed by `Runner`. This is a simple tool to ensure we
/// wrap our `Runnable`s in `Runner` handling.
pub struct Token(());

const TOK: Token = Token(());
const LABEL_COLORS: &[SetForegroundColor] = &[YELLOW, GREEN, BLUE, CYAN];

pub trait Runnable: Sync {
    fn name(&self) -> Cow<'_, str>;
    fn description(&self) -> Cow<'_, str>;
    /// The entrypoint of a Runnable.
    ///
    /// Note: Because of the `Runner`s log-handling, all output should go exclusively through
    /// tracing.
    #[allow(async_fn_in_trait)]
    async fn run(self, token: Token) -> eyre::Result<()>;
}

/// A simple command runner to show a emit a tracing span and show a spinner for
/// a running command or for several concurrent commands.
pub struct Runner;

fn run_span(name: &str, description: &str) -> Span {
    let name = name.magenta().to_string();
    let message = "Running".blue().to_string();
    let span = info_span!("run", indicatif.pb_show = true, name, description, message);
    let pb_message = format!("[{name}] {message}");
    span.pb_set_message(&pb_message);
    span
}

impl Runner {
    pub async fn run<R: Runnable>(runnable: R) -> eyre::Result<()> {
        let span = run_span(&runnable.name(), &runnable.description());
        let ctx = runnable.name().into_owned();

        runnable.run(TOK).instrument(span).await.wrap_err(ctx)
    }

    pub async fn run_parallel<R, I>(name: &str, runnables: I) -> eyre::Result<()>
    where
        R: Runnable,
        I: IntoIterator<Item = R>,
    {
        let runnables = runnables.into_iter().collect::<Vec<_>>();
        let names = runnables.iter().map(|r| r.name()).collect::<Vec<_>>();
        let description = names.join(", ");
        let span = run_span(name, &description);
        let _enter = span.enter();
        let futures: Vec<_> = runnables
            .into_iter()
            .enumerate()
            .map(|(i, runnable)| {
                let color = LABEL_COLORS[i % LABEL_COLORS.len()];
                let name = runnable.name();
                let name = format!("{color}{name}{RESET}");
                let description: &str = &runnable.description();

                let message = "Running".blue().to_string();

                let span = info_span!(
                    "parallel",
                    indicatif.pb_show = true,
                    name,
                    description,
                    message,
                );
                let pb_message = format!("[{name}] {message}");
                span.pb_set_message(&pb_message);
                let ctx = runnable.name().into_owned();
                async move {
                    runnable.run(TOK).await.wrap_err(ctx)
                }.instrument(span)
            })
            .collect();

        futures::future::try_join_all(futures).await?;

        Ok(())
    }
}
