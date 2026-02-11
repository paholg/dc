use bollard::secret::ContainerSummaryStateEnum;
use tabular::{Row, Table};

use crate::{
    bytes::format_bytes,
    workspace::{ExecSession, Workspace},
};

const TABLE_SPEC: &str = "{:<}  {:<}  {:>}  {:>}  {:<}";

fn format_age(created: Option<i64>) -> String {
    let ts = match created {
        Some(secs) => jiff::Timestamp::from_second(secs).ok(),
        None => None,
    };
    let ts = match ts {
        Some(t) => t,
        None => return "-".into(),
    };
    let dur = jiff::Timestamp::now().duration_since(ts);
    let secs = dur.as_secs();
    if secs < 0 {
        return "-".into();
    }
    let secs = secs as u64;
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86400 => format!("{}h", s / 3600),
        s if s < 604800 => format!("{}d", s / 86400),
        s if s < 2_592_000 => format!("{}w", s / 604800),
        s if s < 31_536_000 => format!("{}mo", s / 2_592_000),
        s => format!("{}y", s / 31_536_000),
    }
}

struct WsFields {
    name: String,
    status: String,
    created: String,
    mem: String,
}

fn ws_fields(ws: &Workspace) -> WsFields {
    let name = if ws.dirty {
        format!("{}*", ws.name)
    } else {
        ws.name.clone()
    };
    let status = match ws.status() {
        ContainerSummaryStateEnum::EMPTY => "-".to_string(),
        ref s => s.to_string(),
    };
    let mem = match ws.stats.ram {
        0 => "-".into(),
        ram => format_bytes(ram),
    };
    WsFields {
        name,
        status,
        created: format_age(ws.created()),
        mem,
    }
}

fn ws_row(ws: &Workspace) -> Row {
    let f = ws_fields(ws);
    let execs = if ws.execs.is_empty() {
        "-".to_string()
    } else {
        ws.execs.iter().count().to_string()
    };
    Row::new()
        .with_cell(f.name)
        .with_cell(f.status)
        .with_cell(f.created)
        .with_ansi_cell(f.mem)
        .with_cell(execs)
}

/// Full table with header row, for `list` output.
pub fn workspace_table<'a>(workspaces: impl IntoIterator<Item = &'a Workspace>) -> Table {
    let mut workspaces: Vec<_> = workspaces.into_iter().collect();
    workspaces.sort_by(|a, b| b.root.cmp(&a.root).then_with(|| a.name.cmp(&b.name)));

    let mut table = Table::new(TABLE_SPEC);
    table.add_row(
        Row::new()
            .with_cell("NAME")
            .with_cell("STATUS")
            .with_cell("CREATED")
            .with_cell("MEM")
            .with_cell("EXECS"),
    );
    for ws in workspaces {
        table.add_row(ws_row(ws));
    }
    table
}
