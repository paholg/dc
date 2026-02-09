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
use crate::devcontainer::DevContainer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speed {
    Fast,
    Slow,
}

#[derive(Debug, Clone)]
pub struct Stats {
    /// Current memory use in bytes.
    pub ram: u64,
    /// Current CPU use, in percent.
    pub cpu: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct ExecSession {
    pub pid: u32,
    pub command: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub path: PathBuf,
    pub project: String,
    pub compose_project_name: String,
    pub container_ids: Vec<String>,
    pub dirty: bool,
    pub execs: Vec<ExecSession>,
    pub status: ContainerSummaryStateEnum,
    pub stats: Option<Stats>,
    pub created: Option<i64>,
}

struct ContainerInfo {
    id: String,
    state: ContainerSummaryStateEnum,
    local_folder: PathBuf,
    project: String,
    created: Option<i64>,
}

impl Workspace {
    pub async fn list_all(
        docker: &Docker,
        config: &Config,
        speed: Speed,
    ) -> eyre::Result<Vec<Workspace>> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            vec!["devcontainer.local_folder".to_string()],
        );
        list_with_filter(docker, filters, None, config, speed).await
    }

    pub async fn list_project(
        docker: &Docker,
        project: Option<&str>,
        config: &Config,
        speed: Speed,
    ) -> eyre::Result<Vec<Workspace>> {
        match project {
            Some(name) => {
                let mut filters = HashMap::new();
                filters.insert(
                    "label".to_string(),
                    vec!["devcontainer.local_folder".to_string()],
                );
                list_with_filter(docker, filters, Some(name), config, speed).await
            }
            None => Self::list_all(docker, config, speed).await,
        }
    }
}

const TABLE_SPEC: &str = "{:<}  {:<}  {:<}  {:>}  {:>}  {:>}  {:<}";

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

fn format_age(created: Option<i64>) -> String {
    let ts = match created {
        Some(secs) => jiff::Timestamp::from_second(secs).ok(),
        None => None,
    };
    let ts = match ts {
        Some(t) => t,
        None => return "-".into(),
    };
    let dur = jiff::Timestamp::now().duration_since(ts);
    let secs = dur.as_secs();
    if secs < 0 {
        return "-".into();
    }
    let secs = secs as u64;
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86400 => format!("{}h", s / 3600),
        s if s < 604800 => format!("{}d", s / 86400),
        s if s < 2_592_000 => format!("{}w", s / 604800),
        s if s < 31_536_000 => format!("{}mo", s / 2_592_000),
        s => format!("{}y", s / 31_536_000),
    }
}

struct WsFields {
    name: String,
    project: String,
    status: String,
    created: String,
    cpu: String,
    mem: String,
}

fn ws_fields(ws: &Workspace) -> eyre::Result<WsFields> {
    let name = ws
        .path
        .file_name()
        .ok_or_else(|| eyre!("workspace path has no filename"))?
        .to_string_lossy();
    let name = if ws.dirty {
        format!("{name}*")
    } else {
        name.into_owned()
    };
    let status = match ws.status {
        ContainerSummaryStateEnum::EMPTY => "-".to_string(),
        ref s => s.to_string(),
    };
    let cpu = ws.stats.as_ref().map_or("-".into(), |s| match s.cpu {
        Some(cpu) => format!("{:.1}%", cpu),
        None => "-".into(),
    });
    let mem = ws
        .stats
        .as_ref()
        .map_or("-".into(), |s| format_bytes(s.ram));
    Ok(WsFields {
        name,
        project: ws.project.clone(),
        status,
        created: format_age(ws.created),
        cpu,
        mem,
    })
}

fn ws_rows(ws: &Workspace) -> eyre::Result<Vec<Row>> {
    let f = ws_fields(ws)?;
    if ws.execs.is_empty() {
        return Ok(vec![
            Row::new()
                .with_cell(f.name)
                .with_cell(f.project)
                .with_cell(f.status)
                .with_cell(f.created)
                .with_cell(f.cpu)
                .with_ansi_cell(f.mem)
                .with_cell("-"),
        ]);
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
                    .with_cell(&f.created)
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
                    .with_cell("")
                    .with_cell(cmd),
            );
        }
    }
    Ok(rows)
}

