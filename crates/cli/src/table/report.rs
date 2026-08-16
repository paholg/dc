//! A sequence of tables and headings, rendered as one unit.
//!
//! [`Table`] is a single grid; a command that wants a heading, a summary table
//! and a detail table needs them redrawn together, or they fight over the
//! cursor. A [`Report`] owns the redraw loop and concatenates its blocks:
//!
//! - `run_tty`: redraw in place on a tick. Live until Ctrl-C; otherwise until
//!   every cell is ready or the deadline passes, then a final static frame.
//! - `run_piped`: wait for each cell's first value (up to the deadline) and
//!   print once, with no spinner or cursor control.

use std::io::Write;
use std::time::Duration;

use crossterm::{cursor, queue, terminal};

use super::Table;

/// How long the non-live / piped paths wait before showing `-` for whatever is
/// still pending.
pub(crate) const DEFAULT_DEADLINE: Duration = Duration::from_secs(5);

const TICK: Duration = Duration::from_millis(80);

enum Block {
    Text(String),
    /// Lines recomputed each frame, so they can appear as their source
    /// resolves. Given the terminal width, since prose has to be wrapped by
    /// whoever writes it: every line rendered has to be one physical line, or
    /// the redraw loop loses count.
    Lines(Box<dyn Fn(u16) -> Vec<String>>),
    Table(Table),
}

pub(crate) struct Report {
    blocks: Vec<Block>,
    deadline: Duration,
}

impl From<Table> for Report {
    fn from(table: Table) -> Self {
        Report::new().table(table)
    }
}

impl Report {
    pub(crate) fn new() -> Self {
        Report {
            blocks: Vec::new(),
            deadline: DEFAULT_DEADLINE,
        }
    }

    /// A static line — a heading, or a blank line between tables.
    pub(crate) fn text(mut self, line: impl Into<String>) -> Self {
        self.blocks.push(Block::Text(line.into()));
        self
    }

    /// A block of lines that fills in as its source resolves — notes hanging
    /// off a table, say. Returning an empty vec renders nothing.
    pub(crate) fn lines(mut self, f: impl Fn(u16) -> Vec<String> + 'static) -> Self {
        self.blocks.push(Block::Lines(Box::new(f)));
        self
    }

    pub(crate) fn table(mut self, table: Table) -> Self {
        self.blocks.push(Block::Table(table));
        self
    }

    /// Override how long to wait on pending cells. Worth raising for sources
    /// slower than the default 5s, so they land as values rather than `-`.
    pub(crate) fn deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    fn tables(&self) -> impl Iterator<Item = &Table> {
        self.blocks.iter().filter_map(|b| match b {
            Block::Table(t) => Some(t),
            Block::Text(_) | Block::Lines(_) => None,
        })
    }

    /// Live if any of its tables is.
    fn live(&self) -> bool {
        self.tables().any(|t| t.live)
    }

    /// True once no cell in any table is pending.
    fn done(&self) -> bool {
        self.tables().all(Table::done)
    }

    fn render(&self, frame: usize, final_frame: bool, width: u16) -> (String, u16) {
        let mut out = String::new();
        let mut lines = 0u16;
        for block in &self.blocks {
            match block {
                Block::Text(text) => {
                    out.push_str(&super::render::truncate_visible(text, width));
                    out.push('\n');
                    lines += 1;
                }
                Block::Lines(build) => {
                    for line in build(width) {
                        out.push_str(&super::render::truncate_visible(&line, width));
                        out.push('\n');
                        lines += 1;
                    }
                }
                Block::Table(table) => {
                    let (block, block_lines) = table.render_block(frame, final_frame, width);
                    out.push_str(&block);
                    lines += block_lines;
                }
            }
        }
        (out, lines)
    }

    /// Redraw in place until done (non-live) or Ctrl-C (live).
    pub(crate) async fn run_tty(self) -> eyre::Result<()> {
        let mut stderr = std::io::stderr();
        let mut ticker = tokio::time::interval(TICK);
        let start = tokio::time::Instant::now();
        let live = self.live();
        let mut prev_lines = 0u16;
        let mut frame = 0usize;

        loop {
            if live {
                tokio::select! {
                    _ = ticker.tick() => {}
                    _ = tokio::signal::ctrl_c() => {
                        // Leave the last frame; move below it.
                        writeln!(stderr)?;
                        return Ok(());
                    }
                }
            } else {
                ticker.tick().await;
            }

            let finished = !live && (self.done() || start.elapsed() >= self.deadline);
            let width = terminal::size().map(|(c, _)| c).unwrap_or(u16::MAX);
            let (block, lines) = self.render(frame, finished, width);

            if prev_lines > 0 {
                queue!(stderr, cursor::MoveUp(prev_lines))?;
            }
            queue!(stderr, cursor::MoveToColumn(0))?;
            queue!(stderr, terminal::Clear(terminal::ClearType::FromCursorDown))?;
            write!(stderr, "{block}")?;
            stderr.flush()?;

            prev_lines = lines;
            frame += 1;

            if finished {
                return Ok(());
            }
        }
    }

    /// Wait for each cell's first value (up to the deadline), then print once.
    pub(crate) async fn run_piped(mut self) -> eyre::Result<()> {
        self.settle().await;

        let (block, _) = self.render(0, true, u16::MAX);
        print!("{block}");
        std::io::stdout().flush()?;
        Ok(())
    }

    /// Wait for every cell to leave `Pending`, up to the deadline. For callers
    /// that consume the values rather than render them.
    pub(crate) async fn settle(&mut self) {
        let ready: Vec<_> = self
            .blocks
            .iter_mut()
            .filter_map(|b| match b {
                Block::Table(t) => Some(std::mem::take(&mut t.ready)),
                Block::Text(_) | Block::Lines(_) => None,
            })
            .flatten()
            .collect();
        let _ = tokio::time::timeout(self.deadline, futures::future::join_all(ready)).await;
    }
}
