use std::process::Command;

use eyre::{Result, bail};

/// Run `argv` as root, with stdio inherited so sudo can prompt for a
/// password.
pub(super) fn run_root(argv: &[&str]) -> Result<()> {
    let as_root = rustix::process::geteuid().is_root();
    let mut cmd = if as_root {
        let mut cmd = Command::new(argv[0]);
        cmd.args(&argv[1..]);
        cmd
    } else {
        let mut cmd = Command::new("sudo");
        cmd.arg("--prompt=\"[sudo] password (to modify the system trust store): \"")
            .arg("--")
            .args(argv);
        cmd
    };

    let display = argv.join(" ");
    let status = match cmd.status() {
        Err(e) if !as_root && e.kind() == std::io::ErrorKind::NotFound => {
            bail!("`sudo` isn't available; rerun this command as root")
        }
        other => other.map_err(|e| eyre::eyre!("run `{display}`: {e}"))?,
    };
    eyre::ensure!(status.success(), "`{display}` failed ({status})");
    Ok(())
}
