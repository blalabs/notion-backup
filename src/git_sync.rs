//! Git operations for committing the backup tree.

use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, Result};
use log::info;

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Initialise a git repository at `repo` if one does not exist.
pub fn ensure_repo(repo: &Path) -> Result<()> {
    std::fs::create_dir_all(repo)?;
    if repo.join(".git").exists() {
        return Ok(());
    }
    info!("Initialising git repository at {}", repo.display());
    git(repo, &["init"])?;
    Ok(())
}

/// Return true if there are staged or unstaged changes to commit.
pub fn has_changes(repo: &Path) -> Result<bool> {
    Ok(!git(repo, &["status", "--porcelain"])?.trim().is_empty())
}

/// Stage everything and commit. Returns false if there was nothing to commit.
pub fn commit_all(repo: &Path, message: &str) -> Result<bool> {
    git(repo, &["add", "-A"])?;
    if !has_changes(repo)? {
        info!("No changes to commit");
        return Ok(false);
    }
    git(repo, &["commit", "-m", message])?;
    info!("Committed: {message}");
    Ok(true)
}

/// Push the current branch to `remote`, adding it as `origin` if no remotes
/// are configured yet.
pub fn push(repo: &Path, remote: &str) -> Result<()> {
    let remotes = git(repo, &["remote"])?;
    if !remotes.split_whitespace().any(|r| r == "origin") {
        info!("Adding origin remote {remote}");
        git(repo, &["remote", "add", "origin", remote])?;
    }
    let branch = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    info!("Pushing {branch} to origin");
    git(repo, &["push", "-u", "origin", &branch])?;
    Ok(())
}
