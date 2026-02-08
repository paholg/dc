use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tokio::process::Command;
use tracing::warn;

use crate::cli::up::compose_project_name;
use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    /// Worktree exists but has no containers
    None,
    Dead,
    Exited,
    Removing,
    Created,
    Paused,
    Restarting,
    Running,
}

impl Status {
    pub fn from_docker_state(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "running" => Self::Running,
            "created" => Self::Created,
            "paused" => Self::Paused,
            "restarting" => Self::Restarting,
            "removing" => Self::Removing,
            "exited" => Self::Exited,
            "dead" => Self::Dead,
            _ => Self::None,
        }
    }
}

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
    pub status: Status,
    pub stats: Option<Stats>,
}

#[derive(Deserialize)]
struct DockerPsEntry {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "State")]
    state: String,
    #[serde(rename = "Labels")]
    labels: String,
}

#[derive(Deserialize)]
struct DockerStatsEntry {
    #[serde(rename = "Container")]
    container: String,
    #[serde(rename = "MemUsage")]
    mem_usage: String,
    #[serde(rename = "CPUPerc")]
    cpu_perc: String,
}

#[derive(Deserialize)]
struct DockerExecInspect {
    #[serde(rename = "Running")]
    running: bool,
    #[serde(rename = "Pid")]
    pid: u32,
    #[serde(rename = "ProcessConfig")]
    process_config: ExecProcessConfig,
}

#[derive(Deserialize)]
struct ExecProcessConfig {
    entrypoint: String,
    #[serde(default)]
    arguments: Vec<String>,
}

fn parse_labels(labels_str: &str) -> HashMap<&str, &str> {
    labels_str
        .split(',')
        .filter_map(|kv| kv.split_once('='))
        .collect()
}

fn parse_mem_usage(s: &str) -> u64 {
    // "123.4MiB / 8GiB" → take the part before " / "
    let usage = s.split(" / ").next().unwrap_or("").trim();
    let (num_str, unit) = split_number_unit(usage);
    let num: f64 = num_str.parse().unwrap_or(0.0);
    match unit.to_lowercase().as_str() {
        "b" => num as u64,
        "kib" => (num * 1024.0) as u64,
        "mib" => (num * 1024.0 * 1024.0) as u64,
        "gib" => (num * 1024.0 * 1024.0 * 1024.0) as u64,
        "tib" => (num * 1024.0 * 1024.0 * 1024.0 * 1024.0) as u64,
        _ => 0,
    }
}

fn split_number_unit(s: &str) -> (&str, &str) {
    let pos = s.find(|c: char| c.is_alphabetic()).unwrap_or(s.len());
    (&s[..pos], &s[pos..])
}

fn parse_cpu_perc(s: &str) -> f32 {
    // "1.23%" → 1.23
    s.trim_end_matches('%').trim().parse().unwrap_or(0.0)
}

struct ContainerInfo {
    id: String,
    state: String,
    local_folder: PathBuf,
    project: String,
}

impl Workspace {
    pub async fn list_all(config: &Config) -> eyre::Result<Vec<Workspace>> {
        list_with_filter(&["--filter", "label=dev.dc.managed=true"], None, config).await
    }

    pub async fn list_project(project: Option<&str>, config: &Config) -> eyre::Result<Vec<Workspace>> {
        match project {
            Some(name) => {
                let filter = format!("label=dev.dc.project={name}");
                list_with_filter(&["--filter", &filter], Some(name), config).await
            }
            None => Self::list_all(config).await,
        }
    }
}

// Phase 1: Docker discovery
async fn docker_ps(extra_filters: &[&str]) -> eyre::Result<Vec<ContainerInfo>> {
    let mut args = vec!["ps", "-a"];
    args.extend_from_slice(extra_filters);
    args.extend_from_slice(&["--format", "json"]);

    let out = Command::new("docker").args(&args).output().await?;
    let output = String::from_utf8(out.stdout)?;
    let mut containers = Vec::new();

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: DockerPsEntry = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(e) => {
                warn!("failed to parse docker ps JSON line: {e}");
                continue;
            }
        };
        let labels = parse_labels(&entry.labels);
        let local_folder = match labels.get("devcontainer.local_folder") {
            Some(f) => PathBuf::from(f),
            None => continue,
        };
        let project = labels.get("dev.dc.project").unwrap_or(&"").to_string();

        containers.push(ContainerInfo {
            id: entry.id,
            state: entry.state,
            local_folder,
            project,
        });
    }

    Ok(containers)
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

// Phase 3a: docker stats (one command for all running containers)
async fn docker_stats(container_ids: &[String]) -> HashMap<String, Stats> {
    if container_ids.is_empty() {
        return HashMap::new();
    }

    let mut args = vec![
        "stats".to_string(),
        "--no-stream".into(),
        "--format".into(),
        "json".into(),
    ];
    args.extend(container_ids.iter().cloned());

    let output = match Command::new("docker").args(&args).output().await {
        Ok(o) => match String::from_utf8(o.stdout) {
            Ok(s) => s,
            Err(_) => return HashMap::new(),
        },
        Err(_) => return HashMap::new(),
    };

    let mut map = HashMap::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<DockerStatsEntry>(line) {
            map.insert(
                entry.container.clone(),
                Stats {
                    ram: parse_mem_usage(&entry.mem_usage),
                    cpu: parse_cpu_perc(&entry.cpu_perc),
                },
            );
        }
    }
    map
}

