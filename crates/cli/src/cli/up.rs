use std::collections::BTreeSet;

use clap::Args;
use clap_complete::ArgValueCompleter;
use color_eyre::owo_colors::OwoColorize;
use eyre::WrapErr;
use indexmap::IndexMap;
use tracing::info_span;
use tracing_indicatif::span_ext::IndicatifSpanExt;

use docker::MANAGED_LABEL;

use crate::cli::exec::exec_interactive;
use crate::cli::fwd::forward;
use crate::cli::{State, go, proxy};
use crate::complete::complete_workspace;
use crate::config::Config;
use crate::devcontainer::substitution;
use crate::docker::compose::{self, compose_cmd, compose_image, compose_ps_q, compose_pull};
use crate::docker::{probe, uid};
use crate::run::cmd::NamedCmd;
use crate::run::{self, Runner};
use crate::state::DevcontainerState;
use crate::workspace::Workspace;
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

    /// Exec once up with the given command [default: the container user's shell]
    #[arg(short = 'x', long, num_args = 0.., allow_hyphen_values = true)]
    exec: Option<Vec<String>>,
}

impl Up {
    pub(crate) async fn run(self, project: Option<String>) -> eyre::Result<()> {
        let config = Config::load()?;
        let state = State::new(project, &config).await?;
        let workspace = state.resolve_workspace(self.workspace.clone()).await?;

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
            finish_message,
            failed = tracing::field::Empty,
        );
        span.pb_set_message(&pb_message);
        let _guard = span.enter();

        let brought_up: eyre::Result<()> = async {
            if !workspace.is_root {
                worktree::create(&workspace, self.detach, self.branch.as_deref()).await?;
            }

            if state.has_devcontainer() {
                self.up_devcontainer(&config, &state, &workspace).await?;
            }

            if self.go {
                go::go(&workspace.path)?;
            }

            Ok(())
        }
        .await;

        // Name the workspace in the error itself: an error gets pasted around
        // without the log line above it that says which one we were bringing up.
        let result = brought_up.wrap_err_with(|| format!("workspace: {}", workspace.name));

        if result.is_err() {
            run::mark_failed(&span);
        }

