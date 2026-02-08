use std::borrow::Cow;
use std::io::{BufRead, BufReader};

use duct::Expression;
use serde::{Deserialize, Serialize};

use crate::runner::Runnable;

#[derive(Debug, Deserialize, Serialize, Default, Clone)]
#[serde(untagged)]
pub enum Cmd {
    #[default]
    None,
    Shell(String),
    Args(Vec<String>),
}

impl Cmd {
    fn as_expression(&self) -> Option<Expression> {
        match self {
            Cmd::None => None,
            Cmd::Shell(prog) => Some(duct_sh::sh_dangerous(prog)),
            Cmd::Args(args) => {
                let (prog, rest) = args.split_first()?;
                Some(duct::cmd(prog, rest))
            }
        }
    }
}

impl Runnable for Cmd {
    fn command(&self) -> Cow<'_, str> {
        match self {
            Cmd::None => "".into(),
            Cmd::Shell(prog) => prog.into(),
            Cmd::Args(args) => args.join(" ").into(),
        }
    }

    fn run(&self) -> eyre::Result<()> {
        let Some(expression) = self.as_expression() else {
            return Ok(());
        };

        let handle = expression.stderr_to_stdout().reader()?;
        for line in BufReader::new(handle).lines() {
            let line = line?;
            tracing::info!("{line}");
        }

        Ok(())
    }
}
