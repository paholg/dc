use std::path::Path;

use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::devcontainer::substitution::Context;
use crate::run::Runner;
use crate::run::cmd::{Cmd, CmdTemplate, NamedCmd};
use crate::run::docker_exec::DockerExec;

/// A lifecycle command as written in config, before `${...}` substitution.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(untagged)]
pub(crate) enum LifecycleCommandTemplate {
    Single(CmdTemplate),
    Parallel(IndexMap<String, CmdTemplate>),
}

impl LifecycleCommandTemplate {
    /// `field` is the property name, e.g. `postCreateCommand`.
    pub(crate) fn render(
        &self,
        field: &str,
        context: &Context<'_>,
    ) -> eyre::Result<LifecycleCommand> {
        match self {
            LifecycleCommandTemplate::Single(cmd) => {
                Ok(LifecycleCommand::Single(cmd.render(field, context)?))
            }
            LifecycleCommandTemplate::Parallel(map) => Ok(LifecycleCommand::Parallel(
                map.iter()
                    .map(|(name, cmd)| {
                        Ok((
                            name.clone(),
                            cmd.render(&format!("{field}.{name}"), context)?,
                        ))
                    })
                    .collect::<eyre::Result<_>>()?,
            )),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum LifecycleCommand {
    Single(Cmd),
    Parallel(IndexMap<String, Cmd>),
}

impl LifecycleCommand {
    pub(crate) async fn run_on_host(&self, name: &str, dir: Option<&Path>) -> eyre::Result<()> {
        match self {
            LifecycleCommand::Single(cmd) => {
                let cmd = NamedCmd { name, cmd, dir };
                Runner::run(cmd).await
            }
            LifecycleCommand::Parallel(map) => {
                let execs = map.iter().map(|(cmd_name, cmd)| NamedCmd {
                    name: cmd_name,
                    cmd,
                    dir,
                });

                Runner::run_parallel(name, execs).await
            }
        }
    }

    pub(crate) async fn run_in_container(
        &self,
        name: &str,
        container: &str,
        user: Option<&str>,
        workdir: Option<&Path>,
        env: &IndexMap<String, Option<String>>,
    ) -> eyre::Result<()> {
        match self {
            LifecycleCommand::Single(cmd) => {
                let exec = DockerExec {
                    name,
                    container,
                    cmd,
                    user,
                    workdir,
                    env,
                };
                Runner::run(exec).await
            }
            LifecycleCommand::Parallel(map) => {
                let execs = map.iter().map(|(cmd_name, cmd)| DockerExec {
                    name: cmd_name,
                    container,
                    cmd,
                    user,
                    workdir,
                    env,
                });

                Runner::run_parallel(name, execs).await
            }
        }
    }
}
