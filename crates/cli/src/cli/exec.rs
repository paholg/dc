use std::io::IsTerminal;
use std::os::unix::process::CommandExt;

use clap::Args;
use clap_complete::ArgValueCompleter;
use docker::ContainerStatus;
use eyre::eyre;
use indexmap::IndexMap;

use crate::cli::State;
use crate::complete::complete_workspace;
use crate::config::Config;
use crate::docker::probe;
use crate::state::DevcontainerState;

/// Exec into a running devcontainer
#[derive(Debug, Args)]
pub(crate) struct Exec {
    /// Workspace name [default: current working directory]
    #[arg(short, long, add = ArgValueCompleter::new(complete_workspace))]
    workspace: Option<String>,

    /// command to run [default: the container user's shell]
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    cmd: Vec<String>,
}

impl Exec {
    pub(crate) async fn run(self, project: Option<String>) -> eyre::Result<()> {
        let config = Config::load()?;
        let state = State::new(project, &config).await?;
        let workspace = state.resolve_workspace(self.workspace).await?;
        let devcontainer = state.devcontainer_for(&workspace.path)?;
        let devcontainer = &devcontainer;
        let workspace_full = workspace.devcontainer(devcontainer).await?;
        if workspace_full.status() != Some(ContainerStatus::Running) {
            return Err(eyre!(
                "workspace is not running: {}",
                workspace.path.display()
            ));
        }
        let container_id = workspace_full.service_container_id()?;
        let container =
            probe::ContainerData::inspect(&devcontainer.docker().await?.client, container_id)
                .await?;
        let context = devcontainer
            .context(&workspace.path)
            .with_container_env(&container.env);
        let user = devcontainer
            .config
            .remote_user
            .as_ref()
            .map(|user| context.render_field("remoteUser", user))
            .transpose()?;
        let probed = probe::user_env(
            container_id,
            user.as_deref(),
            &container.env,
            devcontainer.config.user_env_probe,
        )
        .await?;
        let mut remote_env: IndexMap<String, Option<String>> =
            probed.into_iter().map(|(k, v)| (k, Some(v))).collect();
        for (key, template) in &devcontainer.config.remote_env {
            let value = template
                .as_ref()
                .map(|t| context.render_field(&format!("remoteEnv.{key}"), t))
                .transpose()?;
            remote_env.insert(key.clone(), value);
        }

        exec_interactive(
            container_id,
            devcontainer,
            &remote_env,
            &self.cmd,
            user.as_deref(),
        )
        .await
    }
}

/// `user` is the rendered `remoteUser`. With no command on the CLI, we run the
/// container user's shell.
pub(crate) async fn exec_interactive(
    container_id: &str,
    devcontainer: &DevcontainerState,
    remote_env: &IndexMap<String, Option<String>>,
    cmd_args: &[String],
    user: Option<&str>,
) -> eyre::Result<()> {
    let mut cmd = std::process::Command::new("docker");
    cmd.arg("exec");
    if std::io::stdin().is_terminal() {
        cmd.arg("-it");
    }

    if let Some(u) = user {
        cmd.args(["-u", u]);
    }
    cmd.arg("-w").arg(&devcontainer.workspace_folder);

    for (k, v) in remote_env {
        // null in remoteEnv means "unset" per spec; we can't truly unset PID-1-inherited vars via
        // `docker exec`, so set to empty string — closer to intent than the reference's literal
        // "null" stringification.
        cmd.arg("-e")
            .arg(format!("{k}={}", v.as_deref().unwrap_or("")));
    }

    cmd.arg(container_id);

    if cmd_args.is_empty() {
        // The probe usually hands us `SHELL` already; only ask the container when it doesn't.
        let default_shell = match remote_env.get("SHELL").and_then(Option::as_deref) {
            Some(shell) if !shell.is_empty() => shell.to_string(),
            _ => probe::resolve_user_shell(container_id, user).await?,
        };
        cmd.arg(default_shell);
    } else {
        cmd.args(cmd_args);
    }

    // Restore cursor visibility — indicatif hides it for spinners and exec()
    // replaces the process before indicatif's cleanup can run.
    let _ = crossterm::execute!(std::io::stderr(), crossterm::cursor::Show);

    Err(cmd.exec().into())
}
