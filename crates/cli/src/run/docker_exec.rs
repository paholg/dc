use std::borrow::Cow;
use std::path::Path;

use indexmap::IndexMap;

use crate::run;
use crate::run::cmd::Cmd;

pub(crate) struct DockerExec<'a> {
    pub(crate) name: &'a str,
    pub(crate) container: &'a str,
    pub(crate) cmd: &'a Cmd,
    pub(crate) user: Option<&'a str>,
    pub(crate) workdir: Option<&'a Path>,
    pub(crate) env: &'a IndexMap<String, Option<String>>,
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
        // Per spec, `null` in remoteEnv means "unset" the variable. We can't actually unset PID-1
        // inherited vars via `docker exec -e`, so we set to empty string — closer to spec intent
        // than the reference impl, which stringifies `null` to the literal text "null".
        let env_args: Vec<String> = self
            .env
            .iter()
            .map(|(k, v)| format!("{k}={}", v.as_deref().unwrap_or("")))
            .collect();
        for e in &env_args {
            args.extend(["-e", e]);
        }
        args.push(self.container);
        args.extend(self.cmd.as_args());

        let full_argv: Vec<&str> = std::iter::once("docker").chain(args).collect();

        // Everything needed to reproduce the run, minus the `-e` flags: the
        // probed environment is what made the old error unreadable.
        let short_id = self.container.get(..12).unwrap_or(self.container);
        let mut what = format!("`{}` in container {short_id}", self.cmd.description());
        let mut details = Vec::new();
        if let Some(u) = self.user {
            details.push(format!("user {u}"));
        }
        if let Some(w) = self.workdir {
            details.push(format!("cwd {}", w.display()));
        }
        if !details.is_empty() {
            what.push_str(&format!(" ({})", details.join(", ")));
        }

        super::run_cmd(&full_argv, None, &what).await
    }
}
