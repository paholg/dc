use std::borrow::Cow;
use std::path::Path;

use serde::{Deserialize, Serialize};
use vec1::Vec1;

use crate::run;

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

    pub fn description(&self) -> Cow<'_, str> {
        match &self {
            Cmd::Shell(prog) => prog.into(),
            Cmd::Args(vec1) => vec1.join(" ").into(),
        }
    }
}

pub struct NamedCmd<'a> {
    pub name: &'a str,
    pub cmd: &'a Cmd,
    pub dir: Option<&'a Path>,
}

impl run::Runnable for NamedCmd<'_> {
    fn name(&self) -> Cow<'_, str> {
        self.name.into()
    }

    fn description(&self) -> Cow<'_, str> {
        self.cmd.description()
    }

    async fn run(self, _: run::Token) -> eyre::Result<()> {
        let argv = self.cmd.as_args();
        super::pty::run_in_pty(&argv, self.dir).await
    }
}
