use clap::Args;
use clap_complete::ArgValueCompleter;
use color_eyre::owo_colors::OwoColorize;
use indexmap::IndexMap;
use tracing::info_span;
use tracing_indicatif::span_ext::IndicatifSpanExt;

use crate::cli::exec::exec_interactive;
use crate::cli::fwd::forward;
use crate::cli::{State, go, proxy};
use crate::complete::complete_workspace;
use crate::config::Config;
use crate::docker::compose::{compose_cmd, compose_ps_q};
use crate::docker::probe;
use crate::run::Runner;
use crate::run::cmd::NamedCmd;
use crate::worktree;

/// Bring up a workspace, creating it if it does not exist
#[derive(Debug, Args)]
pub(crate) struct Up {
    /// Foward configured `forwardPorts` once up
    #[arg(short, long)]
    forward: bool,

    /// Detach worktree rather than creating a branch
    #[arg(short, long)]
    detach: bool,

    /// Specify a branch instead of using the worktree name
    #[arg(short, long)]
    branch: Option<String>,

    /// Navigate to the directory after creating (if using via shell wrapper)
    #[arg(short, long)]
    go: bool,

    /// Workspace name
    #[arg(add = ArgValueCompleter::new(complete_workspace))]
    workspace: Option<String>,

    /// Exec once up with the given command [default: configured defaultExec]
    #[arg(short = 'x', long, num_args = 0.., allow_hyphen_values = true)]
    exec: Option<Vec<String>>,
}

impl Up {
    pub(crate) async fn run(self, project: Option<String>) -> eyre::Result<()> {
        let config = Config::load()?;
        let state = State::new(project, &config).await?;
        let workspace = state.resolve_workspace(self.workspace).await?;

        // Set up span.
        let name = &workspace.name;
        let colored_name = name.cyan().to_string();
        let up = "up".cyan().to_string();
        let path = workspace.path.display().to_string();
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

        if !workspace.is_root {
            worktree::create(&workspace, self.detach, self.branch.as_deref()).await?;
        }

        if !state.has_devcontainer() {
            // If there's no devcontainer, then the only thing to do is create the worktree.
            return Ok(());
        }
        let devcontainer = state.devcontainer_for(&workspace.path)?;
        let devcontainer = &devcontainer;

        // initializeCommand runs on the host, from the worktree, before any
        // container exists — so `${containerEnv:…}` is an error here.
        if let Some(cmd) = &devcontainer.config.initialize_command {
            let context = devcontainer.context(&workspace.path);
            cmd.render("initializeCommand", &context)?
                .run_on_host("initializeCommand", Some(&workspace.path))
                .await?;
        }

        // If proxy is configured for this project, make sure the proxy
        // container is running before compose-up so it can react to start
        // events.
        if devcontainer.proxy_enabled() {
            let project = workspace.state.project_name.clone();
            let proxy =
                proxy::ProxyState::from_workspace(&config, project, Some(&workspace)).await?;
            proxy::ensure_up(proxy).await?;
        }

        let mut compose_up_cmd = compose_cmd(devcontainer, &workspace)?;
        compose_up_cmd.args(["up", "-d", "--build", "--remove-orphans"]);

        if let Some(ref services) = devcontainer.config.run_services {
            compose_up_cmd.args(services);
            if !services.contains(&devcontainer.config.service) {
                // TODO: We probably want this in the `else` also, or maybe we
                // don't need it at all?
                compose_up_cmd.arg(&devcontainer.config.service);
            }
        }

        let up_cmd = compose_up_cmd.into_std().into();
        let cmd = NamedCmd {
            name: "docker compose up",
            cmd: &up_cmd,
            dir: None,
        };
        Runner::run(cmd).await?;

        let container_id = compose_ps_q(devcontainer, &workspace).await?;
        let workdir = Some(devcontainer.workspace_folder.as_path());

        let container =
            probe::ContainerData::inspect(&devcontainer.docker().await?.client, &container_id)
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
        let user = user.as_deref();
        let probed = probe::user_env(
            &container_id,
            user,
            &container.env,
            devcontainer.config.user_env_probe,
        )
        .await?;
        // Spec merge order: probed env is the base; devcontainer.json `remoteEnv` overlays.
        // A `None` (spec `null`) emits `-e KEY=` (empty) downstream.
        let mut merged: IndexMap<String, Option<String>> =
            probed.into_iter().map(|(k, v)| (k, Some(v))).collect();
        for (key, template) in &devcontainer.config.remote_env {
            let value = template
                .as_ref()
                .map(|t| context.render_field(&format!("remoteEnv.{key}"), t))
                .transpose()?;
            merged.insert(key.clone(), value);
        }
        let remote_env = &merged;

        // Lifecycle commands: create-only commands run only on first creation
        // For now, though, we always recreate.
        for (name, cmd) in [
            ("onCreateCommand", &devcontainer.config.on_create_command),
            (
                "updateContentCommand",
                &devcontainer.config.update_content_command,
            ),
            (
                "postCreateCommand",
                &devcontainer.config.post_create_command,
            ),
            ("postStartCommand", &devcontainer.config.post_start_command),
        ] {
            let Some(cmd) = cmd else { continue };
            cmd.render(name, &context)?
                .run_in_container(name, &container_id, user, workdir, remote_env)
                .await?;
        }

        // Port forward if requested
        if self.forward {
            forward(devcontainer, &workspace).await?;
        }

        // Interactive exec if requested
        if let Some(cmd_args) = self.exec {
            let default_exec = devcontainer
                .devconcurrent()
                .default_exec
                .as_ref()
                .map(|cmd| cmd.render("defaultExec", &context))
                .transpose()?;
            exec_interactive(
                &container_id,
                devcontainer,
                remote_env,
                &cmd_args,
                user,
                default_exec.as_ref(),
            )?;
        }

        if self.go {
            go::go(&workspace.path)?;
        }

        Ok(())
    }
}
