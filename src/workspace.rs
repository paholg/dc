use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bollard::Docker;
use bollard::models::ContainerSummaryStateEnum;
use bollard::query_parameters::{ListContainersOptions, StatsOptions};
use eyre::eyre;
use futures::StreamExt;
use nucleo_picker::{Picker, Render};
use tabular::{Row, Table};
use tokio::process::Command;

use crate::bytes::format_bytes;
use crate::cli::up::compose_project_name;
use crate::config::Config;

#[derive(Debug)]
pub struct Stats {
    /// Current memory use in bytes.
    pub ram: u64,
    /// Current CPU use, in percent.
    pub cpu: f32,
}

#[derive(Debug)]
pub struct ExecSession {
    pub pid: u32,
    pub command: Vec<String>,
}

#[derive(Debug)]
pub struct Workspace {
    pub path: PathBuf,
    pub project: String,
    pub compose_project_name: String,
    pub container_ids: Vec<String>,
    pub dirty: bool,
    pub execs: Vec<ExecSession>,
    pub status: ContainerSummaryStateEnum,
    pub stats: Option<Stats>,
}

struct ContainerInfo {
    id: String,
    state: ContainerSummaryStateEnum,
    local_folder: PathBuf,
    project: String,
}

impl Workspace {
    pub async fn list_all(docker: &Docker, config: &Config) -> eyre::Result<Vec<Workspace>> {
        let mut filters = HashMap::new();
        filters.insert("label".to_string(), vec!["dev.dc.managed=true".to_string()]);
        list_with_filter(docker, filters, None, config).await
    }

    pub async fn list_project(
        docker: &Docker,
        project: Option<&str>,
        config: &Config,
    ) -> eyre::Result<Vec<Workspace>> {
        match project {
            Some(name) => {
                let mut filters = HashMap::new();
                filters.insert("label".to_string(), vec![format!("dev.dc.project={name}")]);
                list_with_filter(docker, filters, Some(name), config).await
            }
            None => Self::list_all(docker, config).await,
        }
    }
}

const TABLE_SPEC: &str = "{:<}  {:<}  {:<}  {:>}  {:>}  {:<}";

fn format_exec(exec: &ExecSession) -> String {
    const MAX_LEN: usize = 40;
    let mut parts = exec.command.iter();
    let first = match parts.next() {
        Some(s) => Path::new(s)
            .file_name()
            .unwrap_or(s.as_ref())
            .to_string_lossy(),
        None => return String::new(),
    };
    let mut out = first.into_owned();
    for arg in parts {
        out.push(' ');
        out.push_str(arg);
    }
    if out.len() > MAX_LEN {
        out.truncate(MAX_LEN - 1);
        out.push('…');
    }
    out
}

struct WsFields {
    name: String,
    project: String,
    status: String,
    cpu: String,
    mem: String,
}

fn ws_fields(ws: &Workspace) -> WsFields {
    let name = ws.path.file_name().unwrap_or_default().to_string_lossy();
    let name = if ws.dirty {
        format!("{name}*")
    } else {
        name.into_owned()
    };
    let status = match ws.status {
        ContainerSummaryStateEnum::EMPTY => "-".to_string(),
        ref s => s.to_string(),
    };
    let cpu = ws.stats.as_ref().map_or("-".into(), |s| {
        if s.cpu < 0.05 {
            "-".into()
        } else {
            format!("{:.1}%", s.cpu)
        }
    });
    let mem = ws
        .stats
        .as_ref()
        .map_or("-".into(), |s| format_bytes(s.ram));
    WsFields {
        name,
        project: ws.project.clone(),
        status,
        cpu,
        mem,
    }
}

