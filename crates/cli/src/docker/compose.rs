use std::path::{Component, Path, PathBuf};

use docker::{
    COMPOSE_PROJECT_LABEL, LOCAL_FOLDER_LABEL, MANAGED_LABEL, PROJECT_LABEL, WORKSPACE_LABEL,
};
use eyre::{Context, eyre};
use indexmap::IndexMap;
use serde_json::json;

use crate::run::run_command;
use crate::{state::DevcontainerState, workspace::Workspace};

fn override_path(workspace: &Workspace) -> PathBuf {
    workspace
        .state
        .project_working_dir()
        .join(format!("{}-override.yml", workspace.name))
}

pub(crate) fn remove_override_file(workspace: &Workspace) {
    let path = override_path(workspace);

    if path.exists()
        && let Err(e) = std::fs::remove_file(&path)
    {
        eprintln!("warning: failed to remove {}: {e}", path.display());
    }
}

/// The directory containing the `devcontainer.json` file, used as a reference for
/// `dockerComposeFile`.
///
/// The config must be the workspace's own — every caller loads it with
/// [`State::devcontainer_for`](crate::state::State::devcontainer_for). A project
/// configured entirely from `config.toml` has no devcontainer.json to be
/// relative to, so those paths resolve against the workspace root.
fn config_dir(devcontainer: &DevcontainerState, workspace: &Workspace) -> PathBuf {
    devcontainer
        .labels
        .config_file()
        .and_then(Path::parent)
        .map_or_else(|| workspace.path.clone(), Path::to_path_buf)
}

/// Lexically fold away `.` and `..`, without resolving symlinks or hitting the filesystem.
///
/// This is how the reference implemenation resolves compose paths.
fn normalize_compose_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// The workspace's compose files, absolute and normalized.
pub(crate) fn compose_files(
    devcontainer: &DevcontainerState,
    workspace: &Workspace,
) -> eyre::Result<Vec<PathBuf>> {
    let context = devcontainer.context(&workspace.path);
    let dir = config_dir(devcontainer, workspace);

    devcontainer
        .config
        .docker_compose_file
        .iter()
        .map(|f| {
            let f = context.render_path("dockerComposeFile", f)?;
            Ok(normalize_compose_path(&dir.join(f)))
        })
        .collect()
}

