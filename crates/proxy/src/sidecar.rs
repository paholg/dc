//! Lifecycle of the per-service sidecar.
//!
//! One sidecar per `(workspace, service)` that has port remappings, joined to
//! that service's container network namespace. The sidecar runs our own
//! `devconcurrent-proxy` image with the `sidecar` subcommand; it reads its
//! plan from `/etc/sidecar/plan.json` (written here via the docker archive
//! upload API) and binds each `host` port in the target's netns, forwarding
//! to `127.0.0.1:<container>`. TLS-marked ports get a leaf cert minted from
//! the proxy's CA and uploaded alongside the plan.
//!
//! Clients reach the service by connecting to that container's IP at
//! `<host>` — on Linux directly via the docker bridge, on macOS via a tunnel
//! such as docker-mac-net-connect.

use docker::{
    Docker, PROJECT_LABEL, PROXY_CONFIG_HASH_LABEL, PROXY_GROUP_LABEL, PROXY_SERVICE_LABEL,
    PROXY_SIDECAR_LABEL, PROXY_TARGET_LABEL, WORKSPACE_LABEL, build_archive,
};
use eyre::{Result, WrapErr};
use shared::{
    ProxyService, SIDECAR_CERT_FILE, SIDECAR_KEY_FILE, SIDECAR_PLAN_DIR, SIDECAR_PLAN_FILE,
    SidecarPlan,
};

use crate::certs::CaHolder;

/// Image used for sidecars. Same image as the proxy itself; the binary
/// switches modes based on argv.
fn sidecar_image() -> String {
    format!(
        "ghcr.io/paholg/devconcurrent-proxy:{}",
        env!("CARGO_PKG_VERSION")
    )
}

/// Create the sidecar joined to `target_cid`'s netns and start it. Returns
/// the sidecar's container ID, or `None` if `svc` has no port mappings.
///
/// `hostname` is the rendered template result, used only as the SAN of any
/// minted TLS leaf cert.
#[allow(clippy::too_many_arguments)]
pub async fn create_sidecar(
    docker: &Docker,
    ca: Option<&CaHolder>,
    project: &str,
    workspace: &str,
    service: &str,
    svc: &ProxyService,
    hostname: &str,
    target_cid: &str,
) -> Result<Option<String>> {
    // A plain port where host == container is a no-op: DNS already resolves
    // the hostname to the container's IP, and the app binds the port itself.
    // Binding it again in the sidecar would just race the app for `0.0.0.0:port`.
    let ports: Vec<_> = svc
        .ports
        .iter()
        .copied()
        .filter(|p| p.tls || p.host != p.container)
        .collect();
    if ports.is_empty() {
        return Ok(None);
    }

    let image = sidecar_image();
    docker
        .ensure_image(&image)
        .await
        .wrap_err("ensure sidecar image")?;

    let name = sanitize_container_name(&format!(
        "devconcurrent-proxy-sidecar-{project}-{workspace}-{service}"
    ));
    let network_mode = format!("container:{target_cid}");

    // Build the plan JSON and (optionally) mint a cert.
    let plan = SidecarPlan {
        hostname: hostname.to_string(),
        ports,
    };
    let plan_json = serde_json::to_vec_pretty(&plan).wrap_err("serialize sidecar plan")?;

    let tls_pair: Option<(Vec<u8>, Vec<u8>)> = if plan.ports.iter().any(|p| p.tls) {
        if let Some(ca) = ca {
            match ca.mint(hostname) {
                Ok((cert_pem, key_pem)) => Some((cert_pem.into_bytes(), key_pem.into_bytes())),
                Err(e) => {
                    tracing::warn!(
                        hostname,
                        "mint cert failed; TLS ports will be disabled: {e:?}"
                    );
                    None
                }
            }
        } else {
            tracing::warn!(
                hostname,
                "TLS ports declared but proxy has no CA; TLS ports will be disabled"
            );
            None
        }
    } else {
        None
    };

    // If a stale sidecar exists from a previous run, force-remove it first.
    match docker.remove_container(&name).force(true).call().await {
        Ok(()) | Err(docker::Error::NotFound) => {}
        Err(e) => {
            tracing::warn!(name = %name, "remove stale sidecar: {e}");
        }
    }

    let id = docker
        .create_container(&name)
        .image(&image)
        .network_mode(&network_mode)
        .cmd(vec!["sidecar".to_string()])
        .with_label(PROXY_GROUP_LABEL, "true")
        .with_label(PROXY_SIDECAR_LABEL, "true")
        .with_label(PROXY_CONFIG_HASH_LABEL, plan.hash())
        .with_label(PROXY_TARGET_LABEL, target_cid)
        .with_label(PROJECT_LABEL, project)
        .with_label(WORKSPACE_LABEL, workspace)
        .with_label(PROXY_SERVICE_LABEL, service)
        .call()
        .await
        .wrap_err("create sidecar container")?;

    // Upload plan (and TLS cert+key, if any) into /etc/sidecar/ before start.
    let mut files: Vec<(&str, &[u8])> = vec![(SIDECAR_PLAN_FILE, plan_json.as_slice())];
    if let Some((cert, key)) = &tls_pair {
        files.push((SIDECAR_CERT_FILE, cert.as_slice()));
        files.push((SIDECAR_KEY_FILE, key.as_slice()));
    }
    let tar = build_archive(&files);
    docker
        .upload_archive(&id, SIDECAR_PLAN_DIR, tar)
        .await
        .wrap_err("upload sidecar plan")?;

    docker
        .start_container(&id)
        .await
        .wrap_err("start sidecar container")?;
    Ok(Some(id))
}

/// Remove a sidecar by its container ID. Errors are logged, not propagated.
pub async fn remove_sidecar(docker: &Docker, id: &str) {
    match docker.remove_container(id).force(true).call().await {
        Ok(()) | Err(docker::Error::NotFound) => {}
        Err(e) => tracing::warn!(id = %id, "remove sidecar: {e}"),
    }
}

/// Remove every sidecar whose `PROXY_TARGET_LABEL` no longer points to a
/// running container — leftovers after a proxy crash.
pub async fn sweep_orphans(docker: &Docker) -> Result<()> {
    let sidecars = docker
        .list_containers()
        .all(true)
        .with_label(PROXY_SIDECAR_LABEL, "true")
        .call()
        .await
        .wrap_err("list sidecars")?;
    for sc in sidecars {
        let target_cid = if let Some(cid) = sc.labels.get(PROXY_TARGET_LABEL) {
            cid.clone()
        } else {
            tracing::warn!(sidecar = %sc.id, "sidecar without target label; removing");
            remove_sidecar(docker, &sc.id).await;
            continue;
        };
        let alive = match docker.inspect_container(&target_cid).await {
            Ok(d) => d.state.running,
            Err(docker::Error::NotFound) => false,
            Err(e) => {
                tracing::warn!(cid = %target_cid, "inspect target during sweep: {e}");
                true
            }
        };
        if !alive {
            tracing::info!(sidecar = %sc.id, "removing orphaned sidecar");
            remove_sidecar(docker, &sc.id).await;
        }
    }
    Ok(())
}

fn sanitize_container_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