fn ws_rows(ws: &Workspace) -> Vec<Row> {
    let f = ws_fields(ws);
    if ws.execs.is_empty() {
        return vec![
            Row::new()
                .with_cell(f.name)
                .with_cell(f.project)
                .with_cell(f.status)
                .with_cell(f.cpu)
                .with_ansi_cell(f.mem)
                .with_cell("-"),
        ];
    }
    let mut rows = Vec::with_capacity(ws.execs.len());
    for (i, exec) in ws.execs.iter().enumerate() {
        let cmd = format_exec(exec);
        if i == 0 {
            rows.push(
                Row::new()
                    .with_cell(&f.name)
                    .with_cell(&f.project)
                    .with_cell(&f.status)
                    .with_cell(&f.cpu)
                    .with_ansi_cell(&f.mem)
                    .with_cell(cmd),
            );
        } else {
            rows.push(
                Row::new()
                    .with_cell("")
                    .with_cell("")
                    .with_cell("")
                    .with_cell("")
                    .with_cell("")
                    .with_cell(cmd),
            );
        }
    }
    rows
}

fn ws_row_compact(ws: &Workspace) -> Row {
    let f = ws_fields(ws);
    let execs = if ws.execs.is_empty() {
        "-".into()
    } else {
        ws.execs
            .iter()
            .map(format_exec)
            .collect::<Vec<_>>()
            .join(", ")
    };
    Row::new()
        .with_cell(f.name)
        .with_cell(f.project)
        .with_cell(f.status)
        .with_cell(f.cpu)
        .with_ansi_cell(f.mem)
        .with_cell(execs)
}

/// Full table with header row, for `list` output.
pub fn workspace_table<'a>(workspaces: impl IntoIterator<Item = &'a Workspace>) -> Table {
    let mut table = Table::new(TABLE_SPEC);
    table.add_row(
        Row::new()
            .with_cell("NAME")
            .with_cell("PROJECT")
            .with_cell("STATUS")
            .with_cell("CPU")
            .with_cell("MEM")
            .with_cell("EXECS"),
    );
    for ws in workspaces {
        for row in ws_rows(ws) {
            table.add_row(row);
        }
    }
    table
}

/// Pair each workspace with its aligned table-row string, for the picker.
pub fn picker_items(workspaces: Vec<Workspace>) -> Vec<PickerItem> {
    let mut table = Table::new(TABLE_SPEC);
    for ws in &workspaces {
        table.add_row(ws_row_compact(ws));
    }
    let rendered = table.to_string();
    workspaces
        .into_iter()
        .zip(rendered.lines())
        .map(|(workspace, line)| PickerItem {
            workspace,
            rendered: line.to_string(),
        })
        .collect()
}

pub struct PickerItem {
    pub workspace: Workspace,
    pub rendered: String,
}

struct PickerItemRenderer;

impl Render<PickerItem> for PickerItemRenderer {
    type Str<'a> = &'a str;

    fn render<'a>(&self, item: &'a PickerItem) -> Self::Str<'a> {
        &item.rendered
    }
}

pub fn pick_workspace(workspaces: Vec<Workspace>) -> eyre::Result<(PathBuf, String, String)> {
    match workspaces.len() {
        0 => Err(eyre!("no running workspaces found")),
        1 => {
            let ws = workspaces.into_iter().next().unwrap();
            let cid = ws
                .container_ids
                .into_iter()
                .next()
                .ok_or_else(|| eyre!("no containers for workspace"))?;
            let project = ws.project.clone();
            Ok((ws.path, cid, project))
        }
        _ => {
            let items = picker_items(workspaces);
            let mut picker = Picker::new(PickerItemRenderer);
            let injector = picker.injector();
            for item in items {
                injector.push(item);
            }
            let item = picker
                .pick()
                .map_err(|e| eyre!("{e}"))?
                .ok_or_else(|| eyre!("no workspace selected"))?;
            let cid = item
                .workspace
                .container_ids
                .first()
                .cloned()
                .ok_or_else(|| eyre!("no containers for workspace"))?;
            let project = item.workspace.project.clone();
            Ok((item.workspace.path.clone(), cid, project))
        }
    }
}

