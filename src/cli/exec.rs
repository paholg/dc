use std::borrow::Cow;
use std::path::PathBuf;

use clap::Args;
use eyre::eyre;
use nucleo_picker::{Picker, Render};

use crate::config::Config;
use crate::devcontainer::DevContainer;
use crate::workspace::{Status, Workspace};

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

struct WsRenderer;

impl Render<Workspace> for WsRenderer {
    type Str<'a> = Cow<'a, str>;

    fn render<'a>(&self, ws: &'a Workspace) -> Self::Str<'a> {
        ws.path.file_name().unwrap_or_default().to_string_lossy()
    }
}

impl Exec {
    pub async fn run(self, config: &Config) -> eyre::Result<()> {
        let (path, container_id) = if let Some(ref name) = self.name {
            let workspaces = Workspace::list_project(None, config).await?;
            let ws = workspaces
                .into_iter()
                .find(|ws| {
                    ws.path
                        .file_name()
                        .map(|f| f == name.as_str())
                        .unwrap_or(false)
                })
                .ok_or_else(|| eyre!("no workspace found with name: {name}"))?;
            if ws.status != Status::Running {
                return Err(eyre!("workspace is not running: {}", ws.path.display()));
            }
            let cid = ws
                .container_ids
                .into_iter()
                .next()
                .ok_or_else(|| eyre!("no containers for workspace"))?;
            (ws.path, cid)
        } else {
            let mut workspaces = Workspace::list_project(self.project.as_deref(), config).await?;
            workspaces.retain(|ws| ws.status == Status::Running);
            pick_workspace(workspaces)?
        };

        let dc = DevContainer::load(&path)?;
        let crate::devcontainer::Kind::Compose(ref compose) = dc.kind else {
            panic!();
        };

        super::up::exec_interactive(
            &container_id,
            dc.common.remote_user.as_deref(),
            Some(compose.workspace_folder.as_path()),
            &self.cmd,
            config,
        )
    }
}

fn pick_workspace(workspaces: Vec<Workspace>) -> eyre::Result<(PathBuf, String)> {
    match workspaces.len() {
        0 => Err(eyre!("no running workspaces found")),
        1 => {
            let ws = workspaces.into_iter().next().unwrap();
            let cid = ws
                .container_ids
                .into_iter()
                .next()
                .ok_or_else(|| eyre!("no containers for workspace"))?;
            Ok((ws.path, cid))
        }
        _ => {
            let mut picker = Picker::new(WsRenderer);
            let injector = picker.injector();
            for ws in workspaces {
                injector.push(ws);
            }
            let ws = picker
                .pick()
                .map_err(|e| eyre!("{e}"))?
                .ok_or_else(|| eyre!("no workspace selected"))?;
            let cid = ws
                .container_ids
                .first()
                .cloned()
                .ok_or_else(|| eyre!("no containers for workspace"))?;
            Ok((ws.path.clone(), cid))
        }
    }
}
