use std::collections::BTreeMap;
use std::time::Duration;

use eyre::{WrapErr, eyre};
use indexmap::IndexMap;
use num_bigint::BigUint;
use rand::distr::{Alphanumeric, SampleString};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::devcontainer::UserEnvProbe;

const PROBE_WARN_AFTER: Duration = Duration::from_secs(2);
const PROBE_TIMEOUT_AFTER: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub(crate) struct ContainerData {
    pub(crate) env: IndexMap<String, String>,
}

impl ContainerData {
    /// Read `Config.Env` from the container.
    pub(crate) async fn inspect(client: &docker::Docker, container_id: &str) -> eyre::Result<Self> {
        let details = client
            .inspect_container(container_id)
            .await
            .wrap_err_with(|| format!("failed to inspect container {container_id}"))?;
        Ok(Self {
            env: details.config.parsed_env(),
        })
    }
}

/// Compute `${devcontainerId}`: SHA-256 of the JSON-encoded `devcontainer.*` labels (with
/// keys sorted), interpreted as a big-endian unsigned integer and base-32 encoded, padded
/// to 52 chars. Mirrors [the reference impl][ref].
///
/// Takes labels rather than a container so the id is available before one exists; the labels
/// are ours to begin with, written into the compose override.
///
/// [ref]: https://github.com/devcontainers/cli/blob/main/src/spec-common/variableSubstitution.ts
pub(crate) fn devcontainer_id<'a>(labels: impl Iterator<Item = (&'a str, &'a str)>) -> String {
    let id_labels: BTreeMap<&str, &str> = labels
        .filter(|(key, _)| key.starts_with("devcontainer."))
        .collect();
    let json = serde_json::to_string(&id_labels).expect("string-keyed map always serializes");
    let digest = Sha256::digest(json.as_bytes());
    format!("{:0>52}", BigUint::from_bytes_be(&digest).to_str_radix(32))
}

/// Run the configured `userEnvProbe` against `container_id`. Returns an empty map for
/// `UserEnvProbe::None`. `PWD` is dropped from the result. `PATH` is merged with `container_env`'s
/// so container-side binaries stay on PATH even if shell init replaced it.
pub(crate) async fn user_env(
    container_id: &str,
    user: Option<&str>,
    container_env: &IndexMap<String, String>,
    kind: UserEnvProbe,
) -> eyre::Result<IndexMap<String, String>> {
    let Some(flags) = posix_shell_flags(kind) else {
        return Ok(IndexMap::new());
    };
    let shell = resolve_user_shell(container_id, user).await?;
    ensure_posix_shell(&shell)?;
    let mut probed = capture_shell_env(container_id, user, &shell, flags).await?;
    if let (Some(probed_path), Some(container_path)) =
        (probed.get("PATH"), container_env.get("PATH"))
    {
        let merged = merge_paths(probed_path, container_path);
        probed.insert("PATH".to_string(), merged);
    }
    Ok(probed)
}

/// Splice `container_path` entries into `shell_path`, preserving the relative order of both sides.
/// Container entries that are already in the shell path advance the insertion point; others get
/// inserted at the current position.
///
/// The reference also drops `/sbin` entries for non-root users; we skip that filter for now —
/// extra sbin entries are mostly harmless and we'd otherwise need to thread the effective user
/// through.
fn merge_paths(shell_path: &str, container_path: &str) -> String {
    let mut result: Vec<&str> = shell_path.split(':').collect();
    let mut insert_at = 0;
    for entry in container_path.split(':') {
        if let Some(found) = result.iter().position(|existing| *existing == entry) {
            insert_at = found + 1;
        } else {
            result.insert(insert_at, entry);
            insert_at += 1;
        }
    }
    result.join(":")
}

/// Read the user's login shell inside the container: `$SHELL` if set, otherwise the shell field
/// from `/etc/passwd`, otherwise `/bin/sh`.
pub(crate) async fn resolve_user_shell(
    container_id: &str,
    user: Option<&str>,
) -> eyre::Result<String> {
    let script = r#"printf %s "${SHELL:-$(getent passwd "$(id -un)" 2>/dev/null | cut -d: -f7)}""#;
    let output = run_in_container(container_id, user, &["sh", "-c", script]).await?;
    let shell = String::from_utf8(output)?.trim().to_string();
    Ok(if shell.is_empty() {
        "/bin/sh".to_string()
    } else {
        shell
    })
}

/// Reject non-POSIX shells with a clear error rather than feeding them flags they don't
/// understand. Devcontainers overwhelmingly use POSIX shells; pwsh/fish/nu users should set
/// `userEnvProbe: "none"`.
fn ensure_posix_shell(shell: &str) -> eyre::Result<()> {
    let name = shell.rsplit('/').next().unwrap_or(shell);
    match name {
        "bash" | "zsh" | "sh" | "dash" | "ash" | "ksh" | "mksh" => Ok(()),
        _ => Err(eyre!(
            "userEnvProbe: unsupported shell '{shell}' \
             (set userEnvProbe to \"none\" in devcontainer.json)"
        )),
    }
}

