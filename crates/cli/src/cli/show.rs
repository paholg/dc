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
    /// Set the variables in the calling shell, in this shell's syntax, instead
    /// of printing a table.
    ///
    /// With the `dc` shell function sourced, this takes effect directly.
    /// Otherwise the assignments go to stdout for you to `eval`. Setting
    /// `shell.exportEnv` in config.toml wires this up on every prompt for you.
    #[arg(long, value_name = "SHELL")]
    export: Option<Shell>,
}

impl Show {
    pub(crate) async fn run(self, project: Option<String>) -> eyre::Result<()> {
        // `env` builds its own state: with `--export` it runs from a shell
        // prompt, where standing outside a project is ordinary rather than an
        // error.
        let command = match self.command {
            ShowCommands::Env(env) => return env.run(project).await,
            command => command,
        };

        let config = Config::load()?;
        let state = State::new(project, &config).await?;
        match command {
            ShowCommands::Ports(ports) => ports.run(state).await,
            ShowCommands::Workspace(ws) => ws.run(state).await,
            ShowCommands::Ip(ip) => ip.run(state).await,
            ShowCommands::Hostname(hostname) => hostname.run(state).await,
            ShowCommands::Env(_) => unreachable!("returned above"),
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
    let docker = devcontainer.docker().await?;
    let (ports, healthy) = tokio::join!(
        docker.workspace_forwarded_ports(&workspace),
        docker.is_forwarding_healthy(&workspace),
    );
    let ports = ports?;

    if !ports.is_empty() && !healthy? {
        fwd::remove_sidecars(&state, &docker.client).await?;
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
            .docker()
            .await?
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
    async fn run(self, project: Option<String>) -> eyre::Result<()> {
        let vars = gather(project).await?;

        let Some(shell) = self.export else {
            match vars {
                None => eyre::bail!("not inside a workspace"),
                Some(vars) if vars.is_empty() => {
                    eprintln!("no variables configured under customizations.devconcurrent.env");
                }
                Some(vars) => print!("{}", pair_table("VARIABLE", "VALUE", &vars)),
            }

            return Ok(());
        };

        // Outside a workspace there is nothing to set, and clearing whatever we
        // set in the workspace we just left is exactly right.
        let vars = vars.unwrap_or_default();

        let previous = std::env::var(helpers::EXPORTED_ENV).unwrap_or_default();
        let script = export_script(Dialect::of(shell)?, &previous, &vars)?;
        if script.is_empty() {
            return Ok(());
        }

        helpers::forward_to_shell(&script)
    }
}

/// Render every configured variable for the current workspace, or `None` if we
/// aren't standing in one.
///
/// The prompt hook runs on every prompt, from anywhere, so "no project here" and
/// "no workspace here" are ordinary answers rather than failures. A config or
/// template that doesn't work still is one — noticing that is the point.
async fn gather(project: Option<String>) -> eyre::Result<Option<Vec<(String, String)>>> {
    let config = Config::load()?;
    let Ok(state) = State::new(project, &config).await else {
        return Ok(None);
    };
    let Ok(devcontainer) = state.try_devcontainer() else {
        return Ok(None);
    };
    let Ok(workspace) = state.resolve_workspace(None).await else {
        return Ok(None);
    };
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
                .wrap_err_with(|| format!("rendering customizations.devconcurrent.env.{name}"))?;

            Ok((name.as_str().to_string(), value))
        })
        .collect::<eyre::Result<Vec<_>>>()?;

    Ok(Some(vars))
}

/// The shell script that moves the calling shell from `previous` — the value
/// of [`helpers::EXPORTED_ENV`], naming what we set last time — to `vars`.
fn export_script(
    dialect: Dialect,
    previous: &str,
    vars: &[(String, String)],
) -> eyre::Result<String> {
    let mut lines: Vec<String> = previous
        .split_whitespace()
        .filter(|name| !vars.iter().any(|(n, _)| n == name))
        .map(|name| dialect.unset(name))
        .collect();

    for (name, value) in vars {
        lines.push(dialect.assign(name, value)?);
    }

    let names = vars.iter().map(|(name, _)| name.as_str()).join(" ");
    if names.is_empty() {
        if !previous.is_empty() {
            lines.push(dialect.unset(helpers::EXPORTED_ENV));
        }
    } else if names != previous {
        lines.push(dialect.assign(helpers::EXPORTED_ENV, &names)?);
    }

    Ok(lines.join("\n"))
}

/// How to spell a variable assignment. The shells we support fall into two
/// camps, and resolving which one up front keeps the "unsupported shell" error
/// out of every line we write.
#[derive(Clone, Copy, Debug)]
enum Dialect {
    Posix,
    Fish,
}

impl Dialect {
    fn of(shell: Shell) -> eyre::Result<Self> {
        match shell {
            Shell::Bash | Shell::Zsh => Ok(Self::Posix),
            Shell::Fish => Ok(Self::Fish),
            shell => eyre::bail!(
                "cannot write variable assignments for {shell}; --export supports bash, zsh and fish"
            ),
        }
    }

