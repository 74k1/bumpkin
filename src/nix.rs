use std::{path::Path, process::Command};

use crate::git;

pub fn current_system() -> Result<String, String> {
    let out = Command::new("nix")
        .args(["eval", "--raw", "--impure", "--expr", "builtins.currentSystem"])
        .output()
        .map_err(|e| format!("run nix eval: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    String::from_utf8(out.stdout)
        .map(|s| s.trim().to_string())
        .map_err(|e| format!("decode nix output: {e}"))
}

pub fn package_version(root: &Path, package: &str) -> Result<String, String> {
    let out = Command::new("nix")
        .args(["eval", "--raw", &format!("path:{}#{package}.version", root.display())])
        .output()
        .map_err(|e| format!("eval package version: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    String::from_utf8(out.stdout)
        .map(|s| s.trim().to_string())
        .map_err(|e| format!("decode package version: {e}"))
}

pub fn build_package(root: &Path, package: &str) -> Result<bool, String> {
    git::status(Command::new("nix").args(["build", "--no-link", &format!("path:{}#{package}", root.display())]))
}

pub fn build_update_script(root: &Path, package: &str) -> Result<String, String> {
    if !root.join("flake.nix").exists() {
        return Err("expected a flake root containing flake.nix".to_string());
    }
    let system = current_system()?;
    let installable = format!(
        "path:{}#packages.{}.{}.passthru.updateScript",
        root.display(), system, package
    );
    let out = Command::new("nix")
        .args(["build", "--no-link", "--print-out-paths", "--impure", &installable])
        .current_dir(root)
        .output()
        .map_err(|e| format!("build updateScript: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "build updateScript exited with {}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
