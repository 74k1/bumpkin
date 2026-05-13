use std::{fs, path::{Path, PathBuf}};

#[derive(Debug)]
pub struct Candidate {
    pub attr_path: String,
    pub file: PathBuf,
    pub has_update_script: bool,
}

pub fn by_maintainer(root: &Path, maintainer: &str) -> Result<Vec<Candidate>, String> {
    let pkgs_dir = root.join("pkgs");
    let mut files = Vec::new();
    collect_nix_files(&pkgs_dir, &mut files)?;

    let mut out = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file).map_err(|e| format!("read {}: {e}", file.display()))?;
        if !text.contains(maintainer) {
            continue;
        }
        out.push(Candidate {
            attr_path: attr_from_pkg_path(&pkgs_dir, &file),
            file,
            has_update_script: text.contains("passthru.updateScript") || text.contains("updateScript"),
        });
    }
    out.sort_by(|a, b| a.attr_path.cmp(&b.attr_path));
    Ok(out)
}

fn collect_nix_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("read dir {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("read dir entry: {e}"))?;
        let path = entry.path();
        if path.is_dir() {
            collect_nix_files(&path, out)?;
        } else if path.extension().and_then(|s| s.to_str()) == Some("nix") {
            out.push(path);
        }
    }
    Ok(())
}

fn attr_from_pkg_path(pkgs_dir: &Path, file: &Path) -> String {
    let rel = file.strip_prefix(pkgs_dir).unwrap_or(file);
    if rel.file_name().and_then(|s| s.to_str()) == Some("default.nix") {
        rel.parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    } else {
        rel.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    }
}
