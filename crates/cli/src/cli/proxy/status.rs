//! `dc proxy status` — check that every configured hostname and port really is
//! reachable, and say which layer broke when one isn't.
//!
//! One report, filled in as the checks resolve: the state of the proxy itself
//! (including whether it's running settings the config has since moved past),
//! then a table per workspace with a column per layer. A table only ever shows
//! `✓`, `-` or `✗` — every `✗` is spelled out, with its fix, in a note under
//! the table it came from, so that one long explanation can't set the width of
//! a whole column.

use std::collections::HashMap;
use std::io::IsTerminal;
use std::sync::Arc;
use std::time::Duration;

use clap::Args;
use clap_complete::engine::ArgValueCompleter;
use crossterm::style::Stylize;
use eyre::Result;
use serde::Serialize;

use self::checks::{Check, Probe, RowChecks};
use self::endpoints::{Discovery, Endpoint, Sidecar};
use super::ProxyState;
use crate::ansi::{RED, RESET};
use crate::complete::complete_workspace;
use crate::table::{Align, ColumnDef, Datum, Gatherer, Report, Table, TableBuilder, text, value};

mod checks;
mod endpoints;

/// Long enough for every stage of the slowest row to land as a real result
/// rather than a timed-out `-`.
const DEADLINE: Duration = Duration::from_secs(25);

/// How long to wait between passes in `--live`.
const LIVE_PERIOD: Duration = Duration::from_secs(2);

#[derive(Debug, Args)]
pub(crate) struct StatusArgs {
    /// Workspace name (only useful if its devcontainer.json diverges from the root workspace)
    #[arg(short, long, add = ArgValueCompleter::new(complete_workspace))]
    workspace: Option<String>,

    /// Check every proxy-enabled project, not just this one
    #[arg(short, long)]
    all: bool,

    /// Show live, updating data
    #[arg(short, long, conflicts_with = "json")]
    live: bool,

    /// Print the results as JSON instead of a table
    #[arg(long)]
    json: bool,
}

impl StatusArgs {
    pub(crate) fn workspace(&self) -> Option<String> {
        self.workspace.clone()
    }
}

pub(super) async fn run(proxy: &ProxyState, args: &StatusArgs) -> Result<()> {
    let found = endpoints::discover(&proxy.docker, &proxy.options).await?;
    let (endpoints, sidecars) = scope(found, proxy, args.all);

    let strays = stray_sidecars(&sidecars, &endpoints);
    let probe = Arc::new(Probe::new(
        proxy.config.port,
        proxy.config.ca_root.as_deref(),
        sidecars,
    ));

    let proxy_checks = spawn_proxy_checks(proxy, strays, args.live);
    let rows: Vec<Row> = endpoints
        .into_iter()
        .map(|endpoint| Row {
            checks: spawn_row(probe.clone(), endpoint.clone(), args.live),
            endpoint,
        })
        .collect();

    if args.json {
        return emit_json(&proxy_checks, &rows).await;
    }

    let title = if args.all {
        "all projects".to_string()
    } else {
        proxy.project.to_string()
    };
    let groups = by_workspace(rows);
    let mut report = Report::new()
        .text(format!("PROJECT: {}", title.blue()))
        .text(String::new())
        .table(proxy_table(&proxy_checks, args.live))
        .lines(proxy_notes(&proxy_checks))
        .deadline(DEADLINE);

    if groups.is_empty() {
        report = report.text(String::new()).text(
            "No proxy-enabled services are running."
                .dark_grey()
                .to_string(),
        );
    }
    for group in &groups {
        // The project only needs saying when more than one is in play.
        let heading = if args.all {
            format!("{} / {}", group.project, group.workspace)
        } else {
            group.workspace.clone()
        };
        report = report
            .text(String::new())
            .text(format!("WORKSPACE: {}", heading.yellow()))
            .text(String::new())
            .table(endpoint_table(&group.rows, args.live))
            .lines(notes(&group.rows));
    }

    if std::io::stderr().is_terminal() {
        report.run_tty().await?;
    } else {
        report.run_piped().await?;
    }

    if failed(&proxy_checks, &groups) {
        // A diagnostic that found a problem still ran correctly, so this isn't
        // an error to report — just an exit code for whoever is scripting it.
        std::process::exit(1);
    }
    Ok(())
}

struct Row {
    endpoint: Arc<Endpoint>,
    checks: Gatherer<RowChecks>,
}

/// One table's worth of rows: everything the proxy serves for a single
/// workspace.
struct Group {
    project: String,
    workspace: String,
    rows: Vec<Row>,
}

