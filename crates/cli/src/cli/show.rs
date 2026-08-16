use clap::{Args, Subcommand};
use clap_complete::Shell;
use eyre::WrapErr;
use itertools::Itertools;

use crate::{
    cli::{State, fwd},
    config::Config,
    docker::compose,
    helpers,
    table::{Align, ColumnDef, TableBuilder, text},
};

/// Show some value
#[derive(Debug, Args)]
pub(crate) struct Show {
    #[command(subcommand)]
    command: ShowCommands,
}

#[derive(Debug, Subcommand)]
enum ShowCommands {
    /// Show currently-forwarded ports for this workspace
    Ports(Ports),
    /// Print the current workspace name, or exit 1
    Workspace(ShowWorkspace),
    /// Show container IP addresses for this workspace
    Ip(Ip),
    /// Show proxied hostnames for this workspace
    Hostname(Hostname),
    /// Show this workspace's configured shell variables
    Env(Env),
}

#[derive(Debug, Args)]
struct Ports;

#[derive(Debug, Args)]
struct ShowWorkspace;

#[derive(Debug, Args)]
struct Ip {
    /// Compose service name; if omitted, list all services for this workspace
    service: Option<String>,
}

#[derive(Debug, Args)]
struct Hostname {
    /// Compose service name; if omitted, list every service in this
    /// workspace's compose configuration
    service: Option<String>,
}

#[derive(Debug, Args)]
struct Env {
    /// Set the variables in the calling shell, instead of printing a table.
    ///
    /// With the `dc` shell function sourced, this takes effect directly.
    /// Otherwise the assignments go to stdout for you to `eval`.
    #[arg(long)]
    export: bool,

    /// Shell dialect for `--export`.
    ///
    /// When called from `dc`, defaults to your shell.
    #[arg(long, requires = "export")]
    shell: Option<Shell>,
}

impl Show {
    pub(crate) async fn run(self, project: Option<String>) -> eyre::Result<()> {
        let config = Config::load()?;
        let state = State::new(project, &config).await?;
        match self.command {
            ShowCommands::Ports(ports) => ports.run(state).await,
            ShowCommands::Workspace(ws) => ws.run(state).await,
            ShowCommands::Ip(ip) => ip.run(state).await,
            ShowCommands::Hostname(hostname) => hostname.run(state).await,
            ShowCommands::Env(env) => env.run(state).await,
        }
    }
}

impl Ports {
    async fn run(self, state: State<'_>) -> eyre::Result<()> {
        let ports = get_ports(state).await?;

        println!("{ports}");
        Ok(())
    }
}

async fn get_ports(state: State<'_>) -> eyre::Result<String> {
    let workspace = state.resolve_workspace(None).await?;
    let devcontainer = state.try_devcontainer()?;
    let (ports, healthy) = tokio::join!(
        devcontainer.docker.workspace_forwarded_ports(&workspace),
        devcontainer.docker.is_forwarding_healthy(&workspace),
    );
    let ports = ports?;

    if !ports.is_empty() && !healthy? {
        fwd::remove_sidecars(&state, &devcontainer.docker.client).await?;
        Ok(String::new())
    } else {
        Ok(ports.into_iter().join(","))
    }
}

impl ShowWorkspace {
    async fn run(self, state: State<'_>) -> eyre::Result<()> {
        match state.resolve_workspace(None).await {
            Ok(workspace) => {
                println!("{}", workspace.name);
                Ok(())
            }
            Err(_) => std::process::exit(1),
        }
    }
}

impl Ip {
    async fn run(self, state: State<'_>) -> eyre::Result<()> {
        let devcontainer = state.try_devcontainer()?;
        let workspace = state.resolve_workspace(None).await?;
        let ips = devcontainer
            .docker
            .workspace_compose_ips(&workspace)
            .await?;

        if let Some(service) = self.service {
            let ip = ips.iter().find(|(s, _)| s == &service).ok_or_else(|| {
                eyre::eyre!(
                    "no service '{service}' with an IP address in workspace '{}'",
                    workspace.name
                )
            })?;
            println!("{}", ip.1);
        } else {
            print!("{}", pair_table("SERVICE", "IP", &ips));
        }
        Ok(())
    }
}

impl Hostname {
    async fn run(self, state: State<'_>) -> eyre::Result<()> {
        let devcontainer = state.try_devcontainer()?;
        let workspace = state.resolve_workspace(None).await?;
        let proxy = &devcontainer.devconcurrent().proxy;

        let services = compose::compose_services(devcontainer, &workspace).await?;

        let hostname = |service: &str| {
            proxy
                .render_hostname(
                    &state.project_name,
                    &workspace.name,
                    service,
                    workspace.is_root,
                )
                .ok_or_else(|| {
                    eyre::eyre!("could not render the hostname template for service '{service}'")
                })
        };

        if let Some(service) = self.service {
            eyre::ensure!(
                services.contains(&service),
                "no compose service '{service}' in workspace '{}'; found {}",
                workspace.name,
                services.join(", ")
            );
            println!("{}", hostname(&service)?);
            return Ok(());
        }

        let rows = services
            .into_iter()
            .map(|service| {
                let rendered = hostname(&service)?;
                Ok((service, rendered))
            })
            .collect::<eyre::Result<Vec<_>>>()?;

        print!("{}", pair_table("SERVICE", "HOSTNAME", &rows));
        Ok(())
    }
}

