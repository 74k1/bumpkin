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

pub fn clean(root: &Path) -> Result<bool, String> {
    Ok(changed_paths(root)?.is_empty())
}

pub fn changed_paths(root: &Path) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .args(["status", "--porcelain=1", "--untracked-files=all"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("git status: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }

    let mut paths = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if line.len() < 4 {
            continue;
        }
        let path = &line[3..];
        if let Some((_, to)) = path.split_once(" -> ") {
            paths.push(to.to_string());
        } else {
            paths.push(path.to_string());
        }
    }
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

pub fn commit_paths(
    root: &Path,
    paths: &[String],
    title: &str,
    body: &str,
    signed: bool,
) -> Result<(), String> {
    if paths.is_empty() {
        return Err("no paths to commit".to_string());
    }

    let mut add = Command::new("git");
    add.arg("add").arg("--").args(paths).current_dir(root);
    let status = add.status().map_err(|e| format!("git add: {e}"))?;
    if !status.success() {
        return Err(format!("git add exited with {status}"));
    }

    if signed {
        run(root, &["commit", "-S", "-m", title, "-m", body])
    } else {
        run(root, &["commit", "-m", title, "-m", body])
    }
}

pub fn status(cmd: &mut Command) -> Result<bool, String> {
    cmd.status()
        .map(|s| s.success())
        .map_err(|e| format!("run command: {e}"))
}
