use std::borrow::Cow;
use std::collections::VecDeque;
use std::process::ExitStatus;

use color_eyre::owo_colors::OwoColorize;
use color_eyre::{Section, SectionExt};
use crossterm::style::SetForegroundColor;
use eyre::WrapErr;
use itertools::Itertools;
use tracing::{Instrument, Span, field, info_span};
use tracing_indicatif::span_ext::IndicatifSpanExt;

use tokio::io::{AsyncBufReadExt, AsyncRead};

use crate::ansi::{BLUE, CYAN, GREEN, RESET, YELLOW};

pub(crate) mod cmd;
pub(crate) mod docker_exec;

/// A token required to call `Runnable::run`.
///
/// Can only be constructed by `Runner`. This is a simple tool to ensure we
/// wrap our `Runnable`s in `Runner` handling.
pub(crate) struct Token(());

const TOK: Token = Token(());
const LABEL_COLORS: &[SetForegroundColor] = &[YELLOW, GREEN, BLUE, CYAN];

pub(crate) trait Runnable: Sync {
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
pub(crate) struct Runner;

fn run_span(name: &str, description: &str) -> Span {
    let name = name.magenta().to_string();
    let message = "Running".blue().to_string();
    let span = info_span!(
        "run",
        indicatif.pb_show = true,
        name,
        description,
        message,
        failed = field::Empty,
    );
    let pb_message = format!("[{name}] {message}");
    span.pb_set_message(&pb_message);
    span
}

/// Tell the subscriber that this span's work failed, so it reports the failure
/// rather than a duration and a finish message that claim success.
pub(crate) fn mark_failed(span: &Span) {
    span.record("failed", true);
}

impl Runner {
    pub(crate) async fn run<R: Runnable>(runnable: R) -> eyre::Result<()> {
        let span = run_span(&runnable.name(), &runnable.description());
        let ctx = runnable.name().into_owned();

        let result = runnable
            .run(TOK)
            .instrument(span.clone())
            .await
            .wrap_err(ctx);
        if result.is_err() {
            mark_failed(&span);
        }
        result
    }

    pub(crate) async fn run_parallel<R, I>(name: &str, runnables: I) -> eyre::Result<()>
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
                    failed = field::Empty,
                );
                let pb_message = format!("[{name}] {message}");
                span.pb_set_message(&pb_message);
                let ctx = runnable.name().into_owned();
                async move {
                    // Left unwrapped: who failed is said once, either by the
                    // wrap below or by the section header.
                    let result = runnable.run(TOK).await;
                    if result.is_err() {
                        mark_failed(&Span::current());
                    }
                    (ctx, result)
                }
                .instrument(span)
            })
            .collect();

        // `join_all`, not `try_join_all`: the latter drops the other futures the
        // moment one fails, killing those commands mid-run and throwing away
        // whatever they were about to say about themselves.
        let results = futures::future::join_all(futures).await;
        let total = results.len();
        let mut failures: Vec<(String, eyre::Report)> = results
            .into_iter()
            .filter_map(|(name, result)| result.err().map(|err| (name, err)))
            .collect();

        if failures.is_empty() {
            return Ok(());
        }
        mark_failed(&span);

        let report = if failures.len() == 1 {
            let (name, err) = failures.pop().expect("just checked");
            err.wrap_err(name)
        } else {
            let names = failures.iter().map(|(name, _)| name.as_str()).join(", ");
            let mut report = eyre::eyre!("{} of {total} commands failed: {names}", failures.len());
            // Each failure keeps its own message and captured output, rather
            // than the first one silencing the rest.
            for (name, err) in failures {
                let rendered = format!("{err:?}");
                report = report.section(rendered.trim().to_owned().header(format!("{name}:")));
            }
            report
        };

        Err(report).wrap_err(name.to_owned())
    }
}

/// How many lines of a failed command's output we keep to show with its error.
const OUTPUT_TAIL_LINES: usize = 30;

/// The tail of one of a command's output streams.
///
/// Bounded, so that a chatty command can't turn its own error report into
/// another wall of text: the last lines are the ones that say why it failed.
struct Tail {
    lines: VecDeque<String>,
    dropped: usize,
}

impl Tail {
    fn new() -> Self {
        Self {
            lines: VecDeque::new(),
            dropped: 0,
        }
    }

    fn push(&mut self, line: String) {
        if self.lines.len() == OUTPUT_TAIL_LINES {
            self.lines.pop_front();
            self.dropped += 1;
        }
        self.lines.push_back(line);
    }

    fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Say how much of the stream this is, so nobody hunts for lines we dropped.
    fn header(&self, stream: &str) -> String {
        if self.dropped == 0 {
            format!("{stream}:")
        } else {
            let total = self.dropped + self.lines.len();
            format!("{stream} (last {} of {total} lines):", self.lines.len())
        }
    }

