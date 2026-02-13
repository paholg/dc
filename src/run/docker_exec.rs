use std::borrow::Cow;
use std::path::Path;

use indexmap::IndexMap;

use crate::run;
use crate::run::cmd::Cmd;

pub struct DockerExec<'a> {
    pub name: &'a str,
    pub container: &'a str,
    pub cmd: &'a Cmd,
    pub user: Option<&'a str>,
    pub workdir: Option<&'a Path>,
    pub env: &'a IndexMap<String, Option<String>>,
}

impl run::Runnable for DockerExec<'_> {
    fn name(&self) -> Cow<'_, str> {
        self.name.into()
    }

    fn description(&self) -> Cow<'_, str> {
        self.cmd.description()
    }

    async fn run(self, _: run::Token) -> eyre::Result<()> {
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
        super::run_cmd(&full_argv, None).await
    }
}