/// Split into one group per workspace, ordered by name so the output doesn't
/// shuffle between runs, with each workspace's services in a stable order too.
fn by_workspace(rows: Vec<Row>) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for row in rows {
        let key = (&row.endpoint.project, &row.endpoint.workspace);
        match groups
            .iter_mut()
            .find(|g| (&g.project, &g.workspace) == key)
        {
            Some(group) => group.rows.push(row),
            None => groups.push(Group {
                project: row.endpoint.project.clone(),
                workspace: row.endpoint.workspace.clone(),
                rows: vec![row],
            }),
        }
    }

    groups.sort_by(|a, b| {
        a.project
            .cmp(&b.project)
            .then_with(|| a.workspace.cmp(&b.workspace))
    });
    for group in &mut groups {
        group
            .rows
            .sort_by(|a, b| a.endpoint.service.cmp(&b.endpoint.service));
    }
    groups
}

/// Narrow to the current project unless asked for everything. The proxy is
/// global, but you are usually only asking about what's in front of you.
fn scope(found: Discovery, proxy: &ProxyState, all: bool) -> (Vec<Arc<Endpoint>>, Vec<Sidecar>) {
    let Discovery {
        endpoints,
        sidecars,
    } = found;
    if all {
        return (endpoints, sidecars);
    }
    let current = proxy.project.as_str();
    (
        endpoints
            .into_iter()
            .filter(|e| e.project == current)
            .collect(),
        sidecars
            .into_iter()
            .filter(|s| s.key.0 == current)
            .collect(),
    )
}

/// Sidecars that belong to no endpoint we know about, or whose target is gone.
/// Left behind by a crash, or by a service that was renamed out of the config.
fn stray_sidecars(sidecars: &[Sidecar], endpoints: &[Arc<Endpoint>]) -> Vec<String> {
    let live: HashMap<(String, String, String), Option<String>> = endpoints
        .iter()
        .filter(|e| e.sidecar.is_some())
        .map(|e| (e.key(), e.container.as_ref().map(|t| t.id.clone())))
        .collect();

    sidecars
        .iter()
        .filter(|sidecar| match live.get(&sidecar.key) {
            None => true,
            Some(target) => target.as_deref() != sidecar.target.as_deref(),
        })
        .map(|sidecar| {
            let (_, workspace, service) = &sidecar.key;
            format!("{workspace}/{service}")
        })
        .collect()
}

// -- the proxy's own state ---------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
struct ProxyChecks {
    docker: Datum<Check>,
    container: Datum<Check>,
    image: Datum<Check>,
    config: Datum<Check>,
    dns: Datum<Check>,
    ca: Datum<Check>,
    trust: Datum<Check>,
    sidecars: Datum<Check>,
}

/// Reads one check out of the set; how a column knows which row it's on.
type PickProxy = fn(&ProxyChecks) -> &Datum<Check>;

/// The rows of the first table, in display order.
const PROXY_ROWS: [(&str, PickProxy); 8] = [
    ("docker", |c| &c.docker),
    ("container", |c| &c.container),
    ("image", |c| &c.image),
    ("config", |c| &c.config),
    ("dns", |c| &c.dns),
    ("ca", |c| &c.ca),
    ("trust", |c| &c.trust),
    ("sidecars", |c| &c.sidecars),
];

impl ProxyChecks {
    fn failed(&self) -> bool {
        PROXY_ROWS
            .iter()
            .any(|(_, pick)| matches!(pick(self), Datum::Value(c) if c.failed()))
    }

    /// Each failed check, as its name and what it said.
    fn failures(&self) -> Vec<(&'static str, String)> {
        PROXY_ROWS
            .iter()
            .filter_map(|(name, pick)| match pick(self) {
                Datum::Value(check) if check.failed() => Some((
                    *name,
                    check.detail.clone().unwrap_or_else(|| "failed".to_string()),
                )),
                _ => None,
            })
            .collect()
    }
}

fn spawn_proxy_checks(
    proxy: &ProxyState,
    strays: Vec<String>,
    live: bool,
) -> Gatherer<ProxyChecks> {
    let docker = proxy.docker.clone();
    let port = proxy.config.port;
    let ca_root = proxy.config.ca_root.clone();
    let expected_hash = proxy.config_hash();
    // Every proxied service is served over https, so any one of them is reason
    // enough to check the CA.
    let wants_tls = proxy
        .options
        .values()
        .flat_map(|o| o.services.values())
        .any(|s| s.container_port.is_some());

    Gatherer::progressive(move |mut out| async move {
        loop {
            checks::run_proxy(
                &docker,
                port,
                ca_root.as_deref(),
                &expected_hash,
                wants_tls,
                &strays,
                &mut out,
            )
            .await;
            if !live {
                return;
            }
            tokio::time::sleep(LIVE_PERIOD).await;
        }
    })
}