// Phase 3b: exec-session detection (batched)
async fn detect_execs(container_ids: &[String]) -> HashMap<String, Vec<ExecSession>> {
    let mut result: HashMap<String, Vec<ExecSession>> = HashMap::new();
    if container_ids.is_empty() {
        return result;
    }

    // Collect exec IDs from all containers in one command
    let mut args = vec![
        "inspect".to_string(),
        "--format".into(),
        "{{json .ExecIDs}}".into(),
    ];
    args.extend(container_ids.iter().cloned());

    let output = match Command::new("docker").args(&args).output().await {
        Ok(o) => match String::from_utf8(o.stdout) {
            Ok(s) => s,
            Err(_) => return result,
        },
        Err(_) => return result,
    };

    // Each line corresponds to a container in order
    let mut all_exec_ids: Vec<(String, Vec<String>)> = Vec::new();
    for (i, line) in output.lines().enumerate() {
        let line = line.trim();
        if let Some(cid) = container_ids.get(i) {
            let exec_ids: Option<Vec<String>> = serde_json::from_str(line).ok().flatten();
            if let Some(eids) = exec_ids {
                if !eids.is_empty() {
                    all_exec_ids.push((cid.clone(), eids));
                }
            }
        }
    }

    // Inspect each exec via the Docker API socket — `docker inspect` doesn't
    // work on exec IDs, only the /exec/{id}/json endpoint does.
    for (cid, eids) in &all_exec_ids {
        for eid in eids {
            if let Some(inspect) = docker_exec_inspect(eid) {
                if inspect.running {
                    let mut command = vec![inspect.process_config.entrypoint.clone()];
                    command.extend(inspect.process_config.arguments.iter().cloned());
                    result.entry(cid.clone()).or_default().push(ExecSession {
                        pid: inspect.pid,
                        command,
                    });
                }
            }
        }
    }

    result
}

fn docker_exec_inspect(exec_id: &str) -> Option<DockerExecInspect> {
    let socket_path = "/var/run/docker.sock";
    let mut stream = UnixStream::connect(socket_path).ok()?;

    let request = format!(
        "GET /exec/{exec_id}/json HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).ok()?;

    let mut reader = BufReader::new(stream);

    // Skip HTTP status line and headers
    let mut line = String::new();
    loop {
        line.clear();
        reader.read_line(&mut line).ok()?;
        if line.trim().is_empty() {
            break;
        }
    }

    // Read remaining body
    let mut body = String::new();
    reader.read_line(&mut body).ok()?;

    serde_json::from_str(body.trim()).ok()
}

async fn list_with_filter(
    filters: &[&str],
    project_scope: Option<&str>,
    config: &Config,
) -> eyre::Result<Vec<Workspace>> {
    // Phase 1: Docker discovery
    let containers = docker_ps(filters).await?;

    // Group containers by worktree path
    struct WorktreeGroup {
        project: String,
        container_ids: Vec<String>,
        states: Vec<String>,
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
        if let Ok(worktrees) = git_worktrees(&project.path, &project.workspace_dir).await {
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

    let stats_map = docker_stats(&all_container_ids).await;
    let mut execs_map = detect_execs(&all_container_ids).await;

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
        let status = group
            .states
            .iter()
            .map(|s| Status::from_docker_state(s))
            .max()
            .unwrap_or(Status::None);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_labels() {
        let labels = parse_labels(
            "devcontainer.local_folder=/tmp/foo,dev.dc.managed=true,dev.dc.project=myproj",
        );
        assert_eq!(labels.get("devcontainer.local_folder"), Some(&"/tmp/foo"));
        assert_eq!(labels.get("dev.dc.managed"), Some(&"true"));
        assert_eq!(labels.get("dev.dc.project"), Some(&"myproj"));
    }

    #[test]
    fn test_parse_mem_usage() {
        assert_eq!(parse_mem_usage("123.4MiB / 8GiB"), 129_394_278);
        assert_eq!(parse_mem_usage("1GiB / 8GiB"), 1_073_741_824);
        assert_eq!(parse_mem_usage("0B / 0B"), 0);
    }

    #[test]
    fn test_parse_cpu_perc() {
        assert!((parse_cpu_perc("1.23%") - 1.23).abs() < f32::EPSILON);
        assert!((parse_cpu_perc("0.00%") - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_status_ordering() {
        assert!(Status::Running > Status::Paused);
        assert!(Status::Paused > Status::Created);
        assert!(Status::Created > Status::Exited);
        assert!(Status::Exited > Status::Dead);
        assert!(Status::Dead > Status::None);
    }

    #[test]
    fn test_status_from_docker_state() {
        assert_eq!(Status::from_docker_state("running"), Status::Running);
        assert_eq!(Status::from_docker_state("Running"), Status::Running);
        assert_eq!(Status::from_docker_state("exited"), Status::Exited);
        assert_eq!(Status::from_docker_state("bogus"), Status::None);
    }
}
