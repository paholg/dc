use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::Args;
use clap_complete::engine::ArgValueCompleter;
use crossterm::style::Stylize;

use crate::bytes::Bytes;
use crate::cli::status::data::{
    ContainerRow, ContainerSources, ContainerState, ContainerStates, Cpu, Execs, FwdPorts, Info,
    Ports, PrevSample, Stats, WsSources,
};
use crate::complete::complete_workspace;
use crate::config::Config;
use crate::docker::DockerClient;
use crate::state::State;
use crate::table::{Align, ColumnDef, Datum, Gatherer, Table, TableBuilder, text, value};
use crate::workspace::Workspace;
use crate::workspace::git_status::GitStatus;

mod data;

const PERIOD: Duration = Duration::from_secs(1);

/// Show status for all workspaces in the project; container stats are aggregated.
#[derive(Debug, Args)]
pub(crate) struct Status {
    /// Split by container, showing a single workspace
    #[arg(short, long, num_args = 0..=1, add = ArgValueCompleter::new(complete_workspace))]
    workspace: Option<Option<String>>,

    /// Show live, updating data
    #[arg(short, long)]
    live: bool,
}

/// A selectable status column. Builds its [`ColumnDef`] from the gathered
/// sources; the set of columns will eventually be user-configurable.
#[derive(Clone, Copy)]
pub(crate) enum Column {
    Name,
    Status,
    Mem,
    Cpu,
    Execs,
    Ports,
    Git,
}

type GitSources = Arc<HashMap<String, Gatherer<Datum<String>>>>;

/// The NAME column: just the workspace name. Available without Docker.
fn name_column<'a>() -> ColumnDef<Workspace<'a>> {
    ColumnDef::new("NAME", Align::Left, |r: &Workspace<'a>| {
        text(r.name.clone())
    })
}

/// The GIT column. Fed by the git gatherers, so available without Docker.
fn git_column<'a>(git: &GitSources) -> ColumnDef<Workspace<'a>> {
    let git = git.clone();
    ColumnDef::new("GIT", Align::Left, move |r: &Workspace<'a>| {
        value(git[&r.name].cell(|g: &Datum<String>| g.clone()))
    })
}

impl Column {
    fn def<'a>(
        self,
        git: &GitSources,
        sources: &Arc<HashMap<String, WsSources>>,
        fwd: &Gatherer<Option<FwdPorts>>,
    ) -> ColumnDef<Workspace<'a>> {
        match self {
            Column::Name => name_column(),
            Column::Status => {
                let sources = sources.clone();
                ColumnDef::new("STATUS", Align::Left, move |r: &Workspace<'a>| {
                    value(
                        sources[&r.name].info.cell(|i: &Option<Info>| {
                            i.as_ref().map_or(Datum::Pending, |i| i.status)
                        }),
                    )
                })
            }
            Column::Mem => {
                let sources = sources.clone();
                ColumnDef::new("MEM", Align::Right, move |r: &Workspace<'a>| {
                    value(
                        sources[&r.name]
                            .stats
                            .cell(|s: &Option<Stats>| s.as_ref().map_or(Datum::Pending, |s| s.mem)),
                    )
                })
            }
            Column::Cpu => {
                let sources = sources.clone();
                ColumnDef::new("CPU", Align::Right, move |r: &Workspace<'a>| {
                    value(
                        sources[&r.name]
                            .stats
                            .cell(|s: &Option<Stats>| s.as_ref().map_or(Datum::Pending, |s| s.cpu)),
                    )
                })
            }
            Column::Execs => {
                let sources = sources.clone();
                ColumnDef::new("EXECS", Align::Right, move |r: &Workspace<'a>| {
                    value(sources[&r.name].execs.cell(|e: &Datum<Execs>| *e))
                })
            }
            Column::Ports => {
                let fwd = fwd.clone();
                ColumnDef::new("PORTS", Align::Left, move |r: &Workspace<'a>| {
                    let name = r.name.clone();
                    value(fwd.cell(move |m: &Option<FwdPorts>| {
                        m.as_ref().map_or(Datum::Pending, |m| {
                            let mut ports = m.get(&name).cloned().unwrap_or_default();
                            ports.sort_unstable();
                            Datum::Value(Ports(ports))
                        })
                    }))
                })
            }
            Column::Git => git_column(git),
        }
    }
}

