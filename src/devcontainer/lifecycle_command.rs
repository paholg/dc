use std::path::Path;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::run::Runner;
use crate::run::cmd::{Cmd, NamedCmd};
use crate::run::docker_exec::DockerExec;

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum LifecycleCommand {
    Single(Cmd),
    Parallel(IndexMap<String, Cmd>),
}

impl LifecycleCommand {
    pub async fn run_on_host(&self, name: &str, dir: Option<&Path>) -> eyre::Result<()> {
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

    pub async fn run_in_container(
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
