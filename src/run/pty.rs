use std::path::Path;
use tokio::io::{AsyncBufReadExt, BufReader};

pub async fn run_in_pty(argv: &[&str], dir: Option<&Path>) -> eyre::Result<()> {
    let (pty, pts) = pty_process::open()?;

    let cmd = pty_process::Command::new(argv[0]).args(&argv[1..]);
    let cmd = match dir {
        Some(d) => cmd.current_dir(d),
        None => cmd,
    };

    let mut child = cmd.spawn(pts)?;

    let mut lines = BufReader::new(pty).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => tracing::trace!("{line}"),
            Ok(None) => break,
            Err(e) if e.raw_os_error() == Some(5) => break, // EIO: child closed pty
            Err(e) => return Err(e.into()),
        }
    }

    let status = child.wait().await?;
    if !status.success() {
        let code = status.code().unwrap_or(1);
        eyre::bail!("command exited with status {code}");
    }

    Ok(())
}
