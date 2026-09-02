use std::path::{Path, PathBuf};
use std::process::Output;

use eyre::WrapErr;
use shared::Template;
use tokio::process::Command;

use crate::helpers::validate_name;
use crate::run::run_cmd;
use crate::workspace::Workspace;

/// What a new worktree checks out
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Checkout<'a> {
    /// A detached HEAD at the root's commit.
    Detach,
    /// This branch, created from the root's HEAD if it does not exist.
    Branch(&'a str),
}

/// The branch a new workspace checks out.
pub(crate) fn resolve_branch(
    explicit_branch: Option<&str>,
    template: Option<&Template>,
    project: &str,
    workspace: &str,
) -> eyre::Result<String> {
    if let Some(branch) = explicit_branch {
        return Ok(branch.to_string());
    }

    let default = Template::default_branch();
    let template = template.unwrap_or(&default);

    shared::render_branch(template, project, workspace)
        .wrap_err_with(|| format!("rendering branch template {:?}", template.source()))
}

pub(crate) async fn create(workspace: &Workspace<'_>, checkout: Checkout<'_>) -> eyre::Result<()> {
    validate_name(&workspace.name).map_err(|e| eyre::eyre!("invalid workspace name: {e}"))?;

    let root_path = &workspace.state.project.path;
    let repo = gix::open(root_path)
        .wrap_err_with(|| format!("failed to open git repo at {}", root_path.display()))?;

    let worktree_path_str = workspace.path.to_string_lossy();
    if workspace.path.exists() {
        // Verify the existing directory is a worktree of the expected repo
        let worktree =
            gix::open(&workspace.path).wrap_err("existing file or directory in the way")?;
        let wt_common = worktree.common_dir().canonicalize()?;
        let repo_common = repo.common_dir().canonicalize()?;
        if wt_common != repo_common {
            eyre::bail!("existing repository at {worktree_path_str}");
        }
    } else {
        let mut args = vec!["git", "worktree", "add", &worktree_path_str];
        match checkout {
            Checkout::Detach => args.push("--detach"),
            Checkout::Branch(branch) => {
                // `-b` refuses an existing branch; without it, `git worktree
                // add` would derive the branch from the path instead.
                let exists = repo
                    .try_find_reference(&format!("refs/heads/{branch}"))
                    .wrap_err_with(|| format!("looking up branch {branch}"))?
                    .is_some();
                if !exists {
                    args.push("-b");
                }
                args.push(branch);
            }
        }

        workspace.state.ensure_project_working_dir()?;
        run_cmd(&args, Some(root_path), "git worktree add").await?;
    }

    lock(workspace).await?;

    Ok(())
}

/// The worktree isn't visible from other worktrees in devcontainers, so we lock
/// it so that they won't clear it with `git worktree prune` and the like.
async fn lock(workspace: &Workspace<'_>) -> eyre::Result<()> {
    let out = Command::new("git")
        .args([
            "worktree",
            "lock",
            "--reason",
            "managed by devconcurrent (mounted into devcontainer)",
        ])
        .arg(&workspace.path)
        .current_dir(&workspace.state.project.path)
        .output()
        .await?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !stderr.contains("already locked") {
            eyre::bail!("git worktree lock failed: {}", stderr.trim());
        }
    }

    Ok(())
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

pub(crate) async fn list(repo_path: &Path) -> eyre::Result<Vec<PathBuf>> {
    let out = worktree_list(repo_path).await?;
    process_list(out)
}

/// A non-async worktree list for use in the completer.
pub(crate) fn list_sync(repo_path: &Path) -> eyre::Result<Vec<PathBuf>> {
    let out = worktree_list_sync(repo_path)?;
    process_list(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(source: &str) -> Template {
        serde_json::from_str::<Template>(&format!("{source:?}")).expect("valid template")
    }

    #[test]
    fn explicit_branch_wins() {
        let branch = resolve_branch(
            Some("other"),
            Some(&template("plg/{{workspace}}")),
            "proj",
            "foo",
        )
        .unwrap();
        assert_eq!(branch, "other");
    }

    #[test]
    fn template_renders_the_workspace() {
        let branch =
            resolve_branch(None, Some(&template("plg/{{workspace}}")), "proj", "foo").unwrap();
        assert_eq!(branch, "plg/foo");
    }

    #[test]
    fn defaults_to_the_workspace_name() {
        assert_eq!(resolve_branch(None, None, "proj", "foo").unwrap(), "foo");
    }
}