impl Env {
    async fn run(self, state: State<'_>) -> eyre::Result<()> {
        let devcontainer = state.try_devcontainer()?;
        let workspace = state.resolve_workspace(None).await?;
        let options = devcontainer.devconcurrent();

        let vars = options
            .env
            .iter()
            .map(|(name, template)| {
                let value = options
                    .proxy
                    .render_env_value(
                        &state.project_name,
                        &workspace.name,
                        workspace.is_root,
                        template,
                    )
                    .wrap_err_with(|| {
                        format!("rendering customizations.devconcurrent.env.{name}")
                    })?;

                Ok((name.as_str().to_string(), value))
            })
            .collect::<eyre::Result<Vec<_>>>()?;

        if !self.export {
            if vars.is_empty() {
                eprintln!("no variables configured under customizations.devconcurrent.env");
                return Ok(());
            }

            print!("{}", pair_table("VARIABLE", "VALUE", &vars));

            return Ok(());
        }

        if vars.is_empty() {
            return Ok(());
        }

        let shell = self.shell.or_else(helpers::calling_shell).ok_or_else(|| {
            eyre::eyre!(
                "could not tell which shell to write for; pass --shell, or source the `dc` \
                 shell function (see the README)"
            )
        })?;
        let script = vars
            .iter()
            .map(|(name, value)| assignment(shell, name, value))
            .collect::<eyre::Result<Vec<_>>>()?
            .join("\n");

        helpers::forward_to_shell(&script)
    }
}

/// One assignment of `value` to `name`, in `shell`'s dialect.
fn assignment(shell: Shell, name: &str, value: &str) -> eyre::Result<String> {
    let quoted = shlex::try_quote(value)?;
    Ok(match shell {
        Shell::Bash | Shell::Zsh => format!("export {name}={quoted}"),
        Shell::Fish => format!("set -gx {name} {quoted}"),
        shell => eyre::bail!(
            "cannot write variable assignments for {shell}; --export supports bash, zsh and fish"
        ),
    })
}

/// A two-column table of already-known values, rendered like the rest of our
/// tables.
fn pair_table(left: &'static str, right: &'static str, rows: &[(String, String)]) -> String {
    [
        ColumnDef::new(left, Align::Left, |r: &(String, String)| text(r.0.clone())),
        ColumnDef::new(right, Align::Left, |r: &(String, String)| text(r.1.clone())),
    ]
    .into_iter()
    .collect::<TableBuilder<(String, String)>>()
    .build(rows, false)
    .rendered()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_each_shells_assignment_syntax() {
        assert_eq!(
            assignment(Shell::Bash, "APP_HOST", "feature.app.test").unwrap(),
            "export APP_HOST=feature.app.test"
        );
        assert_eq!(
            assignment(Shell::Zsh, "APP_HOST", "feature.app.test").unwrap(),
            "export APP_HOST=feature.app.test"
        );
        assert_eq!(
            assignment(Shell::Fish, "APP_HOST", "feature.app.test").unwrap(),
            "set -gx APP_HOST feature.app.test"
        );
    }

    /// The output is `eval`ed, so anything the shell would act on has to be
    /// quoted away. `shlex` chooses the quoting style per chunk, and is
    /// careful to keep `$` and backticks out of double quotes.
    #[test]
    fn quotes_values_the_shell_would_otherwise_interpret() {
        let nasty = "postgres://u:it's $PW`x`@host/db";
        assert_eq!(
            assignment(Shell::Bash, "URL", nasty).unwrap(),
            r#"export URL="postgres://u:it's "'$PW`x`@host/db'"#
        );
        assert_eq!(
            assignment(Shell::Fish, "URL", nasty).unwrap(),
            r#"set -gx URL "postgres://u:it's "'$PW`x`@host/db'"#
        );
    }

    /// The quoting above only matters if a real shell agrees.
    #[test]
    fn bash_evals_the_assignment_back_to_the_original_value() {
        let nasty = "postgres://u:it's $PW`x`@host/db";
        let line = assignment(Shell::Bash, "URL", nasty).unwrap();

        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!("{line}; printf %s \"$URL\""))
            .output()
            .expect("bash is available");

        assert!(out.status.success(), "{out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), nasty);
    }

    /// Nothing to quote, so the value should stay readable.
    #[test]
    fn leaves_ordinary_values_bare() {
        assert_eq!(
            assignment(
                Shell::Bash,
                "DATABASE_URL",
                "postgres://feature.db.test:5432/db"
            )
            .unwrap(),
            "export DATABASE_URL=postgres://feature.db.test:5432/db"
        );
    }

    #[test]
    fn rejects_shells_it_cannot_write_for() {
        assert!(assignment(Shell::PowerShell, "A", "b").is_err());
    }
}