// Phase 1: Docker discovery
async fn docker_ps(
    docker: &Docker,
    filters: HashMap<String, Vec<String>>,
) -> eyre::Result<Vec<ContainerInfo>> {
    let containers = docker
        .list_containers(Some(ListContainersOptions {
            all: true,
            filters: Some(filters),
            ..Default::default()
        }))
        .await?;

    let mut result = Vec::new();
    for c in containers {
        let labels = c.labels.unwrap_or_default();
        let local_folder = match labels.get("devcontainer.local_folder") {
            Some(f) => PathBuf::from(f),
            None => continue,
        };
        let project = labels.get("dev.dc.project").cloned().unwrap_or_default();
        let id = match c.id {
            Some(id) => id,
            None => continue,
        };
        let state = c.state.unwrap_or(ContainerSummaryStateEnum::EMPTY);

        result.push(ContainerInfo {
            id,
            state,
            local_folder,
            project,
        });
    }

    Ok(result)
}

// Phase 2: Git worktree discovery
async fn git_worktrees(repo_path: &Path, workspace_dir: &Path) -> eyre::Result<Vec<PathBuf>> {
    let out = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo_path)
        .output()
        .await?;
    eyre::ensure!(out.status.success(), "git worktree list failed");
    let output = String::from_utf8(out.stdout)?;

    let workspace_dir = workspace_dir.canonicalize().unwrap_or(workspace_dir.into());
    let mut worktrees = Vec::new();

    for line in output.lines() {
        if let Some(path_str) = line.strip_prefix("worktree ") {
            let path = PathBuf::from(path_str);
            let canonical = path.canonicalize().unwrap_or(path.clone());
            if canonical.starts_with(&workspace_dir) {
                worktrees.push(path);
            }
        }
    }

    Ok(worktrees)
}

// Phase 3a: docker stats (one request per container via bollard stream)
async fn docker_stats(docker: &Docker, container_ids: &[String]) -> HashMap<String, Stats> {
    let mut map = HashMap::new();
    if container_ids.is_empty() {
        return map;
    }

    for id in container_ids {
        let mut stream = docker.stats(
            id,
            Some(StatsOptions {
                stream: false,
                one_shot: true,
            }),
        );
        if let Some(Ok(stats)) = stream.next().await {
            let ram = stats
                .memory_stats
                .as_ref()
                .and_then(|m| m.usage)
                .unwrap_or(0);

            let cpu = compute_cpu_percent(&stats);

            map.insert(id.clone(), Stats { ram, cpu });
        }
    }
    map
}

fn compute_cpu_percent(stats: &bollard::models::ContainerStatsResponse) -> f32 {
    let cpu = match stats.cpu_stats.as_ref() {
        Some(c) => c,
        None => return 0.0,
    };
    let precpu = match stats.precpu_stats.as_ref() {
        Some(c) => c,
        None => return 0.0,
    };

    let total = cpu
        .cpu_usage
        .as_ref()
        .and_then(|u| u.total_usage)
        .unwrap_or(0);
    let pre_total = precpu
        .cpu_usage
        .as_ref()
        .and_then(|u| u.total_usage)
        .unwrap_or(0);
    let system = cpu.system_cpu_usage.unwrap_or(0);
    let pre_system = precpu.system_cpu_usage.unwrap_or(0);
    let online_cpus = cpu.online_cpus.unwrap_or(1) as f64;

    let cpu_delta = total as f64 - pre_total as f64;
    let system_delta = system as f64 - pre_system as f64;

    if system_delta > 0.0 && cpu_delta >= 0.0 {
        (cpu_delta / system_delta * online_cpus * 100.0) as f32
    } else {
        0.0
    }
}

