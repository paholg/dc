use std::os::unix::process::CommandExt;
use std::path::Path;

use bollard::secret::ContainerSummaryStateEnum;
use clap::Args;
use eyre::eyre;

use crate::cli::State;
use crate::run::cmd::Cmd;
use crate::workspace::Workspace;

/// Exec into a running devcontainer
#[derive(Debug, Args)]
pub struct Exec {
    /// command to run [default: Configured defaultExec]
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    cmd: Vec<String>,
}

impl Exec {
    pub async fn run(self, state: State) -> eyre::Result<()> {
        let name = state.resolve_workspace().await?;
        let ws = Workspace::get(&state, &name).await?;
        if ws.status() != ContainerSummaryStateEnum::RUNNING {
            return Err(eyre!("workspace is not running: {}", ws.path.display()));
        }
        let dc = state.devcontainer()?;
        let dc_options = dc.common.customizations.dc;
        let crate::devcontainer::Kind::Compose(ref compose) = dc.kind else {
            // This was handled at deserialize time already.
            unimplemented!();
        };
        let cid = ws.service_container_id()?;

        exec_interactive(
            cid,
            dc.common.remote_user.as_deref(),
            Some(compose.workspace_folder.as_path()),
            &self.cmd,
            dc_options.default_exec.as_ref(),
        )
    }
}

pub fn exec_interactive(
    container_id: &str,
    user: Option<&str>,
    workdir: Option<&Path>,
    cmd_args: &[String],
    default_cmd: Option<&Cmd>,
) -> eyre::Result<()> {
    let mut args = vec!["exec".to_string(), "-it".into()];
    if let Some(u) = user {
        args.extend(["-u".into(), u.to_string()]);
    }
    if let Some(w) = workdir {
        args.extend(["-w".into(), w.to_string_lossy().into_owned()]);
    }
    args.push(container_id.to_string());

    if cmd_args.is_empty() {
        args.extend(
            default_cmd
                .ok_or_else(|| eyre!("no command provided and no default configured"))?
                .as_args()
                .into_iter()
                .map(ToString::to_string),
        );
    } else {
        args.extend(cmd_args.iter().cloned());
    }

    // Restore cursor visibility — indicatif hides it for spinners and exec()
    // replaces the process before indicatif's cleanup can run.
    let _ = crossterm::execute!(std::io::stderr(), crossterm::cursor::Show);

    Err(std::process::Command::new("docker")
        .args(&args)
        .exec()
        .into())
}
