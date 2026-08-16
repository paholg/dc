use std::io::Write;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use indicatif::ProgressStyle;
use jiff::fmt::friendly::SpanPrinter;
use jiff::{Unit, Zoned};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Record};
use tracing::{Event, Id, Subscriber};
use tracing_indicatif::IndicatifLayer;
use tracing_indicatif::filter::IndicatifFilter;
use tracing_indicatif::writer::{IndicatifWriter, Stderr, Stdout};
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

use crate::ansi::{BLUE, GRAY, GREEN, MAGENTA, RED, RESET, YELLOW};

fn ts(time: &Zoned) -> String {
    time.strftime("%F %T").to_string()
}

/// The writer everything must go through: it suspends the progress bars for the
/// duration of a write, so their next redraw can't clobber it — and, once the
/// last span closes, so they aren't left stranded in the scrollback.
static STDERR: OnceLock<IndicatifWriter<Stderr>> = OnceLock::new();

/// A writer that plays nicely with the spinners, if the subscriber is up.
pub(crate) fn stderr() -> Option<IndicatifWriter<Stderr>> {
    STDERR.get().cloned()
}

pub(crate) fn init_subscriber() {
    let indicatif_layer = IndicatifLayer::new().with_progress_style(
        ProgressStyle::with_template("{span_child_prefix}{spinner} {elapsed} {msg}")
            .expect("invalid progress style template"),
    );
    let stderr_writer = indicatif_layer.get_stderr_writer();
    let stdout_writer = indicatif_layer.get_stdout_writer();
    let _ = STDERR.set(stderr_writer.clone());
    let indicatif_layer = indicatif_layer.with_filter(IndicatifFilter::new(false));

    let dc_layer = DcLayer {
        stderr_writer,
        stdout_writer,
    }
    .with_filter(filter_fn(|meta| {
        // Filter out verbose (TRACE) output from dependencies.
        *meta.level() < tracing::Level::DEBUG || meta.target().starts_with("devconcurrent")
    }));

    tracing_subscriber::registry()
        .with(dc_layer)
        .with(indicatif_layer)
        .init();
}

struct HasIndicatif;
struct IndicatifName(String);

struct SpanTiming {
    name: Option<String>,
    description: Option<String>,
    message: Option<String>,
    finish_message: Option<String>,
    start: Zoned,
    entered: AtomicBool,
    /// Set through `run::mark_failed` when the span's work returned an error.
    failed: AtomicBool,
}

struct DcLayer {
    stderr_writer: IndicatifWriter<Stderr>,
    stdout_writer: IndicatifWriter<Stdout>,
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for DcLayer {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };

        let mut visitor = Visitor::default();
        attrs.record(&mut visitor);

        if visitor.indicatif_show {
            span.extensions_mut().insert(HasIndicatif);
        }

        if span.name() == "parallel"
            && let Some(ref name) = visitor.name
        {
            span.extensions_mut().insert(IndicatifName(name.clone()));
        }

