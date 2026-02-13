use std::fmt;
use std::path::Path;

use owo_colors::OwoColorize;

#[derive(Debug, Default)]
pub struct GitStatus {
    pub ahead: usize,
    pub behind: usize,
    pub staged: usize,
    pub modified: usize,
    pub deleted: usize,
    pub untracked: usize,
    pub conflicted: usize,
    pub renamed: usize,
}

impl GitStatus {
    pub async fn fetch(path: &Path) -> eyre::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || fetch_sync(&path)).await?
    }

    pub fn is_dirty(&self) -> bool {
        self.staged + self.modified + self.deleted + self.untracked + self.conflicted + self.renamed
            > 0
    }
}

fn fetch_sync(path: &Path) -> eyre::Result<GitStatus> {
    let repo = gix::open(path)?;
    let mut gs = GitStatus::default();

    let (ahead, behind) = ahead_behind(&repo).unwrap_or((0, 0));
    gs.ahead = ahead;
    gs.behind = behind;

    let iter = repo.status(gix::progress::Discard)?.into_iter(Vec::new())?;

    for item in iter {
        let item = item?;
        match &item {
            gix::status::Item::IndexWorktree(iw) => {
                use gix::status::index_worktree::iter::Summary;
                if let Some(summary) = iw.summary() {
                    match summary {
                        Summary::Modified | Summary::TypeChange => gs.modified += 1,
                        Summary::Removed => gs.deleted += 1,
                        Summary::Added => gs.untracked += 1,
                        Summary::Conflict => gs.conflicted += 1,
                        Summary::Renamed => gs.renamed += 1,
                        Summary::Copied | Summary::IntentToAdd => {}
                    }
                }
            }
            gix::status::Item::TreeIndex(change) => {
                gs.staged += 1;
                match change {
                    gix::diff::index::ChangeRef::Deletion { .. } => gs.deleted += 1,
                    gix::diff::index::ChangeRef::Rewrite { copy: false, .. } => gs.renamed += 1,
                    _ => {}
                }
            }
        }
    }

    Ok(gs)
}

fn ahead_behind(repo: &gix::Repository) -> eyre::Result<(usize, usize)> {
    let head = repo.head()?;
    let head_id = head
        .id()
        .ok_or_else(|| eyre::eyre!("unborn HEAD"))?
        .detach();

    let referent = head
        .try_into_referent()
        .ok_or_else(|| eyre::eyre!("detached HEAD"))?;
    let tracking_ref_name = referent
        .remote_tracking_ref_name(gix::remote::Direction::Fetch)
        .ok_or_else(|| eyre::eyre!("no tracking branch"))??;
    let tracking_id = repo
        .find_reference(tracking_ref_name.as_ref())?
        .id()
        .detach();

    if head_id == tracking_id {
        return Ok((0, 0));
    }

    let ahead = repo
        .rev_walk([head_id])
        .with_hidden([tracking_id])
        .all()?
        .filter_map(Result::ok)
        .count();

    let behind = repo
        .rev_walk([tracking_id])
        .with_hidden([head_id])
        .all()?
        .filter_map(Result::ok)
        .count();

    Ok((ahead, behind))
}

impl fmt::Display for GitStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = String::new();

        if self.ahead > 0 {
            s.push('⇡');
        }
        if self.behind > 0 {
            s.push('⇣');
        }
        if self.staged > 0 {
            s.push('+');
        }
        if self.modified > 0 {
            s.push('!');
        }
        if self.deleted > 0 {
            s.push('✘');
        }
        if self.renamed > 0 {
            s.push('»');
        }
        if self.untracked > 0 {
            s.push('?');
        }
        if self.conflicted > 0 {
            s.push('=');
        }

        write!(f, "{}", s.red())
    }
}