fn ws_row_compact(ws: &Workspace) -> eyre::Result<Row> {
    let f = ws_fields(ws)?;
    let execs = if ws.execs.is_empty() {
        "-".into()
    } else {
        ws.execs
            .iter()
            .map(format_exec)
            .collect::<Vec<_>>()
            .join(", ")
    };
    Ok(Row::new()
        .with_cell(f.name)
        .with_cell(f.project)
        .with_cell(f.status)
        .with_cell(f.created)
        .with_cell(f.cpu)
        .with_ansi_cell(f.mem)
        .with_cell(execs))
}

/// Full table with header row, for `list` output.
pub fn workspace_table<'a>(
    workspaces: impl IntoIterator<Item = &'a Workspace>,
) -> eyre::Result<Table> {
    let mut table = Table::new(TABLE_SPEC);
    table.add_row(
        Row::new()
            .with_cell("NAME")
            .with_cell("PROJECT")
            .with_cell("STATUS")
            .with_cell("CREATED")
            .with_cell("CPU")
            .with_cell("MEM")
            .with_cell("EXECS"),
    );
    for ws in workspaces {
        for row in ws_rows(ws)? {
            table.add_row(row);
        }
    }
    Ok(table)
}

/// Pair each workspace with its aligned table-row string, for the picker.
pub fn picker_items(workspaces: Vec<Workspace>) -> eyre::Result<Vec<PickerItem>> {
    let mut table = Table::new(TABLE_SPEC);
    for ws in &workspaces {
        table.add_row(ws_row_compact(ws)?);
    }
    let rendered = table.to_string();
    Ok(workspaces
        .into_iter()
        .zip(rendered.lines())
        .map(|(workspace, line)| PickerItem {
            workspace,
            rendered: line.to_string(),
        })
        .collect())
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

pub fn pick_workspace_any(
    workspaces: Vec<Workspace>,
    empty_msg: &str,
    // TODO: nucleo-picker doesn't support a title natively; we inject it as
    // the first list item as a stopgap.
    title: &str,
) -> eyre::Result<Workspace> {
    match workspaces.len() {
        0 => Err(eyre!("{empty_msg}")),
        1 => Ok(workspaces.into_iter().next().unwrap()),
        _ => {
            let items = picker_items(workspaces)?;
            let mut picker = nucleo_picker::PickerOptions::new()
                .sort_results(false)
                .picker(nucleo_picker::render::StrRenderer);
            let injector = picker.injector();
            for item in &items {
                injector.push(item.rendered.clone());
            }
            injector.push(title.to_string());
            let selected = picker
                .pick()
                .map_err(|e| eyre!("{e}"))?
                .ok_or_else(|| eyre!("no workspace selected"))?;
            let idx = items
                .iter()
                .position(|it| it.rendered == *selected)
                .ok_or_else(|| eyre!("selected item is not a workspace"))?;
            Ok(items.into_iter().nth(idx).unwrap().workspace)
        }
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
            let items = picker_items(workspaces)?;
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
        let labels = c.labels.ok_or_else(|| eyre!("container missing labels"))?;
        let local_folder = match labels.get("devcontainer.local_folder") {
            Some(f) => PathBuf::from(f),
            None => continue,
        };
        let project = labels.get("dev.dc.project").cloned().unwrap_or_default();
        let id = match c.id {
            Some(id) => id,
            None => continue,
        };
        let state = c.state.ok_or_else(|| eyre!("container missing state"))?;

        result.push(ContainerInfo {
            id,
            state,
            local_folder,
            project,
            created: c.created,
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

    let workspace_dir = workspace_dir.canonicalize()?;
    let mut worktrees = Vec::new();

    for line in output.lines() {
        if let Some(path_str) = line.strip_prefix("worktree ") {
            let path = PathBuf::from(path_str);
            if path.starts_with(&workspace_dir) {
                worktrees.push(path);
            }
        }
    }

    Ok(worktrees)
}

// Phase 3a (fast): single one_shot reading — memory only, no CPU delta.
async fn docker_stats_fast(
    docker: &Docker,
    container_ids: &[String],
) -> eyre::Result<HashMap<String, Stats>> {
    if container_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let mut map = HashMap::new();
    for id in container_ids {
        let mut stream = docker.stats(
            id,
            Some(StatsOptions {
                stream: false,
                one_shot: true,
            }),
        );
        match stream.next().await {
            Some(Ok(stats)) => {
                let ram = stats
                    .memory_stats
                    .as_ref()
                    .and_then(|m| m.usage)
                    .ok_or_else(|| eyre!("missing memory stats for container {id}"))?;
                map.insert(id.clone(), Stats { ram, cpu: None });
            }
            Some(Err(e)) => return Err(e.into()),
            None => return Err(eyre!("no stats response for container {id}")),
        }
    }
    Ok(map)
}

// Phase 3a (full): concurrent streams, two readings each for CPU delta.
async fn docker_stats_full(
    docker: &Docker,
    container_ids: &[String],
) -> eyre::Result<HashMap<String, Stats>> {
    if container_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let futures: Vec<_> = container_ids
        .iter()
        .map(|id| async move {
            let mut stream = docker.stats(
                id,
                Some(StatsOptions {
                    stream: true,
                    one_shot: false,
                }),
            );
            // First reading: immediate, gives us memory + baseline CPU counters.
            let first = match stream.next().await {
                Some(r) => r?,
                None => eyre::bail!("no stats response for container {id}"),
            };
            let ram = first
                .memory_stats
                .as_ref()
                .and_then(|m| m.usage)
                .ok_or_else(|| eyre!("missing memory stats for container {id}"))?;

            // Second reading: ~1s later, has a real precpu delta.
            let cpu = match stream.next().await {
                Some(Ok(second)) => compute_cpu_percent(&second),
                _ => None,
            };
            Ok::<_, eyre::Report>((id.clone(), Stats { ram, cpu }))
        })
        .collect();

    futures::future::try_join_all(futures)
        .await
        .map(|v| v.into_iter().collect())
}

fn compute_cpu_percent(stats: &bollard::models::ContainerStatsResponse) -> Option<f32> {
    let cpu = stats.cpu_stats.as_ref()?;
    let precpu = stats.precpu_stats.as_ref()?;

    let total = cpu.cpu_usage.as_ref()?.total_usage?;
    let pre_total = precpu.cpu_usage.as_ref()?.total_usage?;
    let system = cpu.system_cpu_usage?;
    let pre_system = precpu.system_cpu_usage?;
    let online_cpus = cpu.online_cpus? as f32;

    let cpu_delta = total as f32 - pre_total as f32;
    let system_delta = system as f32 - pre_system as f32;

    if system_delta > 0.0 && cpu_delta >= 0.0 {
        Some(cpu_delta / system_delta * online_cpus * 100.0)
    } else {
        Some(0.0)
    }
}

// Phase 3b: exec-session detection
async fn detect_execs(
    docker: &Docker,
    container_ids: &[String],
) -> eyre::Result<HashMap<String, Vec<ExecSession>>> {
    let mut result: HashMap<String, Vec<ExecSession>> = HashMap::new();
    if container_ids.is_empty() {
        return Ok(result);
    }

    for cid in container_ids {
        let info = docker.inspect_container(cid, None).await?;
        let exec_ids = match info.exec_ids {
            Some(ids) if !ids.is_empty() => ids,
            _ => continue,
        };

        for eid in &exec_ids {
            let exec = docker.inspect_exec(eid).await?;
            if exec.running != Some(true) {
                continue;
            }
            let pid = exec.pid.ok_or_else(|| eyre!("running exec has no PID"))? as u32;
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

    Ok(result)
}

async fn list_with_filter(
    docker: &Docker,
    filters: HashMap<String, Vec<String>>,
    project_scope: Option<&str>,
    config: &Config,
    speed: Speed,
) -> eyre::Result<Vec<Workspace>> {
    // Phase 1: Docker discovery
    let containers = docker_ps(docker, filters).await?;

    // Group containers by worktree path
    struct WorktreeGroup {
        project: String,
        container_ids: Vec<String>,
        states: Vec<ContainerSummaryStateEnum>,
        created: Option<i64>,
    }
    let mut groups: HashMap<PathBuf, WorktreeGroup> = HashMap::new();
    for c in &containers {
        let group = groups
            .entry(c.local_folder.clone())
            .or_insert_with(|| WorktreeGroup {
                project: c.project.clone(),
                container_ids: Vec::new(),
                states: Vec::new(),
                created: None,
            });
        group.container_ids.push(c.id.clone());
        group.states.push(c.state);
        if let Some(ts) = c.created {
            group.created = Some(match group.created {
                Some(prev) => prev.min(ts),
                None => ts,
            });
        }
    }

    // Resolve project for unmanaged containers (no dev.dc.project label)
    let canonical_projects: Vec<(String, PathBuf)> = config
        .projects
        .iter()
        .filter_map(|(name, proj)| proj.path.canonicalize().ok().map(|c| (name.clone(), c)))
        .collect();
    let resolutions: Vec<(PathBuf, String)> = groups
        .iter()
        .filter(|(_, group)| group.project.is_empty())
        .filter_map(|(path, _)| {
            let canonical_path = path.canonicalize().ok()?;
            canonical_projects
                .iter()
                .find(|(_, proj_canonical)| canonical_path.starts_with(proj_canonical))
                .map(|(proj_name, _)| (path.clone(), proj_name.clone()))
        })
        .collect();
    for (path, proj_name) in resolutions {
        if let Some(group) = groups.get_mut(&path) {
            group.project = proj_name;
        }
    }

    // Phase 2: Git worktree discovery — merge in worktrees with no containers
    let projects_to_scan: Vec<(&str, &crate::config::Project)> = match project_scope {
        Some(name) => {
            let (n, p) = config.project(Some(name))?;
            vec![(n, p)]
        }
        None => config
            .projects
            .iter()
            .map(|(n, p)| (n.as_str(), p))
            .collect(),
    };

    for (proj_name, project) in &projects_to_scan {
        // Include the project root itself
        groups
            .entry(project.path.clone())
            .or_insert_with(|| WorktreeGroup {
                project: proj_name.to_string(),
                container_ids: Vec::new(),
                states: Vec::new(),
                created: None,
            });

        let workspace_dir = DevContainer::load(project)?
            .common
            .customizations
            .dc
            .workspace_dir();
        for wt in git_worktrees(&project.path, &workspace_dir).await? {
            groups.entry(wt).or_insert_with(|| WorktreeGroup {
                project: proj_name.to_string(),
                container_ids: Vec::new(),
                states: Vec::new(),
                created: None,
            });
        }
    }

    // Post-filter by project scope
    if let Some(scope) = project_scope {
        groups.retain(|_, group| group.project == scope);
    }

    // Phase 3: Enrich
    let all_container_ids: Vec<String> = groups
        .values()
        .flat_map(|g| g.container_ids.iter().cloned())
        .collect();

    let stats_map = match speed {
        Speed::Slow => docker_stats_full(docker, &all_container_ids).await?,
        Speed::Fast => docker_stats_fast(docker, &all_container_ids).await?,
    };
    let mut execs_map = detect_execs(docker, &all_container_ids).await?;

    let mut workspaces = Vec::new();
    for (path, group) in groups {
        // dirty check
        let dirty = if path.exists() {
            !Command::new("git")
                .args(["status", "--porcelain"])
                .current_dir(&path)
                .output()
                .await?
                .stdout
                .is_empty()
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
                cpu: container_stats
                    .iter()
                    .filter_map(|s| s.cpu)
                    .reduce(|a, b| a + b),
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
            created: group.created,
        });
    }

    Ok(workspaces)
}
