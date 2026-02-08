use std::borrow::Cow;
use std::time::Duration;

use indexmap::IndexMap;

use dc::ansi::{CYAN, RESET};
use dc::devcontainer::lifecycle_command::LifecycleCommand;
use dc::runner::cmd::Cmd;
use dc::runner::{Runnable, run};

struct PrintRunnable {
    count: usize,
}

impl Runnable for PrintRunnable {
    fn command(&self) -> Cow<'_, str> {
        Cow::Borrowed("in-process task")
    }

    fn run(&self, _dir: Option<&std::path::Path>) -> eyre::Result<()> {
        for i in 1..=self.count {
            tracing::info!("{CYAN}[rust]{RESET} processing item {i}");
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }
}

fn main() -> eyre::Result<()> {
    dc::subscriber::init_subscriber();

    let mut parallel = IndexMap::new();
    parallel.insert(
        "fetch".into(),
        Cmd::Shell(
            r#"for i in $(seq 1 250); do printf "fetching resource %d\n" "$i"; sleep 0.02; done"#
                .into(),
        ),
    );
    parallel.insert(
        "compile".into(),
        Cmd::Shell(
            r#"for i in $(seq 1 50); do printf "compiling module %d\n" "$i"; sleep 0.05; done"#
                .into(),
        ),
    );
    parallel.insert(
        "lint".into(),
        Cmd::Shell(
            r#"for i in $(seq 1 250); do printf "linting file %d\n" "$i"; sleep 0.01; done"#.into(),
        ),
    );
    let lifecycle = LifecycleCommand::Parallel(parallel);
    run("parallel lifecycle", &lifecycle, None)?;

    run("in-process pre-task", &PrintRunnable { count: 150 }, None)?;

    let cmd = r#"for i in $(seq 1 100); do printf "\033[32m[init]\033[0m step %d: checking prerequisites...\n" "$i"; sleep 0.02; done"#;
    run("initialize", &Cmd::Shell(cmd.into()), None)?;

    let cmd = r#"for i in $(seq 1 100); do printf "\033[34m[build]\033[0m compiling module %d of 100...\n" "$i"; sleep 0.01; done"#;
    run("build project", &Cmd::Shell(cmd.into()), None)?;

    let cmd = r#"for i in $(seq 1 100); do printf "\033[33m[test]\033[0m test_%03d ... \033[32mok\033[0m\n" "$i"; sleep 0.015; done"#;
    run("run tests", &Cmd::Shell(cmd.into()), None)?;

    let cmd = r#"for i in $(seq 1 50); do printf "\033[35m[setup]\033[0m configuring service %d\n" "$i"; sleep 0.03; done"#;
    run("post-install setup", &Cmd::Shell(cmd.into()), None)?;

    run("in-process task", &PrintRunnable { count: 50 }, None)?;

    Ok(())
}
