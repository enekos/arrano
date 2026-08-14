//! Local checkout discovery and worktree management for reviewing PRs with
//! real code context. Follows the zz convention: worktrees live in a sibling
//! `<repo>-worktrees/<branch-slug>` directory ($ZZ_WORKTREE_DIR overrides).

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("failed to spawn git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Map "owner/name" to a local checkout by scanning the configured repo
/// roots: $ARRANO_REPO_ROOTS (colon-separated directories whose children are
/// repos), falling back to common locations under $HOME.
pub fn local_repo_path(repo: &str) -> Option<PathBuf> {
    let name = repo.rsplit('/').next()?;
    let home = std::env::var("HOME").ok()?;
    let roots = std::env::var("ARRANO_REPO_ROOTS")
        .unwrap_or_else(|_| format!("{home}/src:{home}/code:{home}/projects:{home}/dev"));
    for root in roots.split(':').filter(|r| !r.is_empty()) {
        let p = PathBuf::from(root).join(name);
        if p.join(".git").exists() {
            return Some(p);
        }
    }
    None
}

pub fn slug(branch: &str) -> String {
    branch.replace('/', "-")
}

/// All checkouts (main + worktrees) of a repo as (path, branch).
fn checkouts(repo_path: &Path) -> Vec<(PathBuf, String)> {
    let Ok(out) = git(repo_path, &["worktree", "list", "--porcelain"]) else {
        return Vec::new();
    };
    let mut result = Vec::new();
    let mut path: Option<PathBuf> = None;
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(p));
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            if let Some(p) = path.take() {
                result.push((p, b.to_string()));
            }
        }
    }
    result
}

/// Any existing checkout (main or worktree) that has `branch` checked out.
pub fn branch_checkout(repo: &str, branch: &str) -> Option<PathBuf> {
    let rp = local_repo_path(repo)?;
    checkouts(&rp).into_iter().find(|(_, b)| b == branch).map(|(p, _)| p)
}

fn worktree_target(repo_path: &Path, branch: &str) -> PathBuf {
    let name = repo_path.file_name().and_then(|n| n.to_str()).unwrap_or("repo");
    if let Ok(base) = std::env::var("ZZ_WORKTREE_DIR") {
        return PathBuf::from(base).join(name).join(slug(branch));
    }
    let parent = repo_path.parent().unwrap_or(Path::new("."));
    parent.join(format!("{name}-worktrees")).join(slug(branch))
}

/// Get (or create) a checkout of the PR's branch. Reuses any existing
/// checkout on that branch; otherwise fetches and adds a worktree.
pub fn ensure_worktree(repo: &str, branch: &str) -> Result<PathBuf, String> {
    let rp = local_repo_path(repo)
        .ok_or_else(|| format!("no local checkout found for {repo}"))?;
    if let Some(p) = checkouts(&rp).into_iter().find(|(_, b)| b == branch).map(|(p, _)| p) {
        return Ok(p);
    }
    git(&rp, &["fetch", "origin", branch])
        .map_err(|e| format!("fetch origin {branch}: {e}"))?;
    let wt = worktree_target(&rp, branch);
    if let Some(parent) = wt.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let wt_str = wt.to_string_lossy().to_string();
    let local_exists =
        git(&rp, &["rev-parse", "--verify", &format!("refs/heads/{branch}")]).is_ok();
    if local_exists {
        git(&rp, &["worktree", "add", &wt_str, branch])
            .map_err(|e| format!("worktree add: {e}"))?;
    } else {
        git(
            &rp,
            &[
                "worktree",
                "add",
                "--track",
                "-b",
                branch,
                &wt_str,
                &format!("origin/{branch}"),
            ],
        )
        .map_err(|e| format!("worktree add: {e}"))?;
    }
    Ok(wt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_flattens_slashes() {
        assert_eq!(slug("feat/abc-123/import"), "feat-abc-123-import");
    }
}
