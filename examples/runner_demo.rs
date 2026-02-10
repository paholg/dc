use std::borrow::Cow;
use std::time::Duration;

use dc::run::cmd::{Cmd, NamedCmd};
use dc::run::{self, Runnable, Runner};

use dc::ansi::{CYAN, RESET};
use tokio::time::sleep;

struct PrintRunnable {
    count: usize,
}

impl Runnable for PrintRunnable {
    fn name(&self) -> Cow<'_, str> {
        "in-process task".into()
    }

    fn description(&self) -> Cow<'_, str> {
        "longer description of in-process task".into()
    }

    async fn run(self, _: run::Token) -> eyre::Result<()> {
        for i in 1..=self.count {
            tracing::info!("{CYAN}[rust]{RESET} processing item {i}");
            sleep(Duration::from_millis(20)).await;
        }
        Ok(())
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> eyre::Result<()> {
    dc::subscriber::init_subscriber();

    let parallel = [
        NamedCmd {
            name: "fetch".into(),
            cmd: &Cmd::Shell(
                "for i in $(seq 1 75); do printf \"fetching resource $(date)\n\"; sleep 0.04; done"
                    .into(),
            ),
            dir: None,
        },
        NamedCmd {
            name: "compile".into(),
            cmd: &Cmd::Shell(
                r#"for i in $(seq 1 12); do printf "compiling module %d\n" "$i"; sleep 0.1; done"#
                    .into(),
            ),
            dir: None,
        },
        NamedCmd {
            name: "lint".into(),
            cmd: &Cmd::Shell(
                r#"for i in $(seq 1 75); do printf "linting file %d\n" "$i"; sleep 0.02; done"#
                    .into(),
            ),
            dir: None,
        },
    ];

    Runner::run_parallel("par-cmds", parallel).await.unwrap();

    Runner::run(PrintRunnable { count: 150 }).await?;

    let cmd = r#"for i in $(seq 1 100); do printf "step %d: checking prerequisites...\n" "$i"; sleep 0.02; done"#;
    Runner::run(NamedCmd {
        name: "initialize",
        cmd: &Cmd::Shell(cmd.into()),
        dir: None,
    })
    .await?;

    Ok(())
}
