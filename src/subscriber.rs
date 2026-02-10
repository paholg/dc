use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use indicatif::ProgressStyle;
use jiff::fmt::friendly::SpanPrinter;
use jiff::{Unit, Zoned};
use tracing::field::{Field, Visit};
use tracing::span::Attributes;
use tracing::{Event, Id, Subscriber};
use tracing_indicatif::IndicatifLayer;
use tracing_indicatif::filter::IndicatifFilter;
use tracing_indicatif::writer::{IndicatifWriter, Stderr};
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
    let stderr_writer = indicatif_layer.get_stderr_writer();
    let indicatif_layer = indicatif_layer.with_filter(IndicatifFilter::new(false));

    let dc_layer = DcLayer { stderr_writer }.with_filter(filter_fn(|meta| {
        // Filter out verbose (TRACE) output from dependencies.
        *meta.level() < tracing::Level::TRACE || meta.target().starts_with("dc")
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
    start: Zoned,
    entered: AtomicBool,
}

struct DcLayer {
    stderr_writer: IndicatifWriter<Stderr>,
}

impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for DcLayer {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };

        let mut visitor = Visitor::default();
        attrs.record(&mut visitor);

        if visitor.indicatif_show {
            span.extensions_mut().insert(HasIndicatif);

            if let Some(ref name) = visitor.name {
                let has_indicatif_ancestor = span
                    .scope()
                    .skip(1)
                    .any(|s| s.extensions().get::<HasIndicatif>().is_some());

                if has_indicatif_ancestor {
                    span.extensions_mut().insert(IndicatifName(name.clone()));
                }
            }
        }

        span.extensions_mut().insert(SpanTiming {
            name: visitor.name,
            description: visitor.description,
            message: visitor.message,
            start: Zoned::now(),
            entered: AtomicBool::new(false),
        });
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
        line.push_str(&format!(" Took {GREEN}{dur}{RESET}"));
        let mut stderr = self.stderr_writer.clone();
        let _ = writeln!(stderr, "{line}");
        let _ = stderr.flush();
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut visitor = Visitor::default();
        event.record(&mut visitor);
        let msg = visitor.message.unwrap_or_default();

        // Find parallel name from ancestor spans
        let name = ctx.event_span(event).and_then(|span| {
            span.scope()
                .find_map(|s| s.extensions().get::<IndicatifName>().map(|n| n.0.clone()))
        });

        let level = *event.metadata().level();

        // We use TRACE logs as just forwarding output, and want to print them _almost_ undecorated.
        // The caveat is tha when they're run as part of parallel commands, they'll be interleaved,
        // so we want to show the source.
        if level == tracing::Level::TRACE {
            let mut stderr = self.stderr_writer.clone();
            if let Some(name) = &name {
                let _ = writeln!(stderr, "[{name}] {msg}");
            } else {
                let _ = writeln!(stderr, "{msg}");
            }
            let _ = stderr.flush();
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
    indicatif_show: bool,
}

impl Visit for Visitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "name" => self.name = Some(format!("{value:?}")),
            "description" => self.description = Some(format!("{value:?}")),
            "message" => self.message = Some(format!("{value:?}")),
            _ => {}
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        if field.name() == "indicatif.pb_show" {
            self.indicatif_show = value;
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "name" => self.name = Some(value.to_string()),
            "description" => self.description = Some(value.to_string()),
            "message" => self.message = Some(value.to_string()),
            _ => {}
        }
    }
}