fn proxy_table(source: &Gatherer<ProxyChecks>, live: bool) -> Table {
    let status = source.clone();
    let detail = source.clone();
    let columns = [
        ColumnDef::new("CHECK", Align::Left, |r: &(&str, PickProxy)| text(r.0)),
        ColumnDef::new("STATUS", Align::Left, move |r: &(&str, PickProxy)| {
            let pick = r.1;
            value(status.cell(move |c: &ProxyChecks| pick(c).clone()))
        }),
        ColumnDef::new("DETAIL", Align::Left, move |r: &(&str, PickProxy)| {
            let pick = r.1;
            value(detail.cell(move |c: &ProxyChecks| match pick(c) {
                Datum::Pending => Datum::Pending,
                Datum::NotApplicable => Datum::NotApplicable,
                // A failure's explanation goes in a note below the table
                // instead: they run long, and one of them would set the width
                // of the whole column.
                Datum::Value(check) if check.failed() => Datum::Value(String::new()),
                Datum::Value(check) => Datum::Value(check.detail.clone().unwrap_or_default()),
            }))
        }),
    ];

    columns
        .into_iter()
        .collect::<TableBuilder<(&str, PickProxy)>>()
        .build(&PROXY_ROWS, live)
}

/// The notes under the proxy table, on the same terms as a workspace's.
fn proxy_notes(source: &Gatherer<ProxyChecks>) -> impl Fn(u16) -> Vec<String> + use<> {
    let source = source.clone();
    move |width| {
        let mut lines = Vec::new();
        for (name, detail) in source.snapshot().failures() {
            lines.extend(note(name, &detail, width));
        }
        if !lines.is_empty() {
            lines.insert(0, String::new());
        }
        lines
    }
}

// -- one row per service -----------------------------------------------------

fn spawn_row(probe: Arc<Probe>, endpoint: Arc<Endpoint>, live: bool) -> Gatherer<RowChecks> {
    Gatherer::progressive(move |mut out| async move {
        loop {
            // The stages have their own timeouts; this is only a backstop, so
            // one wedged row can't hold the whole table open.
            let _ = tokio::time::timeout(
                checks::ROW_TIMEOUT,
                checks::run(&probe, &endpoint, &mut out),
            )
            .await;
            if !live {
                return;
            }
            tokio::time::sleep(LIVE_PERIOD).await;
        }
    })
}

fn endpoint_table(rows: &[Row], live: bool) -> Table {
    /// A column showing one check of the row.
    fn stage(header: &'static str, pick: checks::PickStage) -> ColumnDef<Row> {
        ColumnDef::new(header, Align::Left, move |r: &Row| {
            value(r.checks.cell(move |c: &RowChecks| pick(c).clone()))
        })
    }

    let mut columns = vec![
        ColumnDef::new("SERVICE", Align::Left, |r: &Row| {
            text(r.endpoint.service.clone())
        }),
        ColumnDef::new("HOSTNAME", Align::Left, |r: &Row| {
            text(
                r.endpoint
                    .hostname
                    .clone()
                    .unwrap_or_else(|| "-".to_string()),
            )
        }),
        ColumnDef::new("PORT", Align::Left, |r: &Row| text(fmt_port(r))),
    ];
    columns.extend(checks::STAGES.map(|(header, pick)| stage(header, pick)));

    columns
        .into_iter()
        .collect::<TableBuilder<Row>>()
        .build(rows, live)
}

/// The host ports are always the same pair, so the only thing worth a column
/// is where they land.
fn fmt_port(row: &Row) -> String {
    match row.endpoint.container_port {
        None => "-".to_string(),
        Some(port) => port.to_string(),
    }
}

/// The notes under a workspace's table: what each `✗` in it means, and what to
/// do about it. Recomputed every frame, so they appear as the checks land.
fn notes(rows: &[Row]) -> impl Fn(u16) -> Vec<String> + use<> {
    let sources: Vec<(String, Gatherer<RowChecks>)> = rows
        .iter()
        .map(|row| (row.endpoint.service.clone(), row.checks.clone()))
        .collect();

    move |width| {
        let mut lines = Vec::new();
        for (label, checks) in &sources {
            for (stage, detail) in checks.snapshot().failures() {
                // A middle dot rather than spacing, since wrapping collapses
                // whitespace and the row's identity would otherwise run into
                // the stage name.
                let subject = format!("{label} · {}", stage.to_lowercase());
                lines.extend(note(&subject, &detail, width));
            }
        }
        if !lines.is_empty() {
            lines.insert(0, String::new());
        }
        lines
    }
}

