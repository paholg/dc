use std::borrow::Cow;

use crate::ansi::{BLUE, CYAN, GREEN, MAGENTA, RED, RESET, YELLOW};

use crossterm::style::SetForegroundColor;
use tracing::info_span;
use tracing_indicatif::span_ext::IndicatifSpanExt;

pub mod cmd;

const LABEL_COLORS: &[SetForegroundColor] = &[CYAN, GREEN, YELLOW, BLUE, RED];

pub trait Runnable: Sync {
    fn command(&self) -> Cow<'_, str>;
    fn run(&self) -> eyre::Result<()>;
}

pub fn run(label: &str, runnable: &impl Runnable) -> eyre::Result<()> {
    let command = runnable.command();
    let span = info_span!(
        "run",
        label,
        ?command,
        indicatif.pb_show = true,
        message = format_args!("Running {command}...")
    );
    let _guard = span.enter();
    span.pb_set_message(&format!("[{MAGENTA}{label}{RESET}] Running {command}..."));

    runnable.run()
}

pub fn run_parallel<'a, I, R>(cmds: I) -> eyre::Result<()>
where
    I: IntoIterator<Item = (&'a str, &'a R)>,
    R: Runnable + 'a,
{
    std::thread::scope(|s| {
        let handles: Vec<_> = cmds
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
                    message = format_args!("Running {command}")
                );
                s.spawn(move || {
                    span.in_scope(|| {
                        span.pb_set_message(&format!("Running {label}: {command}..."));
                        cmd.run()
                    })
                })
            })
            .collect();

        let mut first_err = None;
        for handle in handles {
            if let Err(e) = handle.join().unwrap()
                && first_err.is_none() {
                    first_err = Some(e);
                }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    })
}