        result
    }

    async fn up_devcontainer(
        &self,
        config: &Config,
        state: &State<'_>,
        workspace: &Workspace<'_>,
    ) -> eyre::Result<()> {
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

            // Catch a hostname template that renders outside proxy.tlds now,
            // rather than as a DNS miss or certificate error in the browser.
            let opts = &devcontainer.devconcurrent().proxy;
            let mut services: BTreeSet<&str> = opts.services.keys().map(String::as_str).collect();
            services.insert(devcontainer.config.service.as_str());
            if let Some(run) = &devcontainer.config.run_services {
                services.extend(run.iter().map(String::as_str));
            }
            proxy::check_hostname_tlds(
                opts,
                project.as_str(),
                &workspace.name,
                workspace.is_root,
                services,
                &config.proxy.tlds,
            )?;

            let proxy = proxy::ProxyState::from_workspace(config, project, Some(workspace)).await?;
            proxy::ensure_up(proxy).await?;
        }

        let project_name = compose::project_name(devcontainer, workspace).await?;
        compose::ensure_project_unclaimed(devcontainer, workspace, project_name).await?;

        // Build first, separately from `up`: the uid remap layers onto the
        // service's built image, so the image has to exist before the override
        // that pins it can be written.
        let mut compose_build_cmd = compose_cmd(devcontainer, workspace).await?;
        compose_build_cmd.arg("build");
        self.add_services(devcontainer, &mut compose_build_cmd);
        let build_cmd = compose_build_cmd.into_std().into();
        Runner::run(NamedCmd {
            name: "docker compose build",
            cmd: &build_cmd,
            dir: None,
            // Attestations off, so a fully-cached rebuild keeps its image ID
            // and the uid layer and `compose up` see an unchanged image.
            env: crate::docker::build_env(),
        })
        .await?;

        self.update_remote_user_uid(devcontainer, workspace, project_name)
            .await?;

        let mut compose_up_cmd = compose_cmd(devcontainer, workspace).await?;
        // No `--build`: the build above already ran, and rebuilding here would
        // re-tag the service's own Dockerfile output over the pinned image.
        compose_up_cmd.args(["up", "-d", "--remove-orphans"]);
        self.add_services(devcontainer, &mut compose_up_cmd);

        let up_cmd = compose_up_cmd.into_std().into();
        let cmd = NamedCmd {
            name: "docker compose up",
            cmd: &up_cmd,
            dir: None,
            env: &[],
        };
        Runner::run(cmd).await?;

        let container_id = compose_ps_q(devcontainer, workspace).await?;
        let workdir = Some(devcontainer.workspace_folder.as_path());

        let container =
            probe::ContainerData::inspect(&devcontainer.docker().await?.client, &container_id)
                .await?;
        let context = devcontainer
            .context(&workspace.path)
            .with_container_env(&container.env);
        let user = render_remote_user(devcontainer, &context)?;
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
            (
                "postAttachCommand",
                &devcontainer.config.post_attach_command,
            ),
        ] {
            let Some(cmd) = cmd else { continue };
            cmd.render(name, &context)?
                .run_in_container(name, &container_id, user, workdir, remote_env)
                .await
                // The container id alone doesn't say which compose service to go
                // poke at once the hook has failed.
                .wrap_err_with(|| format!("service: {}", devcontainer.config.service))?;
        }

        // Port forward if requested
        if self.forward {
            forward(devcontainer, workspace).await?;
        }

        // Interactive exec if requested
        if let Some(cmd_args) = &self.exec {
            exec_interactive(&container_id, devcontainer, remote_env, cmd_args, user).await?;
        }

        Ok(())
    }

    /// Append the services `up`/`build` should act on.
    fn add_services(&self, devcontainer: &DevcontainerState, cmd: &mut tokio::process::Command) {
        if let Some(ref services) = devcontainer.config.run_services {
            cmd.args(services);
            if !services.contains(&devcontainer.config.service) {
                // Ensure the primary service is included.
                cmd.arg(&devcontainer.config.service);
            }
        }
    }

    /// Apply `updateRemoteUserUID`, recording the image it builds so that
    /// later `compose_cmd` calls pin the service to it.
    async fn update_remote_user_uid(
        &self,
        devcontainer: &DevcontainerState,
        workspace: &Workspace<'_>,
        project_name: &str,
    ) -> eyre::Result<()> {
        let base_image = compose_image(devcontainer, workspace).await?;
        let client = &devcontainer.docker().await?.client;

        let details = match client.inspect_image(&base_image).await {
            Ok(details) => details,
            // A service that only names an image has nothing for `compose
            // build` to do, so the image may not be here yet — `up` used to be
            // what fetched it. Pull through compose rather than the API so the
            // user's registry credentials apply.
            Err(docker::Error::NotFound { .. }) => {
                compose_pull(devcontainer, workspace).await?;
                client
                    .inspect_image(&base_image)
                    .await
                    .wrap_err_with(|| format!("failed to inspect image {base_image}"))?
            }
            Err(e) => {
                return Err(e).wrap_err_with(|| format!("failed to inspect image {base_image}"));
            }
        };

        // The container doesn't exist yet, so `${containerEnv:…}` resolves
        // against the image's environment — which is what the container will
        // inherit anyway.
        let image_env = details.config.parsed_env();
        let context = devcontainer
            .context(&workspace.path)
            .with_container_env(&image_env);
        let remote_user = render_remote_user(devcontainer, &context)?;
        let container_user = devcontainer
            .config
            .container_user
            .as_ref()
            .map(|user| context.render_field("containerUser", user))
            .transpose()?;

        let base = uid::BaseImage {
            user: &details.config.user,
            platform: details.platform(),
        };
        let Some(update) = uid::plan(
            &devcontainer.config,
            &base,
            uid::derived_image_name(project_name, &devcontainer.config.service),
            container_user.as_deref(),
            remote_user.as_deref(),
            uid::host_ids(),
        ) else {
            return Ok(());
        };

        // The same labels the compose override puts on the service, so
        // `destroy` finds the image with the query it already runs for
        // containers.
        let labels = [
            (MANAGED_LABEL, "true"),
            workspace.project_label(),
            workspace.workspace_label(),
        ];
        uid::build(
            client,
            &update,
            &base_image,
            workspace.state.project_working_dir(),
            &labels,
        )
        .await?;

        devcontainer
            .derived_image
            .set(update.fixed_image)
            .map_err(|_| eyre::eyre!("the updateRemoteUserUID image was already set"))?;
        Ok(())
    }
}

fn render_remote_user(
    devcontainer: &DevcontainerState,
    context: &substitution::Context<'_>,
) -> eyre::Result<Option<String>> {
    devcontainer
        .config
        .remote_user
        .as_ref()
        .map(|user| context.render_field("remoteUser", user))
        .transpose()
}
