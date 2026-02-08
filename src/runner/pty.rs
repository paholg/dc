use std::io::{BufRead, BufReader};
use std::path::Path;

pub fn run_in_pty(argv: &[&str], dir: Option<&Path>) -> eyre::Result<()> {
    let (pty, pts) = pty_process::blocking::open()?;

    let cmd = pty_process::blocking::Command::new(argv[0]).args(&argv[1..]);
    let cmd = match dir {
        Some(d) => cmd.current_dir(d),
        None => cmd,
    };

    let mut child = cmd.spawn(pts)?;

    for line in BufReader::new(pty).lines() {
        match line {
            Ok(line) => tracing::trace!("{line}"),
            Err(e) if e.raw_os_error() == Some(5) => break, // EIO: child closed pty
            Err(e) => return Err(e.into()),
        }
    }

    let status = child.wait()?;
    if !status.success() {
        let code = status.code().unwrap_or(1);
        eyre::bail!("command exited with status {code}");
    }

    Ok(())
}