impl Status {
    pub(crate) async fn run(self, project: Option<String>) -> eyre::Result<()> {
        let config = Config::load()?;
        let state = State::new(project, &config).await?;

        let (table, workspace) = match state.devcontainer.as_ref() {
            None => (self.git_only_table(&state).await?, None),
            Some(dc) => {
                let docker = dc.docker().await?.clone();
                match self.workspace.clone() {
                    None => (self.workspace_table(&state, docker).await?, None),
                    Some(name) => {
                        let workspace = state.resolve_workspace(name).await?;
                        (
                            self.container_table(docker, &workspace).await?,
                            Some(workspace),
                        )
                    }
                }
            }
        };

        let project = state.project_name.to_string().blue();

        eprintln!("PROJECT: {project}");
        if let Some(ws) = workspace {
            let ws_name = ws.name.yellow();
            eprintln!("WORKSPACE: {ws_name}")
        }

        if std::io::stderr().is_terminal() {
            table.run_tty().await
        } else {
            table.run_piped().await
        }
    }

    async fn workspace_table(
        &self,
        state: &State<'_>,
        docker: Arc<DockerClient>,
    ) -> eyre::Result<Table> {
        let mut workspaces = Workspace::list(state).await?;

        let fwd = spawn_fwd(docker.clone(), state.project_name.to_string());

        let git = build_git(&workspaces);
        let sources: Arc<HashMap<String, WsSources>> = Arc::new(
            workspaces
                .iter()
                .map(|ws| {
                    (
                        ws.name.clone(),
                        build_sources(docker.clone(), ws.compose_project_name()),
                    )
                })
                .collect(),
        );

        workspaces.sort_by(|a, b| b.is_root.cmp(&a.is_root).then_with(|| a.name.cmp(&b.name)));

        let columns = [
            Column::Name,
            Column::Status,
            Column::Mem,
            Column::Cpu,
            Column::Execs,
            Column::Ports,
            Column::Git,
        ];

        Ok(columns
            .into_iter()
            // For speed, exclude CPU (requires at least 1 sec) unless live.
            .filter(|c| self.live || !matches!(c, Column::Cpu))
            .map(|c| c.def(&git, &sources, &fwd))
            .collect::<TableBuilder<Workspace>>()
            .build(&workspaces, self.live))
    }

    async fn git_only_table(&self, state: &State<'_>) -> eyre::Result<Table> {
        let mut workspaces = Workspace::list(state).await?;
        workspaces.sort_by(|a, b| b.is_root.cmp(&a.is_root).then_with(|| a.name.cmp(&b.name)));

        let git = build_git(&workspaces);
        let columns = [name_column(), git_column(&git)];
        Ok(columns
            .into_iter()
            .collect::<TableBuilder<Workspace>>()
            .build(&workspaces, self.live))
    }