/// One failure, wrapped to the terminal. Every line has to come back as one
/// physical line or the redraw loop miscounts, so this wraps rather than
/// letting the terminal do it.
fn note(subject: &str, detail: &str, width: u16) -> Vec<String> {
    const INDENT: &str = "  ";
    const HANG: &str = "    ";

    let message = format!("{subject}: {detail}");
    let budget = usize::from(width).saturating_sub(INDENT.len() + 2);
    let mut wrapped = wrap(&message, budget).into_iter();

    let Some(first) = wrapped.next() else {
        return Vec::new();
    };
    let mut lines = vec![format!("{INDENT}{RED}✗{RESET} {first}")];
    lines.extend(wrapped.map(|rest| format!("{HANG}{rest}")));
    lines
}

/// Greedy word wrap. Words longer than the budget get a line to themselves
/// rather than being broken mid-word.
fn wrap(text: &str, budget: usize) -> Vec<String> {
    let budget = budget.max(20);
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(line) if line.chars().count() + 1 + word.chars().count() <= budget => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_string()),
        }
    }
    lines
}

// -- output ------------------------------------------------------------------

fn failed(proxy: &Gatherer<ProxyChecks>, groups: &[Group]) -> bool {
    proxy.snapshot().failed()
        || groups
            .iter()
            .flat_map(|g| &g.rows)
            .any(|r| r.checks.snapshot().failed())
}

#[derive(Serialize)]
struct Json<'a> {
    ok: bool,
    proxy: &'a ProxyChecks,
    endpoints: Vec<JsonEndpoint<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonEndpoint<'a> {
    project: &'a str,
    workspace: &'a str,
    service: &'a str,
    hostname: Option<&'a str>,
    container_port: Option<u16>,
    checks: &'a RowChecks,
}

async fn emit_json(proxy: &Gatherer<ProxyChecks>, rows: &[Row]) -> Result<()> {
    let deadline = tokio::time::Instant::now() + DEADLINE;
    while !settled(proxy, rows) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let proxy_snapshot = proxy.snapshot();
    let row_snapshots: Vec<_> = rows.iter().map(|r| r.checks.snapshot()).collect();
    let ok = !proxy_snapshot.failed() && !row_snapshots.iter().any(|c| c.failed());

    let out = Json {
        ok,
        proxy: &proxy_snapshot,
        endpoints: rows
            .iter()
            .zip(&row_snapshots)
            .map(|(row, checks)| JsonEndpoint {
                project: &row.endpoint.project,
                workspace: &row.endpoint.workspace,
                service: &row.endpoint.service,
                hostname: row.endpoint.hostname.as_deref(),
                container_port: row.endpoint.container_port,
                checks,
            })
            .collect(),
    };

    println!("{}", serde_json::to_string_pretty(&out)?);
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}

fn settled(proxy: &Gatherer<ProxyChecks>, rows: &[Row]) -> bool {
    let proxy_done = PROXY_ROWS
        .iter()
        .all(|(_, pick)| !matches!(pick(&proxy.snapshot()), Datum::Pending));
    proxy_done && rows.iter().all(|r| r.checks.snapshot().settled())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[String]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.replace(&RED.to_string(), "")
                    .replace(&RESET.to_string(), "")
            })
            .collect()
    }

    #[test]
    fn a_note_names_the_row_the_stage_and_the_fix() {
        let lines = note(
            "app 443→8080 · sidecar",
            "no sidecar; run `dc proxy up`",
            200,
        );
        assert_eq!(
            plain(&lines),
            ["  ✗ app 443→8080 · sidecar: no sidecar; run `dc proxy up`"],
        );
    }

    /// Every line has to be one physical line, so a long note is wrapped here
    /// rather than left to the terminal.
    #[test]
    fn a_long_note_wraps_with_a_hanging_indent() {
        let detail = "the system resolver doesn't know main.db.test, and .test isn't routed \
                      to 127.0.0.1:43770 (see the DNS section of the README)";
        let lines = plain(&note("db 5432 · resolv", detail, 60));
        assert!(lines.len() > 1, "expected wrapping, got {lines:?}");
        assert!(lines.iter().all(|l| l.chars().count() <= 60), "{lines:?}");
        assert!(lines[0].starts_with("  ✗ db 5432 · resolv:"));
        assert!(
            lines[1..].iter().all(|l| l.starts_with("    ")),
            "{lines:?}"
        );
    }

    #[test]
    fn wrapping_never_breaks_a_word() {
        let long = "a".repeat(80);
        let wrapped = wrap(&format!("short {long} tail"), 30);
        assert!(wrapped.contains(&long));
    }

    #[test]
    fn wrapping_fills_greedily() {
        assert_eq!(
            wrap("alpha beta gamma delta", 20),
            ["alpha beta gamma", "delta"],
        );
    }

    /// Below the floor the note overflows and gets truncated instead, which
    /// beats one word per line.
    #[test]
    fn wrapping_has_a_minimum_width() {
        assert_eq!(
            wrap("alpha beta gamma delta", 1),
            wrap("alpha beta gamma delta", 20)
        );
    }
}
