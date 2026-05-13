use std::{path::Path, process::Command};

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    let status = Command::new("git")
        .args(args)
        .current_dir(root)
        .status()
        .map_err(|e| format!("run git {args:?}: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("git {args:?} exited with {status}")) }
}

pub fn clean(root: &Path) -> Result<bool, String> {
    status(Command::new("git").args(["diff-index", "--quiet", "HEAD", "--"]).current_dir(root))
}

pub fn diff(root: &Path) -> Result<String, String> {
    output(Command::new("git").args(["diff"]).current_dir(root))
}

pub fn diff_stat(root: &Path) -> Result<String, String> {
    output(Command::new("git").args(["diff", "--stat"]).current_dir(root))
}

pub fn commit(root: &Path, title: &str, body: &str, signed: bool) -> Result<(), String> {
    run(root, &["add", "."])?;
    if signed {
        run(root, &["commit", "-S", "-m", title, "-m", body])
    } else {
        run(root, &["commit", "-m", title, "-m", body])
    }
}

pub fn status(cmd: &mut Command) -> Result<bool, String> {
    cmd.status().map(|s| s.success()).map_err(|e| format!("run command: {e}"))
}

pub fn output(cmd: &mut Command) -> Result<String, String> {
    let out = cmd.output().map_err(|e| format!("run command: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    String::from_utf8(out.stdout).map_err(|e| format!("decode command output: {e}"))
}
