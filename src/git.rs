use std::{path::Path, process::Command};

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    let status = Command::new("git")
        .args(args)
        .current_dir(root)
        .status()
        .map_err(|e| format!("run git {args:?}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("git {args:?} exited with {status}"))
    }
}

/// Run git and return its trimmed stdout.
fn output(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("run git {args:?}: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

pub fn clean(root: &Path) -> Result<bool, String> {
    Ok(changed_paths(root)?.is_empty())
}

pub fn changed_paths(root: &Path) -> Result<Vec<String>, String> {
    let status = output(root, &["status", "--porcelain=1", "--untracked-files=all"])?;
    let mut paths: Vec<String> = status
        .lines()
        .filter(|line| line.len() >= 4)
        .map(|line| line[3..].split_once(" -> ").map_or(&line[3..], |(_, to)| to).to_string())
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

pub fn print_diff(root: &Path) -> Result<(), String> {
    run(root, &["diff", "--color=always"])
}

pub fn print_diff_stat(root: &Path) -> Result<(), String> {
    run(root, &["diff", "--stat", "--color=always"])
}

/// Commit `paths`. `config` entries are passed as one-off `git -c key=value`,
/// so signing settings never end up in the repository's config.
pub fn commit_paths(
    root: &Path,
    paths: &[String],
    title: &str,
    body: &str,
    signed: bool,
    config: &[(&str, Option<&str>)],
) -> Result<(), String> {
    if paths.is_empty() {
        return Err("no paths to commit".to_string());
    }
    let mut add = vec!["add", "--"];
    add.extend(paths.iter().map(String::as_str));
    run(root, &add)?;

    let config: Vec<String> = config
        .iter()
        .filter_map(|(key, value)| value.map(|value| format!("{key}={value}")))
        .collect();
    let mut commit: Vec<&str> = config.iter().flat_map(|c| ["-c", c.as_str()]).collect();
    commit.extend(["commit", "-m", title, "-m", body]);
    if signed {
        commit.push("-S");
    }
    run(root, &commit)
}

pub fn current_branch(root: &Path) -> Result<String, String> {
    output(root, &["branch", "--show-current"])
}

/// Switch to `branch`, dropping whatever a failed update left behind. Commit
/// mode starts from a clean tree, so anything dropped here is bumpkin's own.
pub fn checkout_branch(root: &Path, branch: &str) -> Result<(), String> {
    run(root, &["checkout", "-f", branch])?;
    run(root, &["clean", "-fdq"])
}

pub fn create_branch(root: &Path, branch: &str) -> Result<(), String> {
    run(root, &["checkout", "-b", branch])
}

pub fn delete_branch(root: &Path, branch: &str) -> Result<(), String> {
    run(root, &["branch", "-D", branch])
}

/// Force-push: per-package branches are recreated from the main branch on
/// every run, so an open PR's branch has to be overwritten to update it.
pub fn push_branch(root: &Path, branch: &str) -> Result<(), String> {
    run(root, &["push", "--force", "origin", branch])
}

pub fn pull_ff_only(root: &Path) -> Result<(), String> {
    run(root, &["pull", "--ff-only"])
}

/// Get the fetch URL of the `origin` remote.
pub fn remote_url(root: &Path) -> Result<String, String> {
    output(root, &["remote", "get-url", "origin"])
}

/// Whether `origin/<branch>` (as of the last fetch) already has the same tree.
pub fn matches_remote(root: &Path, branch: &str) -> bool {
    output(root, &["diff", "--quiet", &format!("origin/{branch}"), branch]).is_ok()
}