/// Sanitize a project name the way docker compose (>= 1.21) and the reference
/// implementation's `toProjectName` do: lowercase, keeping only `[a-z0-9-_]`.
pub(crate) fn to_project_name(raw: &str) -> String {
    raw.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// The project name docker compose picks on its own, from the basename of the
/// project directory (the folder holding the first compose file).
fn default_project_name(first_compose_file: &Path) -> String {
    first_compose_file
        .parent()
        .and_then(Path::file_name)
        .map(|name| to_project_name(&name.to_string_lossy()))
        .unwrap_or_default()
}

/// The project name from the workspace and compose file layout alone.
fn derive_project_name(workspace_path: &Path, first_compose_file: &Path) -> String {
    let Some(working_dir) = first_compose_file.parent() else {
        return String::new();
    };

    if working_dir == workspace_path.join(".devcontainer") {
        let basename = workspace_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        to_project_name(&format!("{basename}_devcontainer"))
    } else {
        default_project_name(first_compose_file)
    }
}

/// A `name:` set by the compose files themselves, if any.
///
/// Read from `docker compose config` (without `-p`, so compose reports what it
/// would pick unprompted) rather than by parsing the files, so that `name:` in
/// any fragment, `COMPOSE_PROJECT_NAME` in the project directory's `.env`, and
/// variable interpolation all resolve exactly as they will for anyone else
/// running compose here. Compose falls back to the project directory's
/// basename, which says nothing, so that answer is discarded.
async fn configured_project_name(files: &[PathBuf]) -> eyre::Result<Option<String>> {
    let mut cmd = tokio::process::Command::new("docker");
    cmd.arg("compose");
    for f in files {
        cmd.arg("-f").arg(f);
    }
    cmd.args(["config", "--format", "json"]);

    let out = cmd
        .output()
        .await
        .wrap_err("failed to run docker compose")?;
    eyre::ensure!(
        out.status.success(),
        "docker compose config failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );

    let config: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let Some(name) = config.get("name").and_then(serde_json::Value::as_str) else {
        return Ok(None);
    };

    let name = to_project_name(name);
    let default = files.first().map(|f| default_project_name(f));
    Ok((!name.is_empty() && Some(&name) != default.as_ref()).then_some(name))
}

async fn resolve_project_name(
    devcontainer: &DevcontainerState,
    workspace: &Workspace<'_>,
) -> eyre::Result<String> {
    // The environment wins, for us as for compose itself.
    if let Some(name) = std::env::var_os("COMPOSE_PROJECT_NAME") {
        let name = to_project_name(&name.to_string_lossy());
        if !name.is_empty() {
            return Ok(name);
        }
    }

    let files = compose_files(devcontainer, workspace)?;
    let first = files
        .first()
        .ok_or_else(|| eyre!("devcontainer.json has no dockerComposeFile"))?;
    let derived = derive_project_name(&workspace.path, first);

    match configured_project_name(&files).await {
        Ok(Some(name)) => Ok(name),
        Ok(None) => Ok(derived),
        Err(e) => {
            // Nothing else works if the compose files are unreadable, but this
            // is not the place to report it: fall back so that the caller fails
            // on its own compose command, with compose's own message.
            tracing::debug!("could not read the compose configuration: {e:#}");
            Ok(derived)
        }
    }
}

/// The compose project name for this workspace.
///
/// Derived the way the devcontainer reference implementation derives it (`getProjectName` in
/// devcontainers/cli's `dockerCompose.ts`). We need to respect the reference here so that our
/// containers are recognized by other tools (e.g. VS Code).
pub(crate) async fn project_name<'a>(
    devcontainer: &'a DevcontainerState,
    workspace: &Workspace<'_>,
) -> eyre::Result<&'a str> {
    devcontainer
        .compose_project
        .get_or_try_init(|| resolve_project_name(devcontainer, workspace))
        .await
        .map(String::as_str)
}

/// `docker compose` pointed at the workspace's own compose files, with no
/// project name and without our override. Enough for questions the project name
/// has no bearing on; anything that touches containers wants [`compose_cmd`].
fn compose_config_cmd(
    devcontainer: &DevcontainerState,
    workspace: &Workspace,
) -> eyre::Result<tokio::process::Command> {
    let mut cmd = tokio::process::Command::new("docker");
    cmd.arg("compose");

    for f in compose_files(devcontainer, workspace)? {
        cmd.arg("-f").arg(f);
    }

    Ok(cmd)
}

/// Write the compose override and return docker compose base args.
pub(crate) async fn compose_cmd(
    devcontainer: &DevcontainerState,
    workspace: &Workspace<'_>,
) -> eyre::Result<tokio::process::Command> {
    let override_file_path = write_compose_override(devcontainer, workspace)?;

    let mut cmd = tokio::process::Command::new("docker");
    cmd.args(["compose", "-p"])
        .arg(project_name(devcontainer, workspace).await?);

    for f in compose_files(devcontainer, workspace)? {
        cmd.arg("-f").arg(f);
    }
    cmd.arg("-f").arg(override_file_path);

    Ok(cmd)
}

/// The image the primary service resolves to.
///
/// Asking compose beats deriving the name ourselves: for a service that only
/// has `build:`, the answer is compose's own `{project}-{service}` convention,
/// whose separator changed in compose 2.8.
pub(crate) async fn compose_image(
    devcontainer: &DevcontainerState,
    workspace: &Workspace<'_>,
) -> eyre::Result<String> {
    let service = &devcontainer.config.service;
    // Without our override, so this reports the service's own image rather than
    // a pin `up` may already have written — but *with* `-p`, since a build-only
    // service takes its image name from the project name, and compose would
    // otherwise fall back to the compose file's directory.
    let mut cmd = compose_config_cmd(devcontainer, workspace)?;
    cmd.arg("-p")
        .arg(project_name(devcontainer, workspace).await?);
    cmd.args(["config", "--images", service]);

    let out = cmd
        .output()
        .await
        .wrap_err("failed to run docker compose")?;
    eyre::ensure!(
        out.status.success(),
        "docker compose config --images failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );

    String::from_utf8(out.stdout)?
        .lines()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| eyre!("docker compose reported no image for service '{service}'"))
}

