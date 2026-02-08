use std::io::Write;

use indicatif::ProgressStyle;
use jiff::fmt::friendly::SpanPrinter;
use jiff::{Unit, Zoned};
use tracing::field::{Field, Visit};
use tracing::span::Attributes;
use tracing::{Event, Id, Subscriber};
use tracing_indicatif::IndicatifLayer;
use tracing_indicatif::filter::IndicatifFilter;
use tracing_indicatif::writer::{IndicatifWriter, Stdout};
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

use crate::ansi::{BLUE, GRAY, GREEN, MAGENTA, RED, RESET, YELLOW};

fn ts(time: &Zoned) -> String {
    time.strftime("%F %T").to_string()
}

pub fn init_subscriber() {
    let indicatif_layer = IndicatifLayer::new().with_progress_style(
        ProgressStyle::with_template("{span_child_prefix}{spinner} {elapsed} {msg}")
            .expect("invalid progress style template"),
    );
    let stdout_writer = indicatif_layer.get_stdout_writer();
    let indicatif_layer = indicatif_layer.with_filter(IndicatifFilter::new(false));

    let dc_layer = DcLayer { stdout_writer }.with_filter(filter_fn(|meta| {
        *meta.level() > tracing::Level::TRACE || meta.target().starts_with("dc")
    }));

    tracing_subscriber::registry()
        .with(dc_layer)
        .with(indicatif_layer)
        .init();
}

struct ParallelLabel(String);

struct SpanTiming {
    label: Option<String>,
    message: Option<String>,
    start: Zoned,
}

struct DcLayer {
    stdout_writer: IndicatifWriter<Stdout>,
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for DcLayer {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };

        let mut visitor = Visitor::default();
        attrs.record(&mut visitor);

        if attrs.metadata().name() == "parallel"
            && let Some(ref label) = visitor.label
        {
            span.extensions_mut().insert(ParallelLabel(label.clone()));
        }

        span.extensions_mut().insert(SpanTiming {
            label: visitor.label,
            message: visitor.message,
            start: Zoned::now(),
        });
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let extensions = span.extensions();
        let Some(timing) = extensions.get::<SpanTiming>() else {
            return;
        };

        let ts = ts(&Zoned::now());
        let mut line = format!("{GRAY}{ts}{RESET}");
        if let Some(ref label) = timing.label {
            line.push_str(&format!(" [{MAGENTA}{label}{RESET}]"));
        }
        if let Some(ref message) = timing.message {
            line.push_str(&format!(" {message}"));
        }
        let mut stdout = self.stdout_writer.clone();
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
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
        if let Some(ref label) = timing.label {
            line.push_str(&format!(" [{MAGENTA}{label}{RESET}]"));
        }

        let dur = timing
            .start
            .duration_until(&now)
            .round(Unit::Millisecond)
            .unwrap();
        let dur = SpanPrinter::new().duration_to_string(&dur);

        line.push_str(&format!(" Took {GREEN}{dur}{RESET}"));
        let mut stdout = self.stdout_writer.clone();
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut visitor = Visitor::default();
        event.record(&mut visitor);
        let msg = visitor.message.unwrap_or_default();

        // Find parallel label from ancestor spans
        let label = ctx.event_span(event).and_then(|span| {
            span.scope()
                .find_map(|s| s.extensions().get::<ParallelLabel>().map(|l| l.0.clone()))
        });

        let level = *event.metadata().level();

        // We use TRACE logs as just forwarding output, and want to print them _almost_ undecorated.
        // The caveat is tha when they're run as part of parallel commands, they'll be interleaved,
        // so we want to show the source.
        if level == tracing::Level::TRACE {
            let mut stdout = self.stdout_writer.clone();
            if let Some(label) = &label {
                let _ = writeln!(stdout, "[{label}] {msg}");
            } else {
                let _ = writeln!(stdout, "{msg}");
            }
            let _ = stdout.flush();
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
        if let Some(label) = &label {
            line.push_str(&format!(" [{MAGENTA}{label}{RESET}]"));
        }
        line.push_str(&format!(" {msg}"));

        let mut stdout = self.stdout_writer.clone();
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
    }
}

// -- Visitor -----------------------------------------------------------------

#[derive(Default)]
struct Visitor {
    label: Option<String>,
    message: Option<String>,
}

impl Visit for Visitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "label" => self.label = Some(format!("{value:?}")),
            "message" => self.message = Some(format!("{value:?}")),
            _ => {}
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "label" => self.label = Some(value.to_string()),
            "message" => self.message = Some(value.to_string()),
            _ => {}
        }
    }
}
