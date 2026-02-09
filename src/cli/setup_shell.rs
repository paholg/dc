use clap::Parser;

#[derive(Debug, Parser)]
pub struct SetupShell {
    shell: String,
}

impl SetupShell {
    pub fn run(self) -> eyre::Result<()> {
        Ok(())
    }
}