/// Invoke `<shell> <flags>` inside the container with a sentinel-bracketed `cat /proc/self/environ`
/// and parse the env between markers.
///
/// Warns at 2 seconds and errors at 10 seconds — a hung shell init (e.g. one that prompts via
/// `read`) shouldn't block `dc up` indefinitely.
///
/// We don't fall back to `printenv` if `/proc/self/environ` is empty (the reference impl does).
/// `printenv` separates entries with newlines, corrupting env values that contain `\n`; the
/// fallback only matters for containers without a working `/proc`, which devcontainers
/// effectively always have.
async fn capture_shell_env(
    container_id: &str,
    user: Option<&str>,
    shell: &str,
    flags: &str,
) -> eyre::Result<IndexMap<String, String>> {
    let mark = probe_marker();
    let command = format!("echo -n {mark}; cat /proc/self/environ; echo -n {mark}");
    let argv = [shell, flags, command.as_str()];
    let probe = run_in_container(container_id, user, &argv);
    tokio::pin!(probe);

    if let Ok(result) = tokio::time::timeout(PROBE_WARN_AFTER, &mut probe).await {
        return parse_marked_env(&result?, &mark);
    }
    warn!(
        "userEnvProbe is taking longer than {:?}; check your shell init for prompts \
         or slow operations",
        PROBE_WARN_AFTER,
    );
    let remaining = PROBE_TIMEOUT_AFTER.checked_sub(PROBE_WARN_AFTER).unwrap();
    match tokio::time::timeout(remaining, probe).await {
        Ok(result) => parse_marked_env(&result?, &mark),
        Err(_) => Err(eyre!(
            "userEnvProbe timed out after {:?}",
            PROBE_TIMEOUT_AFTER,
        )),
    }
}

