use std::path::{Path, PathBuf};

use clap::Args;
use color_eyre::owo_colors::OwoColorize;
use eyre::{WrapErr, eyre};
use serde_json::json;
use tracing::info_span;
use tracing_indicatif::span_ext::IndicatifSpanExt;

use crate::cli::State;
use crate::cli::copy::copy_volumes;
use crate::cli::exec::exec_interactive;
use crate::cli::fwd::forward;
use crate::devcontainer::{Common, Compose};
use crate::run::Runner;
use crate::run::cmd::{Cmd, NamedCmd};
use crate::worktree;

/// Spin up a devcontainer, or restart an existing one
#[derive(Debug, Args)]
pub struct Up {
    /// Copy defaultCopyVolumes from root workspace
    #[arg(short, long)]
    copy: bool,

    /// Foward devcontainer `forwardPorts` once up
    #[arg(short, long)]
    forward: bool,

    /// Detach worktree rather than creating a branch
    #[arg(short, long)]
    detach: bool,

    /// exec once up with the given command [default: configured defaultExec]
    #[arg(short = 'x', long, num_args = 0.., allow_hyphen_values = true)]
    exec: Option<Vec<String>>,
}

impl Up {
    pub async fn run(self, state: State) -> eyre::Result<()> {
        let dc = state.devcontainer()?;
        let dc_options = &dc.common.customizations.dc;

        let name = state.resolve_workspace().await?;
        let is_root = state.is_root(&name);
        let worktree_path = if is_root {
            state.project.path.clone()
        } else {
            let workspace_dir = dc_options.workspace_dir(&state.project.path);
            worktree::create(&state.project.path, &workspace_dir, &name, self.detach).await?
        };

        // Set up span.
        let name = &name;
        let colored_name = name.cyan().to_string();
        let up = "up".cyan().to_string();
        let path = worktree_path.display().to_string();
        let description = &path;
        let message = format!(
            "Spinning up workspace {colored_name} from root {}",
            state.project.path.display()
        );
        let pb_message = format!("[{up}] Spinning up workspace {colored_name}");
        let finish_message = format!("Workspace {colored_name} is available.");
        let span = info_span!(
            "up",
            indicatif.pb_show = true,
            name = up,
            description,
            message,
            finish_message
        );
        span.pb_set_message(&pb_message);
        let _guard = span.enter();

        let crate::devcontainer::Kind::Compose(ref compose) = dc.kind else {
            // This was handled at deserialize time already.
            unimplemented!();
        };

        let config_file = worktree_path
            .join(".devcontainer")
            .join("devcontainer.json");
        let override_file = write_compose_override(
            compose,
            &dc.common,
            &worktree_path,
            &config_file,
            &state.project_name,
            dc_options.mount_git,
            &state.project.path,
        )?;

        // Check if the primary container already exists (re-up vs fresh creation)
        let _already_running = compose_ps_q(compose, &worktree_path, &override_file)
            .await
            .is_ok();

        // initializeCommand runs on the host, from the worktree
        if let Some(ref cmd) = dc.common.initialize_command {
            cmd.run_on_host("initializeCommand", Some(&worktree_path))
                .await?;
        }

        if self.copy && !is_root {
            let root_project = compose_project_name(&state.project.path);
            let new_project = compose_project_name(&worktree_path);

            copy_volumes(&state, Vec::new(), &root_project, &new_project).await?;
        }

        compose_up(compose, &worktree_path, &override_file).await?;

        let container_id = compose_ps_q(compose, &worktree_path, &override_file).await?;
        let user = dc.common.remote_user.as_deref();
        let workdir = Some(compose.workspace_folder.as_path());
        let remote_env = &dc.common.remote_env;

        // Lifecycle commands: create-only commands run only on first creation
        // For now, though, we always recreate.
        if let Some(ref cmd) = dc.common.on_create_command {
            cmd.run_in_container("onCreateCommand", &container_id, user, workdir, remote_env)
                .await?;
        }
        if let Some(ref cmd) = dc.common.update_content_command {
            cmd.run_in_container(
                "updateContentCommand",
                &container_id,
                user,
                workdir,
                remote_env,
            )
            .await?;
        }
        if let Some(ref cmd) = dc.common.post_create_command {
            cmd.run_in_container(
                "postCreateCommand",
                &container_id,
                user,
                workdir,
                remote_env,
            )
            .await?;
        }
        if let Some(ref cmd) = dc.common.post_start_command {
            cmd.run_in_container("postStartCommand", &container_id, user, workdir, remote_env)
                .await?;
        }

        // Port forward if requested
        if self.forward {
            forward(&state, name).await?;
        }

        // Interactive exec if requested
        if let Some(cmd_args) = self.exec {
            exec_interactive(
                &container_id,
                user,
                workdir,
                &cmd_args,
                dc_options.default_exec.as_ref(),
            )?;
        }

        Ok(())
    }
}

