use std::borrow::Cow;
use std::path::Path;

use serde::{Deserialize, Serialize};
use vec1::Vec1;

use crate::runner::Runnable;

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged)]
pub enum Cmd {
    Shell(String),
    Args(Vec1<String>),
}

impl Cmd {
    pub fn as_args(&self) -> Vec<&str> {
        match self {
            Cmd::Shell(prog) => vec!["/bin/sh", "-c", prog],
            Cmd::Args(args) => args.iter().map(|s| s.as_str()).collect(),
        }
    }
}

impl Runnable for Cmd {
    fn command(&self) -> Cow<'_, str> {
        match self {
            Cmd::Shell(prog) => prog.into(),
            Cmd::Args(args) => args.join(" ").into(),
        }
    }

    fn run(&self, dir: Option<&Path>) -> eyre::Result<()> {
        let argv = self.as_args();
        super::pty::run_in_pty(&argv, dir)
    }
}
