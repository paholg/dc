use std::borrow::Cow;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::runner::cmd::Cmd;
use crate::runner::{Runnable, run_parallel};

#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum LifecycleCommand {
    Single(Cmd),
    Parallel(IndexMap<String, Cmd>),
}

impl Default for LifecycleCommand {
    fn default() -> Self {
        Self::Single(Cmd::default())
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

    fn run(&self) -> eyre::Result<()> {
        match self {
            LifecycleCommand::Single(cmd) => cmd.run(),
            LifecycleCommand::Parallel(map) => {
                run_parallel(map.iter().map(|(l, c)| (l.as_ref(), c)))
            }
        }
    }
}