/// `docker exec [-u USER] CONTAINER <argv>`, returning captured stdout on success.
async fn run_in_container(
    container_id: &str,
    user: Option<&str>,
    argv: &[&str],
) -> eyre::Result<Vec<u8>> {
    let mut command = tokio::process::Command::new("docker");
    command.arg("exec");
    // No `-i`, and explicitly null stdin: tokio's `output()` leaves stdin inherited (unlike
    // std's), and a probe that reads the terminal stops a backgrounded `dc x cmd &` with
    // SIGTTIN before the real command ever runs.
    command.stdin(std::process::Stdio::null());
    if let Some(u) = user {
        command.args(["-u", u]);
    }
    command.arg(container_id);
    command.args(argv);
    let output = command.output().await?;
    if !output.status.success() {
        return Err(eyre!(
            "docker exec failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}

fn posix_shell_flags(kind: UserEnvProbe) -> Option<&'static str> {
    match kind {
        UserEnvProbe::None => None,
        UserEnvProbe::LoginInteractiveShell => Some("-lic"),
        UserEnvProbe::LoginShell => Some("-lc"),
        UserEnvProbe::InteractiveShell => Some("-ic"),
    }
}

/// A per-call random sentinel that delimits the env section in the probe shell's stdout, so
/// noise from shell init scripts (banners, prompt echoes) doesn't get parsed as env.
fn probe_marker() -> String {
    Alphanumeric.sample_string(&mut rand::rng(), 32)
}

fn parse_marked_env(stdout: &[u8], mark: &str) -> eyre::Result<IndexMap<String, String>> {
    let text = std::str::from_utf8(stdout)?;
    let start = text
        .find(mark)
        .ok_or_else(|| eyre!("probe output missing leading marker"))?;
    let body = &text[start + mark.len()..];
    let end = body
        .find(mark)
        .ok_or_else(|| eyre!("probe output missing trailing marker"))?;
    Ok(body[..end]
        .split('\0')
        .filter_map(|entry| {
            let (key, value) = entry.split_once('=')?;
            (key != "PWD").then(|| (key.to_string(), value.to_string()))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(labels: &[(&str, &str)]) -> String {
        devcontainer_id(labels.iter().copied())
    }

    #[test]
    fn devcontainer_id_format() {
        let id = id(&[
            ("devcontainer.local_folder", "/host/projects/myrepo"),
            (
                "devcontainer.config_file",
                "/host/projects/myrepo/.devcontainer/devcontainer.json",
            ),
            ("dev.devconcurrent.project", "myrepo"),
        ]);
        assert_eq!(id.len(), 52);
        assert!(
            id.chars().all(|c| matches!(c, '0'..='9' | 'a'..='v')),
            "unexpected character in {id}",
        );
    }

    #[test]
    fn devcontainer_id_stable_across_label_order() {
        assert_eq!(
            id(&[
                ("devcontainer.local_folder", "/foo"),
                ("devcontainer.config_file", "/foo/.devcontainer.json"),
            ]),
            id(&[
                ("devcontainer.config_file", "/foo/.devcontainer.json"),
                ("devcontainer.local_folder", "/foo"),
            ]),
        );
    }

    #[test]
    fn parse_marked_env_extracts_between_markers() {
        let mark = "__M__";
        let stdout = b"shell-banner-text\n__M__PATH=/usr/bin\0HOME=/root\0__M__trailing-noise";
        let env = parse_marked_env(stdout, mark).unwrap();
        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert_eq!(env.get("HOME").map(String::as_str), Some("/root"));
    }

    #[test]
    fn parse_marked_env_drops_pwd() {
        let mark = "__M__";
        let stdout = b"__M__PWD=/somewhere\0HOME=/root\0__M__";
        let env = parse_marked_env(stdout, mark).unwrap();
        assert!(!env.contains_key("PWD"));
        assert_eq!(env.get("HOME").map(String::as_str), Some("/root"));
    }

    #[test]
    fn parse_marked_env_errors_without_markers() {
        assert!(parse_marked_env(b"no markers here", "__M__").is_err());
        assert!(parse_marked_env(b"__M__only one", "__M__").is_err());
    }

    #[test]
    fn posix_shell_flags_matches_spec() {
        assert_eq!(posix_shell_flags(UserEnvProbe::None), None);
        assert_eq!(
            posix_shell_flags(UserEnvProbe::LoginInteractiveShell),
            Some("-lic"),
        );
        assert_eq!(posix_shell_flags(UserEnvProbe::LoginShell), Some("-lc"));
        assert_eq!(
            posix_shell_flags(UserEnvProbe::InteractiveShell),
            Some("-ic")
        );
    }

    #[test]
    fn ensure_posix_shell_accepts_known() {
        for shell in [
            "/bin/bash",
            "/usr/bin/zsh",
            "/bin/sh",
            "/usr/bin/dash",
            "/bin/ash",
            "/bin/ksh",
            "/bin/mksh",
        ] {
            ensure_posix_shell(shell).unwrap_or_else(|e| panic!("{shell} should be accepted: {e}"));
        }
    }

    #[test]
    fn ensure_posix_shell_rejects_unknown() {
        for shell in ["/usr/bin/fish", "/usr/bin/pwsh", "/opt/nu/bin/nu"] {
            let err = ensure_posix_shell(shell).unwrap_err().to_string();
            assert!(err.contains(shell), "expected error to name {shell}: {err}");
            assert!(
                err.contains("userEnvProbe"),
                "error should mention userEnvProbe: {err}"
            );
        }
    }

    #[test]
    fn ensure_posix_shell_bare_name() {
        ensure_posix_shell("bash").unwrap();
        assert!(ensure_posix_shell("fish").is_err());
    }

    #[test]
    fn merge_paths_no_op_when_shell_already_contains_all() {
        let shell = "/usr/local/bin:/usr/bin:/bin";
        let container = "/usr/local/bin:/usr/bin:/bin";
        assert_eq!(merge_paths(shell, container), shell);
    }

    #[test]
    fn merge_paths_when_shell_replaced_path_inserts_container_entries() {
        // User shell init did `export PATH="$HOME/.cargo/bin"` (replace, not prepend).
        let shell = "/home/user/.cargo/bin";
        let container = "/usr/local/bin:/usr/bin:/bin";
        // Container entries inserted at the front so shell-side entry trails.
        assert_eq!(
            merge_paths(shell, container),
            "/usr/local/bin:/usr/bin:/bin:/home/user/.cargo/bin",
        );
    }

    #[test]
    fn merge_paths_interleaves_when_partial_overlap() {
        // Shell PATH has /usr/bin in the middle; container has more entries before and after.
        let shell = "/home/user/bin:/usr/bin:/extra";
        let container = "/usr/local/bin:/usr/bin:/bin";
        // /usr/local/bin gets inserted at the front, /usr/bin matches existing,
        // /bin gets inserted right after /usr/bin.
        assert_eq!(
            merge_paths(shell, container),
            "/usr/local/bin:/home/user/bin:/usr/bin:/bin:/extra",
        );
    }

    #[test]
    fn ensure_posix_shell_accepts_non_standard_paths() {
        // NixOS / Homebrew / asdf etc. install shells outside /bin and /usr/bin.
        ensure_posix_shell("/nix/store/abc123-bash-5.2/bin/bash").unwrap();
        ensure_posix_shell("/opt/homebrew/bin/zsh").unwrap();
        ensure_posix_shell("/home/user/.asdf/shims/bash").unwrap();
    }

    #[test]
    fn devcontainer_id_ignores_non_id_labels() {
        assert_eq!(
            id(&[("devcontainer.local_folder", "/foo")]),
            id(&[
                ("devcontainer.local_folder", "/foo"),
                ("dev.devconcurrent.project", "anything"),
            ]),
        );
    }
}
