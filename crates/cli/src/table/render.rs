//! Rendering for [`Table`](super::Table). The redraw loop that drives this
//! lives in [`Report`](super::Report), which may hold several tables.

use tabular::{Row, Table as TabularTable};

use super::{CellState, Report, Table};
use crate::ansi::{GRAY, RESET};

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

impl Table {
    /// True once no cell is pending.
    pub(super) fn done(&self) -> bool {
        self.grid
            .iter()
            .flatten()
            .all(|c| matches!(c.get(), CellState::Ready(_)))
    }

    /// Render the table to a string and its line count. Pending cells show the
    /// spinner frame, or `-` on the final frame.
    pub(super) fn render_block(
        &self,
        frame: usize,
        final_frame: bool,
        width: u16,
    ) -> (String, u16) {
        let spec = self
            .headers
            .iter()
            .map(|(_, align)| align.spec())
            .collect::<Vec<_>>()
            .join("  ");
        let mut table = TabularTable::new(&spec);

        let mut header = Row::new();
        for (h, _) in &self.headers {
            header.add_cell(*h);
        }
        table.add_row(header);

        let spinner = format!("{GRAY}{}{RESET}", SPINNER[frame % SPINNER.len()]);
        for cells in &self.grid {
            let mut row = Row::new();
            for cell in cells {
                let s = match cell.get() {
                    CellState::Ready(s) => s,
                    CellState::Pending if final_frame => super::dash(),
                    CellState::Pending => spinner.clone(),
                };
                row.add_ansi_cell(s);
            }
            table.add_row(row);
        }

        let rendered = table.to_string();
        let mut out = String::new();
        let mut lines = 0u16;
        for line in rendered.lines() {
            out.push_str(&truncate_visible(line, width));
            out.push('\n');
            lines += 1;
        }
        (out, lines)
    }

    /// Redraw in place until done (non-live) or Ctrl-C (live).
    pub(crate) async fn run_tty(self) -> eyre::Result<()> {
        Report::from(self).run_tty().await
    }

    /// Wait for each cell's first value (up to the deadline), then print once.
    pub(crate) async fn run_piped(self) -> eyre::Result<()> {
        Report::from(self).run_piped().await
    }
}

/// Truncate to `max` visible columns, copying ANSI escapes verbatim and
/// resetting if cut. Keeps each row one physical line so `MoveUp` stays correct.
pub(super) fn truncate_visible(line: &str, max: u16) -> String {
    let max = max as usize;
    let mut out = String::new();
    let mut visible = 0usize;
    let mut chars = line.chars().peekable();
    let mut cut = false;

    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            out.push(c);
            while let Some(&next) = chars.peek() {
                chars.next();
                out.push(next);
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        if visible >= max {
            cut = true;
            break;
        }
        out.push(c);
        visible += 1;
    }

    if cut {
        out.push_str(&RESET.to_string());
    }
    out
}
