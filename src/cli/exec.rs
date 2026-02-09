use bollard::Docker;
use bollard::secret::ContainerSummaryStateEnum;
use clap::Args;
use eyre::eyre;

use crate::config::Config;
use crate::devcontainer::DevContainer;
use crate::workspace::{Speed, Workspace};

/// Exec into a running devcontainer
///
/// Supply either project or name, or leave both blank to get a picker.
#[derive(Debug, Args)]
#[command(verbatim_doc_comment)]
pub struct Exec {
    #[arg(short, long, conflicts_with = "name")]
    project: Option<String>,

    #[arg(short, long, conflicts_with = "project")]
    name: Option<String>,

    #[arg(
        num_args = 0..,
        allow_hyphen_values = true,
        trailing_var_arg = true,
    )]
    cmd: Vec<String>,
}

impl Exec {
    pub async fn run(self, docker: &Docker, config: &Config) -> eyre::Result<()> {
        let (_path, container_id, project_name) = if let Some(ref name) = self.name {
            let workspaces = Workspace::list_project(docker, None, config, Speed::Fast).await?;
            let ws = workspaces
                .into_iter()
                .find(|ws| {
                    ws.path
                        .file_name()
                        .map(|f| f == name.as_str())
                        .unwrap_or(false)
                })
                .ok_or_else(|| eyre!("no workspace found with name: {name}"))?;
            if ws.status != ContainerSummaryStateEnum::RUNNING {
                return Err(eyre!("workspace is not running: {}", ws.path.display()));
            }
            let cid = ws
                .container_ids
                .into_iter()
                .next()
                .ok_or_else(|| eyre!("no containers for workspace"))?;
            (ws.path, cid, ws.project)
        } else {
            let mut workspaces =
                Workspace::list_project(docker, self.project.as_deref(), config, Speed::Fast)
                    .await?;
            workspaces.retain(|ws| ws.status == ContainerSummaryStateEnum::RUNNING);
            let (path, cid, project) = crate::workspace::pick_workspace(workspaces)?;
            (path, cid, project)
        };

        let (_, project) = config.project(Some(&project_name))?;
        let dc = DevContainer::load(project)?;
        let crate::devcontainer::Kind::Compose(ref compose) = dc.kind else {
            panic!();
        };
        let dc_options = dc.common.customizations.dc;

        super::up::exec_interactive(
            &container_id,
            dc.common.remote_user.as_deref(),
            Some(compose.workspace_folder.as_path()),
            &self.cmd,
            dc_options.default_exec.as_ref(),
        )
    }
}