    fn body(&self) -> String {
        self.lines.iter().join("\n")
    }
}

/// Which of a child's streams a line came from, so that it lands on the same one
/// of ours.
#[derive(Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

/// Forward a stream of a child's output as it arrives, keeping the tail of it in
/// case the child fails.
async fn forward<R: AsyncRead + Unpin>(reader: R, stream: Stream) -> Tail {
    let mut reader = tokio::io::BufReader::new(reader);
    let mut tail = Tail::new();
    let mut buf = Vec::new();
    // Bytes rather than `lines()`: that stops at the first line that isn't UTF-8,
    // and dropping the reader mid-run leaves the child to die of SIGPIPE on its
    // next write, with nothing to say why.
    while let Ok(1..) = reader.read_until(b'\n', &mut buf).await {
        let line = String::from_utf8_lossy(trim_newline(&buf));
        // Two callsites, because a `tracing` callsite has a fixed field set.
        match stream {
            Stream::Stdout => tracing::trace!(stdout = true, "{line}"),
            Stream::Stderr => tracing::trace!("{line}"),
        }
        tail.push(line.into_owned());
        buf.clear();
    }
    tail
}

/// Drop the line terminator, `\r\n` or `\n`, as `AsyncBufReadExt::lines` does.
fn trim_newline(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// How a failed command is named in its error.
///
/// `what` describes the command as the caller wrote it rather than by its argv —
/// a `docker exec` argv is mostly the probed environment, hundreds of lines of
/// it.
fn exit_message(what: &str, status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("{what} exited with status {code}"),
        // With no code it was signalled, which `ExitStatus` renders itself,
        // e.g. "signal: 9 (SIGKILL)".
        None => format!("{what} {status}"),
    }
}

/// The `(header, body)` of each stream that produced anything, so that the
/// reason for a failure sits next to the failure rather than scrolled off above
/// the spinner.
fn output_sections(stdout: &Tail, stderr: &Tail) -> Vec<(String, String)> {
    [(stdout, "stdout"), (stderr, "stderr")]
        .into_iter()
        .filter(|(tail, _)| !tail.is_empty())
        .map(|(tail, stream)| (tail.header(stream), tail.body()))
        .collect()
}

/// Build the error for a command that ran but exited non-zero.
fn exit_error(what: &str, status: ExitStatus, stdout: &Tail, stderr: &Tail) -> eyre::Report {
    let mut report = eyre::eyre!("{}", exit_message(what, status));

    let sections = output_sections(stdout, stderr);
    if sections.is_empty() {
        return report.note("it produced no output");
    }
    for (header, body) in sections {
        report = report.section(body.header(header));
    }

    report
}

/// Run the given command, capturing all of its output and printing it ourselves, so it plays nicely
/// with our spinners.
///
/// `what` names the command for any error, e.g. ``"`./post_start.sh` in
/// container 483008c0c084"``.
pub(crate) async fn run_command(mut cmd: tokio::process::Command, what: &str) -> eyre::Result<()> {
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd
        .spawn()
        .wrap_err_with(|| format!("failed to start {what}"))?;

    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");

    let (status, stdout, stderr) = tokio::join!(
        child.wait(),
        forward(stdout, Stream::Stdout),
        forward(stderr, Stream::Stderr),
    );

    let status = status.wrap_err_with(|| format!("failed to wait for {what}"))?;
    if !status.success() {
        return Err(exit_error(what, status, &stdout, &stderr));
    }

    Ok(())
}

