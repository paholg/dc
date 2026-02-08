use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use clap::Args;
use tokio::process::Command;
use tracing::warn;

use crate::config::Config;
use crate::workspace::Workspace;

#[derive(Debug, Args)]
pub struct Prune {
    #[arg(
        short,
        long,
        help = "name of project [default: The first one configured]"
    )]
    project: Option<String>,

    #[arg(short, long, help = "skip confirmation prompt")]
    yes: bool,
}

impl Prune {
    pub async fn run(self, config: &Config) -> eyre::Result<()> {
        let (_, project) = config.project(self.project.as_deref())?;

        let worktrees = list_worktrees(&project.path, &project.workspace_dir).await?;
        if worktrees.is_empty() {
            println!("Nothing to prune.");
            return Ok(());
        }

        let workspaces = Workspace::list_project(self.project.as_deref(), config).await?;
        let ws_map: HashMap<&Path, &Workspace> = workspaces
            .iter()
            .map(|ws| (ws.path.as_path(), ws))
            .collect();

        let mut in_use = Vec::new();
        let mut dirty = Vec::new();
        let mut to_clean = Vec::new();

        for path in worktrees {
            if !path.exists() {
                to_clean.push(path);
            } else if let Some(ws) = ws_map.get(path.as_path()) {
                if !ws.execs.is_empty() {
                    in_use.push(path);
                } else if ws.dirty {
                    dirty.push(path);
                } else {
                    to_clean.push(path);
                }
            } else {
                to_clean.push(path);
            }
        }

        if !in_use.is_empty() {
            println!("In use (skipping):");
            for p in &in_use {
                println!("  {}", p.display());
            }
            println!();
        }
        if !dirty.is_empty() {
            println!("Dirty (skipping):");
            for p in &dirty {
                println!("  {}", p.display());
            }
            println!();
        }

        if to_clean.is_empty() {
            return Ok(());
        }

        println!("Will remove:");
        for p in &to_clean {
            println!("  {}", p.display());
        }
        println!();

        if !self.yes && !confirm()? {
            println!("Aborted.");
            return Ok(());
        }

        for path in &to_clean {
            let compose_name = super::up::compose_project_name(path);
            cleanup(&project.path, path, &compose_name).await?;
        }

        Ok(())
    }
}

async fn list_worktrees(repo_path: &Path, workspace_dir: &Path) -> eyre::Result<Vec<PathBuf>> {
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

async fn cleanup(repo_path: &Path, path: &Path, compose_name: &str) -> eyre::Result<()> {
    let down_result = Command::new("docker")
        .args(["compose", "-p", compose_name, "down", "-v", "--remove-orphans"])
        .status()
        .await;

    if let Err(e) = down_result {
        warn!("docker compose down failed for {}: {e}", path.display());
    }

    let override_file = std::env::temp_dir().join(format!("{compose_name}-override.yml"));
    if override_file.exists() {
        let _ = std::fs::remove_file(&override_file);
    }

    let force = !path.exists();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    let path_str = path.to_string_lossy();
    args.push(&path_str);

    let status = Command::new("git")
        .args(&args)
        .current_dir(repo_path)
        .status()
        .await?;
    eyre::ensure!(status.success(), "git worktree remove failed");

    println!("Removed {}", path.display());
    Ok(())
}

fn confirm() -> eyre::Result<bool> {
    print!("Proceed? [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().eq_ignore_ascii_case("y"))
}