/// Match the devcontainer CLI convention: `{basename}_devcontainer`, lowercased,
/// keeping only `[a-z0-9-_]`.
pub(crate) fn compose_project_name(worktree_path: &Path) -> String {
    let basename = worktree_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let raw = format!("{basename}_devcontainer");
    raw.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// Generate a compose override file with:
/// * Our own identification labels
/// * Devcontainer standard labels
/// * Other devcontainer overrides
fn write_compose_override(
    compose: &Compose,
    common: &Common,
    worktree_path: &Path,
    config_file: &Path,
    project_name: &str,
    mount_git: bool,
    project_path: &Path,
) -> eyre::Result<PathBuf> {
    let override_path = std::env::temp_dir().join(format!(
        "{}-override.yml",
        compose_project_name(worktree_path)
    ));
    let local_folder = worktree_path.display();
    let config_file = config_file.display();

    let mut service_obj = json!({
        "labels": [
            format!("devcontainer.local_folder={local_folder}"),
            format!("devcontainer.config_file={config_file}"),
            "dev.dc.managed=true".to_string(),
            format!("dev.dc.project={project_name}"),
        ]
    });

    if !common.container_env.is_empty() {
        service_obj["environment"] = json!(common.container_env);
    }

    if let Some(init) = common.init {
        service_obj["init"] = json!(init);
    }
    if let Some(privileged) = common.privileged {
        service_obj["privileged"] = json!(privileged);
    }
    if !common.cap_add.is_empty() {
        service_obj["cap_add"] = json!(common.cap_add);
    }
    if !common.security_opt.is_empty() {
        service_obj["security_opt"] = json!(common.security_opt);
    }
    if let Some(ref user) = common.container_user {
        service_obj["user"] = json!(user);
    }

    if mount_git && worktree_path != project_path {
        let git_dir = project_path.join(".git");
        let mount = format!("{}:{}", git_dir.display(), git_dir.display());
        service_obj["volumes"] = json!([mount]);
    }

    if compose.override_command {
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
        "services": { &compose.service: service_obj }
    }))?;

    std::fs::write(&override_path, content)
        .wrap_err_with(|| format!("failed to write {}", override_path.display()))?;
    Ok(override_path)
}

pub(crate) fn compose_base_args(
    compose: &Compose,
    worktree_path: &Path,
    override_file: Option<&Path>,
) -> Vec<String> {
    let mut args = vec![
        "compose".into(),
        "-p".into(),
        compose_project_name(worktree_path),
    ];
    for f in &compose.docker_compose_file {
        args.push("-f".into());
        args.push(
            worktree_path
                .join(".devcontainer")
                .join(f)
                .to_string_lossy()
                .into_owned(),
        );
    }
    if let Some(override_file) = override_file {
        args.push("-f".into());
        args.push(override_file.to_string_lossy().into_owned());
    }
    args
}

async fn compose_up(
    compose: &Compose,
    worktree_path: &Path,
    override_file: &Path,
) -> eyre::Result<()> {
    let mut args = vec1::vec1!["docker".into()];
    args.extend(compose_base_args(
        compose,
        worktree_path,
        Some(override_file),
    ));
    args.extend(["up".into(), "-d".into(), "--build".into()]);

    if let Some(ref services) = compose.run_services {
        let mut to_start: Vec<String> = services.clone();
        if !to_start.contains(&compose.service) {
            to_start.push(compose.service.clone());
        }
        args.extend(to_start);
    }

    let cmd = NamedCmd {
        name: "docker compose up",
        cmd: &Cmd::Args(args),
        dir: None,
    };
    Runner::run(cmd).await
}

async fn compose_ps_q(
    compose: &Compose,
    worktree_path: &Path,
    override_file: &Path,
) -> eyre::Result<String> {
    let mut args = compose_base_args(compose, worktree_path, Some(override_file));
    args.extend(["ps".into(), "-q".into(), compose.service.clone()]);

    let out = tokio::process::Command::new("docker")
        .args(&args)
        .output()
        .await?;
    eyre::ensure!(out.status.success(), "docker compose ps failed");
    let output = String::from_utf8(out.stdout)?;
    let id = output.lines().next().unwrap_or("").trim().to_string();
    if id.is_empty() {
        return Err(eyre!(
            "no container found for service '{}'",
            compose.service
        ));
    }
    Ok(id)
}