// TODO: Remove this
pub(crate) async fn run_cmd(
    argv: &[&str],
    dir: Option<&std::path::Path>,
    what: &str,
) -> eyre::Result<()> {
    let mut cmd = tokio::process::Command::new(argv[0]);
    cmd.args(&argv[1..]);
    if let Some(d) = dir {
        cmd.current_dir(d);
    }

    run_command(cmd, what).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sections themselves are opaque once attached to a `Report` — and only
    /// render through the `color_eyre` handler that the binary installs — so
    /// these test the message and sections before they are attached.
    fn sh(script: &str) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", script]);
        cmd
    }

    async fn tails(script: &str) -> (ExitStatus, Tail, Tail) {
        let mut cmd = sh(script);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        let mut child = cmd.spawn().expect("/bin/sh is there");

        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let (status, stdout, stderr) = tokio::join!(
            child.wait(),
            forward(stdout, Stream::Stdout),
            forward(stderr, Stream::Stderr),
        );

        (status.expect("the child is waited on once"), stdout, stderr)
    }

    #[tokio::test]
    async fn the_output_that_explains_the_failure_is_in_the_error() {
        let (status, stdout, stderr) =
            tails("echo hello; echo 'port 53: Address already in use' >&2; exit 2").await;

        assert_eq!(
            exit_message("`post_start.sh` in container abc123", status),
            "`post_start.sh` in container abc123 exited with status 2"
        );
        assert_eq!(
            output_sections(&stdout, &stderr),
            [
                ("stdout:".to_owned(), "hello".to_owned()),
                (
                    "stderr:".to_owned(),
                    "port 53: Address already in use".to_owned()
                ),
            ]
        );
    }

    /// Output that isn't UTF-8 must not cut the stream short: everything after
    /// the bad line still has to arrive. The bytes come straight from the test —
    /// shells differ on whether `printf` produces 0xff from an escape.
    #[tokio::test]
    async fn invalid_utf8_output_is_kept_lossily() {
        let stdout = forward(&b"a\xffb\nafter\n"[..], Stream::Stdout).await;

        assert_eq!(stdout.body(), "a\u{fffd}b\nafter");
    }

    /// A chatty command must not put its whole output back into the error.
    #[tokio::test]
    async fn only_the_tail_of_a_long_stream_is_kept() {
        let (_, stdout, stderr) = tails("seq 1 100 >&2; exit 1").await;
        let sections = output_sections(&stdout, &stderr);
        let [(header, body)] = sections.as_slice() else {
            panic!("only stderr has output: {sections:?}");
        };

        assert_eq!(
            header,
            &format!("stderr (last {OUTPUT_TAIL_LINES} of 100 lines):")
        );
        assert_eq!(body.lines().count(), OUTPUT_TAIL_LINES);
        assert!(body.ends_with("\n100"), "{body}");
        assert!(!body.starts_with('1'), "{body}");
    }

    #[tokio::test]
    async fn a_silent_failure_has_nothing_to_show() {
        let (status, stdout, stderr) = tails("exit 3").await;

        assert_eq!(
            exit_message("the script", status),
            "the script exited with status 3"
        );
        assert!(output_sections(&stdout, &stderr).is_empty());
    }

    /// A signalled command has no exit code; `unwrap_or(1)` used to report it as
    /// a plain failure, which hides the OOM killer.
    #[tokio::test]
    async fn a_signalled_command_names_the_signal() {
        let (status, _, _) = tails("kill -9 $$").await;
        let message = exit_message("the script", status);

        assert!(message.contains("SIGKILL"), "{message}");
    }

    #[tokio::test]
    async fn a_missing_program_says_what_could_not_start() {
        let err = run_cmd(&["definitely-not-a-real-program"], None, "the probe")
            .await
            .expect_err("no such program");
        let report = format!("{err:#}");

        assert!(report.contains("failed to start the probe"), "{report}");
    }

    /// One of the parallel commands in a lifecycle hook.
    struct Script {
        name: &'static str,
        script: String,
    }

    impl Runnable for Script {
        fn name(&self) -> Cow<'_, str> {
            self.name.into()
        }

        fn description(&self) -> Cow<'_, str> {
            (&self.script).into()
        }

        async fn run(self, _: Token) -> eyre::Result<()> {
            run_command(sh(&self.script), self.name).await
        }
    }

    fn script(name: &'static str, script: &str) -> Script {
        Script {
            name,
            script: script.to_owned(),
        }
    }

    #[tokio::test]
    async fn every_parallel_failure_is_reported() {
        let err = Runner::run_parallel(
            "postCreateCommand",
            [
                script("deps", "exit 1"),
                script("fine", "true"),
                script("migrate", "echo 'no such table' >&2; exit 2"),
            ],
        )
        .await
        .expect_err("two of them fail");
        let report = format!("{err:#}");

        assert!(report.contains("postCreateCommand"), "{report}");
        assert!(
            report.contains("2 of 3 commands failed: deps, migrate"),
            "{report}"
        );
    }

    /// With one failure there is nothing to summarize, so it is reported whole —
    /// captured output and all.
    #[tokio::test]
    async fn a_lone_parallel_failure_is_passed_through() {
        let err = Runner::run_parallel(
            "postCreateCommand",
            [script("deps", "exit 1"), script("fine", "true")],
        )
        .await
        .expect_err("one of them fails");
        let report = format!("{err:#}");

        assert!(
            report.contains("postCreateCommand: deps: deps exited with status 1"),
            "{report}"
        );
        assert!(!report.contains("commands failed"), "{report}");
    }

    /// A failure used to drop the other futures, killing those commands partway
    /// through whatever they were doing to the workspace.
    #[tokio::test]
    async fn a_failure_lets_its_siblings_finish() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let marker = dir.path().join("finished");

        let _ = Runner::run_parallel(
            "postCreateCommand",
            [
                script("fails-first", "exit 1"),
                script("slow", &format!("sleep 0.2; touch {}", marker.display())),
            ],
        )
        .await
        .expect_err("the first one fails");

        assert!(marker.exists(), "the slow command was cut short");
    }
}
