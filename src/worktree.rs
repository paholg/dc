use std::path::{Path, PathBuf};
use std::process::Output;

use eyre::WrapErr;
use tokio::process::Command;

use crate::run::run_cmd;

pub async fn create(
    repo_path: &Path,
    workspace_dir: &Path,
    name: &str,
    detach: bool,
) -> eyre::Result<PathBuf> {
    if Path::new(name).file_name().is_none_or(|f| f != name) {
        eyre::bail!("invalid workspace name: {name:?}");
    }

    let repo = gix::open(repo_path)
        .wrap_err_with(|| format!("failed to open git repo at {}", repo_path.display()))?;

    let worktree_path = workspace_dir.join(name);
    let worktree_path_str = worktree_path.to_string_lossy();
    if worktree_path.exists() {
        // Verify the existing directory is a worktree of the expected repo
        let worktree =
            gix::open(&worktree_path).wrap_err("existing file or directory in the way")?;
        let wt_common = worktree.common_dir().canonicalize()?;
        let repo_common = repo.common_dir().canonicalize()?;
        if wt_common != repo_common {
            eyre::bail!("existing repository at {worktree_path_str}");
        }
    } else {
        let mut args = vec!["git", "worktree", "add", &worktree_path_str];
        if detach {
            args.push("--detach");
        }
        run_cmd(&args, Some(repo_path)).await?;
    }

    Ok(worktree_path)
}

async fn worktree_list(repo_path: &Path) -> eyre::Result<Output> {
    Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo_path)
        .output()
        .await
        .map_err(Into::into)
}

// We want a sync version for the completer
fn worktree_list_sync(repo_path: &Path) -> eyre::Result<Output> {
    std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(repo_path)
        .output()
        .map_err(Into::into)
}

fn process_list(out: Output) -> eyre::Result<Vec<PathBuf>> {
    eyre::ensure!(out.status.success(), "git worktree list failed");
    let output =
        String::from_utf8(out.stdout).wrap_err("git worktree list output is not valid UTF-8")?;

    Ok(output
        .lines()
        .filter_map(|line| line.strip_prefix("worktree ").map(PathBuf::from))
        .collect())
}

pub async fn list(repo_path: &Path) -> eyre::Result<Vec<PathBuf>> {
    let out = worktree_list(repo_path).await?;
    process_list(out)
}

/// A non-async worktree list for use in the completer.
pub fn list_sync(repo_path: &Path) -> eyre::Result<Vec<PathBuf>> {
    let out = worktree_list_sync(repo_path)?;
    process_list(out)
}