// Phase 3b: exec-session detection
async fn detect_execs(
    docker: &Docker,
    container_ids: &[String],
) -> HashMap<String, Vec<ExecSession>> {
    let mut result: HashMap<String, Vec<ExecSession>> = HashMap::new();
    if container_ids.is_empty() {
        return result;
    }

    for cid in container_ids {
        let info = match docker.inspect_container(cid, None).await {
            Ok(info) => info,
            Err(_) => continue,
        };
        let exec_ids = match info.exec_ids {
            Some(ids) if !ids.is_empty() => ids,
            _ => continue,
        };

        for eid in &exec_ids {
            let exec = match docker.inspect_exec(eid).await {
                Ok(e) => e,
                Err(_) => continue,
            };
            if exec.running != Some(true) {
                continue;
            }
            let pid = exec.pid.unwrap_or(0) as u32;
            let mut command = Vec::new();
            if let Some(ref pc) = exec.process_config {
                if let Some(ref ep) = pc.entrypoint {
                    command.push(ep.clone());
                }
                if let Some(ref args) = pc.arguments {
                    command.extend(args.iter().cloned());
                }
            }
            result
                .entry(cid.clone())
                .or_default()
                .push(ExecSession { pid, command });
        }
    }

    result
}

async fn list_with_filter(
    docker: &Docker,
    filters: HashMap<String, Vec<String>>,
    project_scope: Option<&str>,
    config: &Config,
) -> eyre::Result<Vec<Workspace>> {
    // Phase 1: Docker discovery
    let containers = docker_ps(docker, filters).await?;

    // Group containers by worktree path
    struct WorktreeGroup {
        project: String,
        container_ids: Vec<String>,
        states: Vec<ContainerSummaryStateEnum>,
    }
    let mut groups: HashMap<PathBuf, WorktreeGroup> = HashMap::new();
    for c in &containers {
        let group = groups
            .entry(c.local_folder.clone())
            .or_insert_with(|| WorktreeGroup {
                project: c.project.clone(),
                container_ids: Vec::new(),
                states: Vec::new(),
            });
        group.container_ids.push(c.id.clone());
        group.states.push(c.state.clone());
    }

    // Phase 2: Git worktree discovery — merge in worktrees with no containers
    let projects_to_scan: Vec<(&str, crate::config::Project)> = match project_scope {
        Some(name) => {
            let (n, p) = config.project(Some(name))?;
            vec![(n, p)]
        }
        None => config
            .projects
            .iter()
            .map(|(n, p)| (n.as_str(), p.clone()))
            .collect(),
    };

    for (proj_name, project) in &projects_to_scan {
        if let Ok(worktrees) = git_worktrees(&project.path, &project.options.workspace_dir()).await
        {
            for wt in worktrees {
                groups.entry(wt).or_insert_with(|| WorktreeGroup {
                    project: proj_name.to_string(),
                    container_ids: Vec::new(),
                    states: Vec::new(),
                });
            }
        }
    }

    // Phase 3: Enrich
    let all_container_ids: Vec<String> = groups
        .values()
        .flat_map(|g| g.container_ids.iter().cloned())
        .collect();

    let stats_map = docker_stats(docker, &all_container_ids).await;
    let mut execs_map = detect_execs(docker, &all_container_ids).await;

    let mut workspaces = Vec::new();
    for (path, group) in groups {
        // dirty check
        let dirty = if path.exists() {
            Command::new("git")
                .args(["status", "--porcelain"])
                .current_dir(&path)
                .output()
                .await
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false)
        } else {
            false
        };

        // "most alive" status
        let status = *group
            .states
            .iter()
            .max()
            .unwrap_or(&ContainerSummaryStateEnum::EMPTY);

        let execs: Vec<ExecSession> = group
            .container_ids
            .iter()
            .flat_map(|id| execs_map.remove(id).unwrap_or_default())
            .collect();

        // Aggregate stats: sum RAM, sum CPU across containers
        let container_stats: Vec<&Stats> = group
            .container_ids
            .iter()
            .filter_map(|id| stats_map.get(id))
            .collect();
        let stats = if container_stats.is_empty() {
            None
        } else {
            Some(Stats {
                ram: container_stats.iter().map(|s| s.ram).sum(),
                cpu: container_stats.iter().map(|s| s.cpu).sum(),
            })
        };

        let compose_project_name = compose_project_name(&path);

        workspaces.push(Workspace {
            path,
            project: group.project,
            compose_project_name,
            container_ids: group.container_ids,
            dirty,
            execs,
            status,
            stats,
        });
    }

    Ok(workspaces)
}
