use std::borrow::Cow;
use std::path::Path;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::runner::Runnable;
use crate::runner::cmd::Cmd;
use crate::runner::docker_exec::DockerExec;
use crate::runner::run_parallel;

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum LifecycleCommand {
    Single(Cmd),
    Parallel(IndexMap<String, Cmd>),
}

impl LifecycleCommand {
    pub fn run_in_container(
        &self,
        label: &str,
        container: &str,
        user: Option<&str>,
        workdir: Option<&Path>,
        env: &IndexMap<String, Option<String>>,
    ) -> eyre::Result<()> {
        match self {
            LifecycleCommand::Single(cmd) => {
                let exec = DockerExec {
                    container,
                    cmd,
                    user,
                    workdir,
                    env,
                };
                crate::runner::run(label, &exec, None)
            }
            LifecycleCommand::Parallel(map) => {
                let execs: Vec<_> = map
                    .iter()
                    .map(|(label, cmd)| {
                        (
                            label.as_str(),
                            DockerExec {
                                container,
                                cmd,
                                user,
                                workdir,
                                env,
                            },
                        )
                    })
                    .collect();
                run_parallel(execs.iter().map(|(l, e)| (*l, e)))
            }
        }
    }
}

impl Runnable for LifecycleCommand {
    fn command(&self) -> Cow<'_, str> {
        match self {
            LifecycleCommand::Single(cmd) => cmd.command(),
            LifecycleCommand::Parallel(map) => map
                .keys()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
                .into(),
        }
    }

    fn run(&self, dir: Option<&Path>) -> eyre::Result<()> {
        match self {
            LifecycleCommand::Single(cmd) => cmd.run(dir),
            LifecycleCommand::Parallel(map) => {
                run_parallel(map.iter().map(|(l, c)| (l.as_ref(), c)))
            }
        }
    }
}
