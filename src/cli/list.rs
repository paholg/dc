use bollard::Docker;
use clap::Args;

use crate::config::Config;
use crate::workspace::{Speed, Workspace, workspace_table};

/// List active devcontainers
#[derive(Debug, Args)]
pub struct List {
    #[arg(short, long, help = "name of project [default: all]")]
    project: Option<String>,
}

impl List {
    pub async fn run(self, docker: &Docker, config: &Config) -> eyre::Result<()> {
        let project = self.project.as_deref();
        let workspaces = Workspace::list_project(docker, project, config, Speed::Slow).await?;
        print!("{}", workspace_table(&workspaces)?);
        Ok(())
    }
}
