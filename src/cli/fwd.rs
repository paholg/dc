use bollard::Docker;
use bollard::secret::ContainerSummaryStateEnum;
use clap::Args;
use eyre::eyre;
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tracing::info;

use crate::config::Config;
use crate::workspace::Workspace;

/// Forward a local TCP port to a running devcontainer
///
/// Supply either project or name, or leave both blank to get a picker.
#[derive(Debug, Args)]
#[command(verbatim_doc_comment)]
pub struct Fwd {
    #[arg(short, long, conflicts_with = "name")]
    project: Option<String>,

    #[arg(short, long, conflicts_with = "project")]
    name: Option<String>,

    /// Host port to listen on (defaults to fwd_port in config)
    port: Option<u16>,
}

impl Fwd {
    pub async fn run(self, docker: &Docker, config: &Config) -> eyre::Result<()> {
        let (container_id, project, ws_name) = if let Some(ref name) = self.name {
            let workspaces = Workspace::list_project(docker, None, config).await?;
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
            let ws_name = name.clone();
            (cid, ws.project, ws_name)
        } else {
            let mut workspaces =
                Workspace::list_project(docker, self.project.as_deref(), config).await?;
            workspaces.retain(|ws| ws.status == ContainerSummaryStateEnum::RUNNING);
            let (path, cid, project) = crate::workspace::pick_workspace(workspaces)?;
            let ws_name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            (cid, project, ws_name)
        };

        let (_, proj_config) = config.project(Some(&project))?;

        let host_port = self
            .port
            .or(proj_config.options.fwd_port)
            .ok_or_else(|| eyre!("no port specified and no fwd_port in config"))?;

        let container_port = proj_config.options.container_port.unwrap_or(host_port);

        // Get container IP
        let info = docker.inspect_container(&container_id, None).await?;
        let networks = info
            .network_settings
            .and_then(|ns| ns.networks)
            .ok_or_else(|| eyre!("container has no networks"))?;
        let ip = networks
            .values()
            .next()
            .and_then(|ep| ep.ip_address.as_deref())
            .and_then(|ip| {
                if ip.is_empty() {
                    None
                } else {
                    Some(ip.to_string())
                }
            })
            .ok_or_else(|| eyre!("container has no IP address"))?;

        let listener = TcpListener::bind(format!("127.0.0.1:{host_port}")).await?;
        eprintln!("{ws_name}: forwarding 127.0.0.1:{host_port} -> {ip}:{container_port}");

        loop {
            let (mut local, _addr) = listener.accept().await?;
            let dest = format!("{ip}:{container_port}");
            tokio::spawn(async move {
                match TcpStream::connect(&dest).await {
                    Ok(mut remote) => {
                        let _ = copy_bidirectional(&mut local, &mut remote).await;
                    }
                    Err(e) => {
                        eprintln!("failed to connect to {dest}: {e}");
                    }
                }
            });
        }
    }
}
