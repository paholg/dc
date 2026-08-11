use std::path::PathBuf;

use docker::{MANAGED_LABEL, PROJECT_LABEL, WORKSPACE_LABEL};
use eyre::{Context, eyre};
use serde_json::json;

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

/// Write the compose override and return docker compose base args.
pub(crate) fn compose_cmd(
    devcontainer: &DevcontainerState,
    workspace: &Workspace,
) -> eyre::Result<tokio::process::Command> {
    let override_file_path = write_compose_override(devcontainer, workspace)?;

    let mut cmd = tokio::process::Command::new("docker");

    cmd.args(["compose", "-p"])
        .arg(workspace.compose_project_name());

    let context = devcontainer.context(&workspace.path);
    for f in &devcontainer.config.docker_compose_file {
        let f = context.render_path("dockerComposeFile", f)?;
        cmd.arg("-f")
            .arg(workspace.path.join(".devcontainer").join(f));
    }

    cmd.arg("-f").arg(override_file_path);
    Ok(cmd)
}

pub(crate) async fn compose_ps_q(
    devcontainer: &DevcontainerState,
    workspace: &Workspace<'_>,
) -> eyre::Result<String> {
    let mut cmd = compose_cmd(devcontainer, workspace)?;

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

    let devconcurrent_options = devcontainer.devconcurrent();

    let mut volumes: Vec<String> = devcontainer
        .config
        .mounts
        .iter()
        .map(|entry| entry.to_compose_volume(&context))
        .collect::<eyre::Result<_>>()?;
    if devconcurrent_options.mount_git() && !workspace.is_root {
        // Git worktrees store a tiny `.git` file pointing to the real `.git` dir at the project
        // root; mount the real dir at its original path so `git` works inside the container.
        let git_dir = workspace.state.project.path.join(".git");
        let git_dir = git_dir.display();
        volumes.push(format!("{git_dir}:{git_dir}"));

        // We also need to mount the workspace at the git-aware path so that certain git commands
        // can find it (such as `git --git-dir=...`).
        let ws_dir = workspace.path.display();
        volumes.push(format!("{ws_dir}:{ws_dir}"));
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