/// Every service defined by this workspace's compose files, sorted.
///
/// Read from the compose configuration rather than from running containers, so
/// the answer doesn't depend on what happens to be up, and so it covers
/// services that carry no `proxy.services` entry.
pub(crate) async fn compose_services(
    devcontainer: &DevcontainerState,
    workspace: &Workspace<'_>,
) -> eyre::Result<Vec<String>> {
    let mut cmd = compose_config_cmd(devcontainer, workspace)?;
    cmd.args(["config", "--services"]);

    let out = cmd
        .output()
        .await
        .wrap_err("failed to run docker compose")?;
    eyre::ensure!(
        out.status.success(),
        "docker compose config --services failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );

    let mut services: Vec<String> = String::from_utf8(out.stdout)?
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    services.sort();
    Ok(services)
}

/// Pull the primary service's image.
///
/// Through compose rather than the API, so the user's registry credentials
/// apply.
pub(crate) async fn compose_pull(
    devcontainer: &DevcontainerState,
    workspace: &Workspace<'_>,
) -> eyre::Result<()> {
    let mut cmd = compose_config_cmd(devcontainer, workspace)?;
    cmd.args(["pull", &devcontainer.config.service]);
    run_command(cmd, "docker compose pull").await
}

pub(crate) async fn compose_ps_q(
    devcontainer: &DevcontainerState,
    workspace: &Workspace<'_>,
) -> eyre::Result<String> {
    let mut cmd = compose_cmd(devcontainer, workspace).await?;

    let service = &devcontainer.config.service;
    cmd.arg("ps").arg("-q").arg(service);

    let out = cmd.output().await?;
    eyre::ensure!(out.status.success(), "docker compose ps failed");
    let output = String::from_utf8(out.stdout)?;
    let id = output.lines().next().unwrap_or("").trim().to_string();
    if id.is_empty() {
        return Err(eyre!("no container found for service '{}'", service));
    }
    Ok(id)
}

/// Describe the workspace a container belongs to, when it is not this one.
///
/// The compose project name comes from the worktree folder name, which is only
/// unique within a project — so two projects with a workspace of the same name
/// land in one compose project, where `up --remove-orphans` and `down -v`
/// operate on each other's containers.
fn foreign_claim(
    labels: &IndexMap<String, String>,
    project: &str,
    workspace: &str,
    local_folder: &Path,
) -> Option<String> {
    let claim_project = labels.get(PROJECT_LABEL);
    let claim_workspace = labels.get(WORKSPACE_LABEL);
    let claim_folder = labels.get(LOCAL_FOLDER_LABEL);

    let foreign = claim_project.is_some_and(|p| p != project)
        || claim_workspace.is_some_and(|w| w != workspace)
        // A container some other tool started carries no labels of ours, but
        // the devcontainer spec's own label still says which folder it is for.
        || claim_folder.is_some_and(|f| Path::new(f) != local_folder);

    if !foreign {
        return None;
    }

    Some(match (claim_project, claim_workspace, claim_folder) {
        (Some(project), Some(workspace), _) => {
            format!("project '{project}' (workspace '{workspace}')")
        }
        (Some(project), None, _) => format!("project '{project}'"),
        (None, _, Some(folder)) => format!("the devcontainer for {folder}"),
        (None, _, None) => "another workspace".to_string(),
    })
}

/// Refuse to run compose against a project name another workspace has claimed.
pub(crate) async fn ensure_project_unclaimed(
    devcontainer: &DevcontainerState,
    workspace: &Workspace<'_>,
    project_name: &str,
) -> eyre::Result<()> {
    let client = &devcontainer.docker().await?.client;

    let containers = client
        .list_containers()
        .all(true)
        .with_label(COMPOSE_PROJECT_LABEL, project_name)
        .call()
        .await?;

    for c in containers {
        if let Some(claim) = foreign_claim(
            &c.labels,
            workspace.state.project_name.as_str(),
            &workspace.name,
            &workspace.path,
        ) {
            return Err(eyre!(
                "\
The compose project '{project_name}' already belongs to {claim}.

Two workspaces cannot share a name, even across projects. The decontainer convention uses the
workspace directory name only."
            ));
        }
    }

    Ok(())
}

/// Generate a compose override file
///
/// We set the standard devcontainer labels, our own labels, and any appropriate overrides from
/// devcontainer.json.
fn write_compose_override(
    devcontainer: &DevcontainerState,
    workspace: &Workspace,
) -> eyre::Result<PathBuf> {
    let override_path = override_path(workspace);

    let mut labels = vec![
        format!("{}=true", MANAGED_LABEL),
        format!("{}={}", PROJECT_LABEL, workspace.state.project_name),
        format!("{}={}", WORKSPACE_LABEL, workspace.name),
    ];
    // The `devcontainer.*` labels are also what `${devcontainerId}` hashes, so
    // they come from the same place the substitution context reads them.
    labels.extend(
        devcontainer
            .labels
            .pairs()
            .into_iter()
            .map(|(key, value)| format!("{key}={value}")),
    );
    let mut service_obj = json!({
        "labels": labels
    });

    let context = devcontainer.context(&workspace.path);
    let env: indexmap::IndexMap<String, String> = devcontainer
        .config
        .container_env
        .iter()
        .map(|(k, v)| {
            Ok((
                k.clone(),
                context.render_field(&format!("containerEnv.{k}"), v)?,
            ))
        })
        .collect::<eyre::Result<_>>()?;
    if !env.is_empty() {
        service_obj["environment"] = json!(env);
    }

    if let Some(init) = devcontainer.config.init {
        service_obj["init"] = json!(init);
    }
    if let Some(privileged) = devcontainer.config.privileged {
        service_obj["privileged"] = json!(privileged);
    }
    if !devcontainer.config.cap_add.is_empty() {
        service_obj["cap_add"] = json!(devcontainer.config.cap_add);
    }
    if !devcontainer.config.security_opt.is_empty() {
        service_obj["security_opt"] = json!(devcontainer.config.security_opt);
    }
    if let Some(user) = &devcontainer.config.container_user {
        service_obj["user"] = json!(context.render_field("containerUser", user)?);
    }
    if let Some(image) = devcontainer.derived_image.get() {
        service_obj["image"] = json!(image);
    }

    let devconcurrent_options = devcontainer.devconcurrent();

    let mut volumes: Vec<serde_json::Value> = devcontainer
        .config
        .mounts
        .iter()
        .map(|entry| Ok(json!(entry.to_compose_volume(&context)?)))
        .collect::<eyre::Result<_>>()?;

    if devconcurrent_options.mount_git() && !workspace.is_root {
        let bind = |path: &Path| {
            json!({
                "type": "bind",
                "source": path.display().to_string(),
                "target": path.display().to_string(),
            })
        };

        // Git worktrees store a tiny `.git` file pointing to the real `.git` dir at the project
        // root; mount the real dir at its original path so `git` works inside the container.
        volumes.push(bind(&workspace.state.project.path.join(".git")));

        // We also need to mount the workspace at the git-aware path so that certain git commands
        // can find it (such as `git --git-dir=...`).
        volumes.push(bind(&workspace.path));
    }

    if !volumes.is_empty() {
        service_obj["volumes"] = json!(volumes);
    }

    if devcontainer.config.override_command {
        // I believe this is the reference devcontainer overrideCommand.
        service_obj["entrypoint"] = json!([
            "/bin/sh",
            "-c",
            r#"echo Container started
 trap "exit 0" 15

 exec "$@"
 while sleep 1 & wait $!; do :; done"#,
            "-"
        ]);
        service_obj["command"] = json!([]);
    }

    let content = serde_json::to_string_pretty(&json!({
        "services": { &devcontainer.config.service: service_obj }
    }))?;

    workspace.state.ensure_project_working_dir()?;
    std::fs::write(&override_path, content)
        .wrap_err_with(|| format!("failed to write {}", override_path.display()))?;
    Ok(override_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn derive(workspace: &str, compose_file: &str) -> String {
        derive_project_name(
            Path::new(workspace),
            &normalize_compose_path(Path::new(compose_file)),
        )
    }

    /// Each case is what the devcontainers CLI computes for the same layout.
    #[test]
    fn the_devcontainer_suffix_only_applies_inside_dot_devcontainer() {
        // .devcontainer/devcontainer.json + "docker-compose.yml"
        assert_eq!(
            derive("/w/fix", "/w/fix/.devcontainer/docker-compose.yml"),
            "fix_devcontainer"
        );
        // .devcontainer/devcontainer.json + "../docker-compose.yml"
        assert_eq!(
            derive("/w/fix", "/w/fix/.devcontainer/../docker-compose.yml"),
            "fix"
        );
        // .devcontainer.json at the workspace root + "docker-compose.yml"
        assert_eq!(derive("/w/fix", "/w/fix/docker-compose.yml"), "fix");
        // .devcontainer/sub/devcontainer.json + "docker-compose.yml"
        assert_eq!(
            derive("/w/fix", "/w/fix/.devcontainer/sub/docker-compose.yml"),
            "sub"
        );
    }

    #[test]
    fn project_names_are_sanitized() {
        assert_eq!(
            derive("/w/Fix.1", "/w/Fix.1/.devcontainer/c.yml"),
            "fix1_devcontainer"
        );
        assert_eq!(to_project_name("A b-c_d.e"), "ab-c_de");
    }

    #[test]
    fn compose_default_name_is_the_project_directory() {
        assert_eq!(
            default_project_name(Path::new("/w/fix/.devcontainer/docker-compose.yml")),
            "devcontainer"
        );
        assert_eq!(
            default_project_name(Path::new("/w/fix/docker-compose.yml")),
            "fix"
        );
    }

    fn labels(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn claim(pairs: &[(&str, &str)]) -> Option<String> {
        foreign_claim(&labels(pairs), "proj", "fix", Path::new("/w/fix"))
    }

    #[test]
    fn our_own_containers_are_not_a_claim() {
        assert_eq!(
            claim(&[
                (PROJECT_LABEL, "proj"),
                (WORKSPACE_LABEL, "fix"),
                (LOCAL_FOLDER_LABEL, "/w/fix"),
            ]),
            None
        );
        // Started by another tool, for this same folder.
        assert_eq!(claim(&[(LOCAL_FOLDER_LABEL, "/w/fix")]), None);
        // Nothing we can attribute.
        assert_eq!(claim(&[]), None);
    }

    #[test]
    fn another_project_is_a_claim() {
        assert_eq!(
            claim(&[(PROJECT_LABEL, "other"), (WORKSPACE_LABEL, "fix")]),
            Some("project 'other' (workspace 'fix')".to_string())
        );
    }

    /// `Foo` and `foo` sanitize to one project name.
    #[test]
    fn a_case_folded_workspace_is_a_claim() {
        assert_eq!(
            claim(&[(PROJECT_LABEL, "proj"), (WORKSPACE_LABEL, "Fix")]),
            Some("project 'proj' (workspace 'Fix')".to_string())
        );
    }

    #[test]
    fn another_tools_container_for_another_folder_is_a_claim() {
        assert_eq!(
            claim(&[(LOCAL_FOLDER_LABEL, "/elsewhere/fix")]),
            Some("the devcontainer for /elsewhere/fix".to_string())
        );
    }
}
