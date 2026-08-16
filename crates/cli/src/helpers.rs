use std::io::Write;
use std::path::PathBuf;

use serde::Deserialize;

pub(crate) const SHELL_FD: &str = "DEVCONCURRENT_SHELL_FD";
/// Space-separated names of the variables `show env --export` set last time.
///
/// It exports this alongside the variables themselves, then reads it back on
/// the next run to know what to unset — so leaving a workspace, or dropping a
/// variable from the config, doesn't leave a stale value behind.
pub(crate) const EXPORTED_ENV: &str = "DEVCONCURRENT_ENV";

/// Send a shell command to the calling shell (via the `dc` wrapper function).
///
/// If `DEVCONCURRENT_SHELL_FD` names an open file descriptor, write the command
/// there and the wrapper will `eval` it. Otherwise print to stdout so the user
/// can copy it (or pipe to `eval` themselves).
pub(crate) fn forward_to_shell(command: &str) -> eyre::Result<()> {
    if let Ok(fd) = std::env::var(SHELL_FD) {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(format!("/dev/fd/{fd}"))?;
        writeln!(f, "{command}")?;
    } else {
        println!("{command}");
    }
    Ok(())
}

/// Simple validator for workspace and project names.
///
/// We use the same rules for both for simplicity.
pub(crate) fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("must not be empty".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("{name:?} must contain only [a-zA-Z0-9-_]"));
    }
    Ok(())
}

pub(crate) fn deserialize_shell_path_opt<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<PathBuf>, D::Error> {
    Option::<String>::deserialize(d)
        .map(|o| o.map(|s| PathBuf::from(shellexpand::tilde(&s).as_ref())))
}

pub(crate) fn deserialize_shell_path<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<PathBuf, D::Error> {
    let s = String::deserialize(d)?;
    Ok(PathBuf::from(shellexpand::tilde(&s).as_ref()))
}
