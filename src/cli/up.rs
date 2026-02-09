use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

use bollard::Docker;
use clap::Args;
use eyre::eyre;
use serde_json::json;

use crate::cli::copy::copy_volumes;
use crate::config::Config;
use crate::devcontainer::{Common, Compose, DevContainer};
use crate::runner;
use crate::runner::cmd::Cmd;
use crate::worktree;

/// Spin up a devcontainer
#[derive(Debug, Args)]
pub struct Up {
    #[arg(
        short,
        long,
        help = "name of project [default: The first one configured]"
    )]
    project: Option<String>,

    #[arg(help = "name of new workspace [default: Root workspace]")]
    name: Option<PathBuf>,

    #[arg(
        short = 'x',
        long,
        num_args = 0..,
        allow_hyphen_values = true,
        help = "exec into it once up with the given command [default: conigured defaultExec]"
    )]
    exec: Option<Vec<String>>,

    #[arg(
        short = 'c',
        long,
        num_args = 0..,
        help = "copy named volumes from root workspace [default: configured defaultCopyVolumes]"
    )]
    copy: Option<Vec<String>>,
}

impl Up {
    pub async fn run(self, docker: &Docker, config: &Config) -> eyre::Result<()> {
        let (name, project) = config.project(self.project.as_deref())?;

        let dc = DevContainer::load(project)?;
        let dc_options = &dc.common.customizations.dc;

        let worktree_path = match self.name {
            Some(ref ws_name) => {
                let workspace_dir = dc_options.workspace_dir();
                let ws_name = ws_name.to_string_lossy();
                worktree::create(&project.path, &workspace_dir, &ws_name).await?
            }
            None => project.path.clone(),
        };

        let crate::devcontainer::Kind::Compose(ref compose) = dc.kind else {
            // This was handled at deserialize time already.
            panic!();
        };

        let config_file = worktree_path
            .join(".devcontainer")
            .join("devcontainer.json");
        let override_file =
            write_compose_override(compose, &dc.common, &worktree_path, &config_file, name)?;

        // Check if the primary container already exists (re-up vs fresh creation)
        let _already_running = compose_ps_q(compose, &worktree_path, &override_file)
            .await
            .is_ok();

        // initializeCommand runs on the host, from the worktree
        if let Some(ref cmd) = dc.common.initialize_command {
            runner::run("initializeCommand", cmd, Some(&worktree_path)).await?;
        }

        if let Some(ref copy_args) = self.copy
            && self.name.is_some()
        {
            let volumes = if !copy_args.is_empty() {
                copy_args.clone()
            } else {
                dc.common
                    .customizations
                    .dc
                    .default_copy_volumes
                    .clone()
                    .ok_or_else(|| {
                        eyre!("no volumes specified and no defaultCopyVolumes configured")
                    })?
            };

            let root_project = compose_project_name(&project.path);
            let new_project = compose_project_name(&worktree_path);

            copy_volumes(docker, &volumes, &root_project, &new_project).await?;
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

/// Generate a compose override file that injects devcontainer labels onto the
/// primary service so that VS Code and other tools can discover the container.
fn write_compose_override(
    compose: &Compose,
    common: &Common,
    worktree_path: &Path,
    config_file: &Path,
    project_name: &str,
) -> eyre::Result<PathBuf> {
    let override_path = std::env::temp_dir().join(format!(
        "{}-override.yml",
        compose_project_name(worktree_path)
    ));
    let local_folder = worktree_path.to_string_lossy();
    let config_file = config_file.to_string_lossy();

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

    std::fs::write(&override_path, content)?;
    Ok(override_path)
}

fn compose_base_args(compose: &Compose, worktree_path: &Path, override_file: &Path) -> Vec<String> {
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
    args.push("-f".into());
    args.push(override_file.to_string_lossy().into_owned());
    args
}

async fn compose_up(
    compose: &Compose,
    worktree_path: &Path,
    override_file: &Path,
) -> eyre::Result<()> {
    let mut args = vec1::vec1!["docker".into()];
    args.extend(compose_base_args(compose, worktree_path, override_file));
    args.extend(["up".into(), "-d".into(), "--build".into()]);

    if let Some(ref services) = compose.run_services {
        let mut to_start: Vec<String> = services.clone();
        if !to_start.contains(&compose.service) {
            to_start.push(compose.service.clone());
        }
        args.extend(to_start);
    }

    let cmd = crate::runner::cmd::Cmd::Args(args);
    runner::run("docker compose up", &cmd, None).await
}

async fn compose_ps_q(
    compose: &Compose,
    worktree_path: &Path,
    override_file: &Path,
) -> eyre::Result<String> {
    let mut args = compose_base_args(compose, worktree_path, override_file);
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

pub(crate) fn exec_interactive(
    container_id: &str,
    user: Option<&str>,
    workdir: Option<&Path>,
    cmd_args: &[String],
    default_cmd: Option<&Cmd>,
) -> eyre::Result<()> {
    let mut args = vec!["exec".to_string(), "-it".into()];
    if let Some(u) = user {
        args.extend(["-u".into(), u.to_string()]);
    }
    if let Some(w) = workdir {
        args.extend(["-w".into(), w.to_string_lossy().into_owned()]);
    }
    args.push(container_id.to_string());

    if cmd_args.is_empty() {
        args.extend(
            default_cmd
                .ok_or_else(|| eyre!("no command provided and no default configured"))?
                .as_args()
                .into_iter()
                .map(ToString::to_string),
        );
    } else {
        args.extend(cmd_args.iter().cloned());
    }

    // Restore cursor visibility — indicatif hides it for spinners and exec()
    // replaces the process before indicatif's cleanup can run.
    let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show);

    Err(std::process::Command::new("docker")
        .args(&args)
        .exec()
        .into())
}
