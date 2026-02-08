use std::path::{Path, PathBuf};

use jiff::Zoned;
use nanoid::nanoid;

pub fn create(repo_path: &Path, workspace_dir: &Path, name: &str) -> eyre::Result<PathBuf> {
    // Validate it's a git repo
    gix::open(repo_path)?;

    let worktree_path = workspace_dir.join(name);
    if worktree_path.exists() {
        return Err(eyre::eyre!(
            "workspace '{}' already exists; pick a different name or remove it with `dc down`",
            worktree_path.display()
        ));
    }

    duct::cmd!("git", "worktree", "add", "--detach", &worktree_path, "HEAD")
        .dir(repo_path)
        .stdout_capture()
        .run()?;

    Ok(worktree_path)
}

pub fn generate_name(base: &str) -> String {
    const ALPHABET: &[char] = &[
        '0', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'j',
        'k', 'm', 'n', 'p', 'q', 'r', 's', 't', 'w', 'x', 'y', 'z',
    ];
    let id = nanoid!(3, ALPHABET);
    let ts = Zoned::now().strftime("%m%d-%H%M").to_string();

    format!("{base}-{ts}-{id}")
}
