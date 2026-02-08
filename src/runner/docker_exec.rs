use std::borrow::Cow;
use std::path::Path;

use indexmap::IndexMap;

use crate::runner::Runnable;
use crate::runner::cmd::Cmd;

pub struct DockerExec<'a> {
    pub container: &'a str,
    pub cmd: &'a Cmd,
    pub user: Option<&'a str>,
    pub workdir: Option<&'a Path>,
    pub env: &'a IndexMap<String, Option<String>>,
}

impl Runnable for DockerExec<'_> {
    fn command(&self) -> Cow<'_, str> {
        self.cmd.command()
    }

    fn run(&self, _dir: Option<&Path>) -> eyre::Result<()> {
        let workdir_str;
        let mut args: Vec<&str> = vec!["exec"];
        if let Some(u) = self.user {
            args.extend(["-u", u]);
        }
        if let Some(w) = self.workdir {
            workdir_str = w.to_string_lossy();
            args.extend(["-w", &workdir_str]);
        }
        let env_args: Vec<String> = self
            .env
            .iter()
            .map(|(k, v)| match v {
                Some(v) => format!("{k}={v}"),
                None => k.clone(),
            })
            .collect();
        for e in &env_args {
            args.extend(["-e", e]);
        }
        args.push(self.container);
        args.extend(self.cmd.as_args());

        let full_argv: Vec<&str> = std::iter::once("docker").chain(args).collect();
        super::pty::run_in_pty(&full_argv, None)
    }
}
