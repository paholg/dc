use std::env;
use std::path::{Path, PathBuf};

use tokio::process::Command;

use crate::error::{Error, Result};

/// The conventional daemon socket, and the only path Docker Desktop
/// special-cases when sharing a socket into a container.
const DEFAULT_SOCKET: &str = "/var/run/docker.sock";

/// Locate the Unix socket of the local docker (or podman-as-docker) daemon.
///
/// Resolution order (first existing socket wins; tried-and-missing paths are
/// included in the error if everything fails):
///
/// 1. `$DOCKER_HOST` env var, if set. Must use the `unix://` scheme.
/// 2. `docker context inspect`, if `docker` is on `PATH`.
/// 3. `$XDG_RUNTIME_DIR/podman/podman.sock` (rootless podman without docker CLI).
/// 4. `/var/run/docker.sock` and `/run/podman/podman.sock`, in that order.
pub async fn discover_socket() -> Result<PathBuf> {
    let mut tried = Vec::new();

    if let Ok(host) = env::var("DOCKER_HOST") {
        let raw = host
            .strip_prefix("unix://")
            .ok_or_else(|| Error::NonUnixHost { host: host.clone() })?;
        let socket = PathBuf::from(raw);
        if socket.exists() {
            return Ok(socket);
        }
        tried.push(socket);
    }

    if let Some(socket) = docker_context_socket().await {
        if socket.exists() {
            return Ok(socket);
        }
        tried.push(socket);
    }

    if let Ok(xdg) = env::var("XDG_RUNTIME_DIR") {
        let socket = PathBuf::from(xdg).join("podman/podman.sock");
        if socket.exists() {
            return Ok(socket);
        }
        tried.push(socket);
    }

    for path in [DEFAULT_SOCKET, "/run/podman/podman.sock"] {
        let socket = PathBuf::from(path);
        if socket.exists() {
            return Ok(socket);
        }
        tried.push(socket);
    }

    Err(Error::SocketNotFound { tried })
}

/// The host path to bind-mount so that a *container* reaches the same daemon
/// `socket` talks to.
///
/// Usually that is the socket itself. The exception is Docker Desktop, which
/// runs the daemon in a VM: a bind of `/var/run/docker.sock` is special-cased
/// and wired to the daemon, while any other host path — including the per-user
/// socket Docker Desktop creates at `~/.docker/run/docker.sock`, which is what
/// `docker context` reports and so what [`discover_socket`] finds — goes
/// through the VM's file sharing and arrives in the container as a socket
/// nothing is listening on.
///
/// Docker Desktop symlinks `/var/run/docker.sock` at that per-user socket, so
/// when the two resolve to the same file the literal path is the same daemon
/// and is the safer source to mount. When they resolve differently, or the
/// default path isn't there at all, `socket` is used as-is: mounting some
/// *other* daemon's socket (a rootful docker alongside rootless podman, say)
/// would be worse than a mount that doesn't work.
pub(crate) fn mount_source(socket: &Path) -> PathBuf {
    mount_source_with_default(socket, Path::new(DEFAULT_SOCKET))
}

fn mount_source_with_default(socket: &Path, default: &Path) -> PathBuf {
    if socket == default {
        return socket.to_path_buf();
    }
    match (socket.canonicalize(), default.canonicalize()) {
        (Ok(a), Ok(b)) if a == b => default.to_path_buf(),
        _ => socket.to_path_buf(),
    }
}

async fn docker_context_socket() -> Option<PathBuf> {
    let out = Command::new("docker")
        .args(["context", "inspect", "-f", "{{.Endpoints.docker.Host}}"])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let host = String::from_utf8(out.stdout).ok()?;
    host.trim().strip_prefix("unix://").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    /// A stand-in for `/var/run/docker.sock` and the per-user socket it points
    /// at on Docker Desktop.
    fn linked_pair(dir: &Path) -> (PathBuf, PathBuf) {
        let socket = dir.join("user-docker.sock");
        std::fs::write(&socket, "").unwrap();
        let default = dir.join("default-docker.sock");
        symlink(&socket, &default).unwrap();
        (socket, default)
    }

    #[test]
    fn the_default_socket_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (_, default) = linked_pair(dir.path());
        assert_eq!(mount_source_with_default(&default, &default), default);
    }

    #[test]
    fn the_default_path_wins_when_it_is_the_same_socket() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, default) = linked_pair(dir.path());
        assert_eq!(mount_source_with_default(&socket, &default), default);
    }

    #[test]
    fn a_different_daemon_at_the_default_path_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("podman.sock");
        let default = dir.path().join("default-docker.sock");
        std::fs::write(&socket, "").unwrap();
        std::fs::write(&default, "").unwrap();
        assert_eq!(mount_source_with_default(&socket, &default), socket);
    }

    #[test]
    fn a_missing_default_path_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("podman.sock");
        std::fs::write(&socket, "").unwrap();
        let default = dir.path().join("nope.sock");
        assert_eq!(mount_source_with_default(&socket, &default), socket);
    }
}