        span.extensions_mut().insert(SpanTiming {
            name: visitor.name,
            description: visitor.description,
            message: visitor.message,
            finish_message: visitor.finish_message,
            start: Zoned::now(),
            entered: AtomicBool::new(false),
            failed: AtomicBool::new(visitor.failed),
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };

        let mut visitor = Visitor::default();
        values.record(&mut visitor);

        if visitor.failed
            && let Some(timing) = span.extensions().get::<SpanTiming>()
        {
            timing.failed.store(true, Ordering::Relaxed);
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let extensions = span.extensions();
        let Some(timing) = extensions.get::<SpanTiming>() else {
            return;
        };

        if timing.entered.swap(true, Ordering::Relaxed) {
            return;
        }

        let ts = ts(&Zoned::now());
        let mut line = format!("{GRAY}{ts}{RESET}");
        if let Some(ref name) = timing.name {
            line.push_str(&format!(" [{name}]"));
        }
        if let Some(ref message) = timing.message {
            line.push_str(&format!(" {message}:"));
        }
        if let Some(ref description) = timing.description {
            line.push_str(&format!(" {description}"));
        }
        let mut stderr = self.stderr_writer.clone();
        let _ = writeln!(stderr, "{line}");
        let _ = stderr.flush();
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let extensions = span.extensions();
        let Some(timing) = extensions.get::<SpanTiming>() else {
            return;
        };

        let now = Zoned::now();
        let ts = ts(&now);
        let mut line = format!("{GRAY}{ts}{RESET}");
        if let Some(ref name) = timing.name {
            line.push_str(&format!(" [{name}]"));
        }

        let dur = timing
            .start
            .duration_until(&now)
            .round(Unit::Millisecond)
            .unwrap();
        let dur = SpanPrinter::new().duration_to_string(&dur);
        if timing.failed.load(Ordering::Relaxed) {
            // The finish message would claim success, which is exactly what the
            // reader must not believe here.
            line.push_str(&format!(" {RED}Failed{RESET} after {RED}{dur}{RESET}"));
        } else {
            if let Some(msg) = &timing.finish_message {
                line.push(' ');
                line.push_str(msg);
            }
            line.push_str(&format!(" Took {GREEN}{dur}{RESET}"));
        }
        let mut stderr = self.stderr_writer.clone();
        let _ = writeln!(stderr, "{line}");
        let _ = stderr.flush();
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut visitor = Visitor::default();
        event.record(&mut visitor);
        let msg = visitor.message.unwrap_or_default();

        // Find indicatif name from ancestor spans
        let name = ctx.event_span(event).and_then(|span| {
            span.scope()
                .find_map(|s| s.extensions().get::<IndicatifName>().map(|n| n.0.clone()))
        });

        let level = *event.metadata().level();

        // We use TRACE logs as just forwarding output, and want to print them _almost_ undecorated.
        // The caveat is tha when they're run as part of parallel commands, they'll be interleaved,
        // so we want to show the source.
        if level == tracing::Level::TRACE {
            let line = match &name {
                Some(name) => format!("[{name}] {msg}"),
                None => msg,
            };
            // Keep a command's output on the stream it wrote to, so redirecting
            // ours separates what the command said from what we did.
            if visitor.stdout {
                let mut stdout = self.stdout_writer.clone();
                let _ = writeln!(stdout, "{line}");
                let _ = stdout.flush();
            } else {
                let mut stderr = self.stderr_writer.clone();
                let _ = writeln!(stderr, "{line}");
                let _ = stderr.flush();
            }
            return;
        }

        let ts = ts(&Zoned::now());
        let level_color = match level {
            tracing::Level::ERROR => RED,
            tracing::Level::WARN => YELLOW,
            tracing::Level::INFO => GREEN,
            tracing::Level::DEBUG => BLUE,
            tracing::Level::TRACE => unreachable!(),
        };

        let mut line = format!("{GRAY}{ts}{RESET} {level_color}{level:>5}{RESET}");
        if let Some(name) = &name {
            line.push_str(&format!(" [{MAGENTA}{name}{RESET}]"));
        }
        line.push_str(&format!(" {msg}"));

        let mut stderr = self.stderr_writer.clone();
        let _ = writeln!(stderr, "{line}");
        let _ = stderr.flush();
    }
}

// -- Visitor -----------------------------------------------------------------

#[derive(Default)]
struct Visitor {
    name: Option<String>,
    description: Option<String>,
    message: Option<String>,
    finish_message: Option<String>,
    indicatif_show: bool,
    failed: bool,
    /// Set on forwarded output that the child wrote to its stdout.
    stdout: bool,
}

impl Visit for Visitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "name" => self.name = Some(format!("{value:?}")),
            "description" => self.description = Some(format!("{value:?}")),
            "message" => self.message = Some(format!("{value:?}")),
            "finish_message" => self.finish_message = Some(format!("{value:?}")),
            _ => {}
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        match field.name() {
            "indicatif.pb_show" => self.indicatif_show = value,
            "failed" => self.failed = value,
            "stdout" => self.stdout = value,
            _ => {}
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "name" => self.name = Some(value.to_string()),
            "description" => self.description = Some(value.to_string()),
            "message" => self.message = Some(value.to_string()),
            "finish_message" => self.finish_message = Some(value.to_string()),
            _ => {}
        }
    }
}