    async fn container_table(
        &self,
        docker: Arc<DockerClient>,
        workspace: &Workspace<'_>,
    ) -> eyre::Result<Table> {
        let compose_project = workspace.compose_project_name();
        let containers = docker.compose_container_info(&compose_project).await?;

        let mut rows: Vec<ContainerRow> = containers
            .iter()
            .map(|c| ContainerRow {
                id: c.id.clone(),
                service: c.service.clone().unwrap_or_else(|| short_id(&c.id)),
                exposed: c.exposed_ports.clone(),
            })
            .collect();
        rows.sort_by(|a, b| a.service.cmp(&b.service));

        // Live container states by id.
        let info = {
            let docker = docker.clone();
            let compose_project = compose_project.clone();
            Gatherer::spawn(PERIOD, move || {
                let docker = docker.clone();
                let compose_project = compose_project.clone();
                async move {
                    let states = docker
                        .compose_container_info(&compose_project)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .map(|c| (c.id, ContainerState(c.state)))
                        .collect::<ContainerStates>();
                    Some(states)
                }
            })
        };

        // The workspace's forwarded ports; attributed to containers by which
        // exposed port each one targets.
        let fwd = {
            let docker = docker.clone();
            let project = workspace.state.project_name.to_string();
            let workspace = workspace.name.clone();
            Gatherer::spawn(PERIOD, move || {
                let docker = docker.clone();
                let project = project.clone();
                let workspace = workspace.clone();
                async move {
                    let map = docker.forwarded_ports(&project).await.unwrap_or_default();
                    Some(map.get(&workspace).cloned().unwrap_or_default())
                }
            })
        };

        let sources: Arc<HashMap<String, ContainerSources>> = Arc::new(
            containers
                .iter()
                .map(|c| {
                    (
                        c.id.clone(),
                        build_container_sources(docker.clone(), c.id.clone()),
                    )
                })
                .collect(),
        );

        let mut columns: Vec<ColumnDef<ContainerRow>> = vec![
            ColumnDef::new("NAME", Align::Left, |r: &ContainerRow| {
                text(r.service.clone())
            }),
            ColumnDef::new("STATUS", Align::Left, {
                let info = info.clone();
                move |r: &ContainerRow| {
                    let id = r.id.clone();
                    value(info.cell(move |m: &Option<ContainerStates>| {
                        m.as_ref().map_or(Datum::Pending, |m| {
                            m.get(&id)
                                .copied()
                                .map_or(Datum::NotApplicable, Datum::Value)
                        })
                    }))
                }
            }),
            ColumnDef::new("MEM", Align::Right, {
                let sources = sources.clone();
                move |r: &ContainerRow| {
                    value(
                        sources[&r.id]
                            .stats
                            .cell(|s: &Option<Stats>| s.as_ref().map_or(Datum::Pending, |s| s.mem)),
                    )
                }
            }),
        ];
        if self.live {
            let sources = sources.clone();
            columns.push(ColumnDef::new(
                "CPU",
                Align::Right,
                move |r: &ContainerRow| {
                    value(
                        sources[&r.id]
                            .stats
                            .cell(|s: &Option<Stats>| s.as_ref().map_or(Datum::Pending, |s| s.cpu)),
                    )
                },
            ));
        }
        columns.push(ColumnDef::new("EXECS", Align::Right, {
            let sources = sources.clone();
            move |r: &ContainerRow| value(sources[&r.id].execs.cell(|e: &Datum<Execs>| *e))
        }));
        columns.push(ColumnDef::new("PORTS", Align::Left, {
            let fwd = fwd.clone();
            move |r: &ContainerRow| {
                let exposed = r.exposed.clone();
                value(fwd.cell(move |forwarded: &Option<Vec<u16>>| {
                    forwarded.as_ref().map_or(Datum::Pending, |forwarded| {
                        let ports = exposed
                            .iter()
                            .copied()
                            .filter(|p| forwarded.contains(p))
                            .collect();
                        Datum::Value(Ports(ports))
                    })
                }))
            }
        }));

        Ok(columns
            .into_iter()
            .collect::<TableBuilder<ContainerRow>>()
            .build(&rows, self.live))
    }
}

fn spawn_fwd(docker: Arc<DockerClient>, project: String) -> Gatherer<Option<FwdPorts>> {
    Gatherer::spawn(PERIOD, move || {
        let docker = docker.clone();
        let project = project.clone();
        async move { Some(docker.forwarded_ports(&project).await.unwrap_or_default()) }
    })
}