    /// Export `value` as `name`. The result is `eval`ed by the calling shell,
    /// so the value is quoted; `shlex` handles both camps, and is careful to
    /// keep `$` and backticks out of double quotes for fish's sake.
    fn assign(self, name: &str, value: &str) -> eyre::Result<String> {
        let quoted = shlex::try_quote(value)?;
        Ok(match self {
            Self::Posix => format!("export {name}={quoted}"),
            Self::Fish => format!("set -gx {name} {quoted}"),
        })
    }

    fn unset(self, name: &str) -> String {
        match self {
            Self::Posix => format!("unset {name}"),
            Self::Fish => format!("set -e {name}"),
        }
    }
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

    fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, v)| ((*n).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn writes_each_shells_assignment_syntax() {
        assert_eq!(
            Dialect::of(Shell::Bash)
                .unwrap()
                .assign("APP_HOST", "feature.app.test")
                .unwrap(),
            "export APP_HOST=feature.app.test"
        );
        assert_eq!(
            Dialect::of(Shell::Zsh)
                .unwrap()
                .assign("APP_HOST", "feature.app.test")
                .unwrap(),
            "export APP_HOST=feature.app.test"
        );
        assert_eq!(
            Dialect::of(Shell::Fish)
                .unwrap()
                .assign("APP_HOST", "feature.app.test")
                .unwrap(),
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
            Dialect::Posix.assign("URL", nasty).unwrap(),
            r#"export URL="postgres://u:it's "'$PW`x`@host/db'"#
        );
        assert_eq!(
            Dialect::Fish.assign("URL", nasty).unwrap(),
            r#"set -gx URL "postgres://u:it's "'$PW`x`@host/db'"#
        );
    }

    /// The quoting above only matters if a real shell agrees.
    #[test]
    fn bash_evals_the_assignment_back_to_the_original_value() {
        let nasty = "postgres://u:it's $PW`x`@host/db";
        let line = Dialect::Posix.assign("URL", nasty).unwrap();

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
            Dialect::Posix
                .assign("DATABASE_URL", "postgres://feature.db.test:5432/db")
                .unwrap(),
            "export DATABASE_URL=postgres://feature.db.test:5432/db"
        );
    }

    #[test]
    fn rejects_shells_it_cannot_write_for() {
        assert!(Dialect::of(Shell::PowerShell).is_err());
        assert!(Dialect::of(Shell::Elvish).is_err());
    }

    #[test]
    fn first_run_exports_the_variables_and_records_their_names() {
        let script = export_script(Dialect::Posix, "", &vars(&[("A", "1"), ("B", "2")])).unwrap();
        assert_eq!(
            script,
            "export A=1\nexport B=2\nexport DEVCONCURRENT_ENV='A B'"
        );
    }

    /// Same variables as last time, so there is nothing to unset and the
    /// bookkeeping variable doesn't need rewriting.
    #[test]
    fn steady_state_only_refreshes_the_values() {
        let script =
            export_script(Dialect::Posix, "A B", &vars(&[("A", "9"), ("B", "8")])).unwrap();
        assert_eq!(script, "export A=9\nexport B=8");
    }

    /// Dropping a variable from the config has to clear the stale value.
    #[test]
    fn a_removed_variable_is_unset() {
        let script = export_script(Dialect::Posix, "A B", &vars(&[("A", "1")])).unwrap();
        assert_eq!(script, "unset B\nexport A=1\nexport DEVCONCURRENT_ENV=A");
    }

    /// Leaving the workspace: no variables to set, so clear everything,
    /// bookkeeping included.
    #[test]
    fn leaving_a_workspace_clears_everything() {
        assert_eq!(
            export_script(Dialect::Posix, "A B", &[]).unwrap(),
            "unset A\nunset B\nunset DEVCONCURRENT_ENV"
        );
        assert_eq!(
            export_script(Dialect::Fish, "A B", &[]).unwrap(),
            "set -e A\nset -e B\nset -e DEVCONCURRENT_ENV"
        );
    }

    /// Still outside a workspace on the next prompt — nothing left to say.
    #[test]
    fn staying_outside_a_workspace_writes_nothing() {
        assert_eq!(export_script(Dialect::Posix, "", &[]).unwrap(), "");
    }
}