/// A git-status gatherer per workspace. Needs no Docker.
fn build_git(workspaces: &[Workspace<'_>]) -> GitSources {
    Arc::new(
        workspaces
            .iter()
            .map(|ws| (ws.name.clone(), spawn_git(ws.path.clone())))
            .collect(),
    )
}

fn spawn_git(path: PathBuf) -> Gatherer<Datum<String>> {
    Gatherer::spawn(PERIOD, move || {
        let path = path.clone();
        async move {
            GitStatus::fetch(&path)
                .await
                .map(|g| Datum::Value(g.to_string()))
                .unwrap_or(Datum::NotApplicable)
        }
    })
}

/// The per-workspace Docker gatherers. `stats`/`execs` derive off `info` to
/// reuse the ids it discovers, so each runs independently without re-enumerating.
fn build_sources(docker: Arc<DockerClient>, compose_project: String) -> WsSources {
    let info = {
        let docker = docker.clone();
        Gatherer::spawn(PERIOD, move || {
            let docker = docker.clone();
            let compose_project = compose_project.clone();
            async move {
                let containers = docker
                    .compose_container_info(&compose_project)
                    .await
                    .unwrap_or_default();
                let status = match containers.iter().map(|c| c.state).max() {
                    Some(s) => Datum::Value(ContainerState(s)),
                    None => Datum::NotApplicable,
                };
                let ids = containers.iter().map(|c| c.id.clone()).collect();
                Some(Info { status, ids })
            }
        })
    };

    // Recompute the moment `info` publishes, reusing its ids.
    let stats = {
        let docker = docker.clone();
        let prev: Arc<Mutex<HashMap<String, PrevSample>>> = Arc::new(Mutex::new(HashMap::new()));
        info.derive(move |info| {
            let docker = docker.clone();
            let prev = prev.clone();
            async move { poll_stats(&docker, &info, &prev).await }
        })
    };

    let execs = {
        let docker = docker.clone();
        info.derive(move |info| {
            let docker = docker.clone();
            async move { poll_execs(&docker, &info).await }
        })
    };

    WsSources { info, stats, execs }
}

async fn poll_stats(
    docker: &DockerClient,
    info: &Option<Info>,
    prev: &Mutex<HashMap<String, PrevSample>>,
) -> Option<Stats> {
    // No enumeration yet: stay pending.
    let info = info.as_ref()?;
    if info.ids.is_empty() {
        return Some(Stats {
            mem: Datum::NotApplicable,
            cpu: Datum::NotApplicable,
        });
    }

    let samples = futures::future::join_all(
        info.ids
            .iter()
            .map(|id| async move { (id.clone(), docker.stats_sample(id).await.ok()) }),
    )
    .await;

    let mut prev = prev.lock().unwrap();
    let mut mem_bytes = 0u64;
    let mut cpu_delta = 0u64;
    let mut system_prev = None;
    let mut system_now = None;
    let mut cpus = 1u32;
    let mut have_sample = false;
    for (id, sample) in &samples {
        let Some(sample) = sample else {
            continue;
        };
        have_sample = true;
        mem_bytes += sample.ram;
        if let Some(p) = prev.get(id) {
            cpu_delta += sample.cpu_total.saturating_sub(p.total);
            system_prev = Some(p.system);
        }
        if let Some(system) = sample.system_cpu {
            system_now = Some(system);
        }
        if let Some(c) = sample.online_cpus {
            cpus = c;
        }
        prev.insert(
            id.clone(),
            PrevSample {
                total: sample.cpu_total,
                system: sample.system_cpu.unwrap_or(0),
            },
        );
    }

    if !have_sample {
        return Some(Stats {
            mem: Datum::NotApplicable,
            cpu: Datum::NotApplicable,
        });
    }

    let cpu = match (system_prev, system_now) {
        (Some(sp), Some(sn)) if sn > sp => Datum::Value(Cpu(cpu_delta as f64 / (sn - sp) as f64
            * f64::from(cpus)
            * 100.0)),
        // Only one sample so far: pending.
        _ => Datum::Pending,
    };

    Some(Stats {
        mem: Datum::Value(Bytes(mem_bytes)),
        cpu,
    })
}

async fn poll_execs(docker: &DockerClient, info: &Option<Info>) -> Datum<Execs> {
    let Some(info) = info.as_ref() else {
        return Datum::Pending;
    };
    if info.ids.is_empty() {
        return Datum::NotApplicable;
    }
    let total: usize = futures::future::join_all(info.ids.iter().map(|id| docker.execs(id)))
        .await
        .into_iter()
        .filter_map(Result::ok)
        .sum();
    Datum::Value(Execs(total))
}

/// Per-container stats and execs gatherers.
fn build_container_sources(docker: Arc<DockerClient>, id: String) -> ContainerSources {
    let stats = {
        let docker = docker.clone();
        let id = id.clone();
        let prev: Arc<Mutex<Option<PrevSample>>> = Arc::new(Mutex::new(None));
        Gatherer::spawn(PERIOD, move || {
            let docker = docker.clone();
            let id = id.clone();
            let prev = prev.clone();
            async move { poll_container_stats(&docker, &id, &prev).await }
        })
    };

    let execs = Gatherer::spawn(PERIOD, move || {
        let docker = docker.clone();
        let id = id.clone();
        async move {
            match docker.execs(&id).await {
                Ok(n) => Datum::Value(Execs(n)),
                Err(_) => Datum::NotApplicable,
            }
        }
    });

    ContainerSources { stats, execs }
}

async fn poll_container_stats(
    docker: &DockerClient,
    id: &str,
    prev: &Mutex<Option<PrevSample>>,
) -> Option<Stats> {
    let Ok(sample) = docker.stats_sample(id).await else {
        return Some(Stats {
            mem: Datum::NotApplicable,
            cpu: Datum::NotApplicable,
        });
    };

    let mut prev = prev.lock().unwrap();
    let cpu = match (*prev, sample.system_cpu) {
        (Some(p), Some(system_now)) if system_now > p.system => {
            let delta = sample.cpu_total.saturating_sub(p.total);
            let cpus = f64::from(sample.online_cpus.unwrap_or(1));
            Datum::Value(Cpu(delta as f64 / (system_now - p.system) as f64
                * cpus
                * 100.0))
        }
        // Only one sample so far: pending.
        _ => Datum::Pending,
    };
    *prev = Some(PrevSample {
        total: sample.cpu_total,
        system: sample.system_cpu.unwrap_or(0),
    });

    Some(Stats {
        mem: Datum::Value(Bytes(sample.ram)),
        cpu,
    })
}

fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}
