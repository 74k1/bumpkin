use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::nix;

#[derive(Debug)]
pub struct Candidate {
    pub attr_path: String,
    pub file: Option<PathBuf>,
    pub backend: Backend,
}

#[derive(Clone, Debug)]
pub enum Backend {
    UpdateScript,
    FetchFromGitHub,
    GitHubReleaseAsset,
    NativeFetcher(&'static str),
    Ecosystem(&'static str),
    Unknown,
}

impl Backend {
    pub fn name(&self) -> &'static str {
        match self {
            Self::UpdateScript => "update-script",
            Self::FetchFromGitHub => "fetchFromGitHub",
            Self::GitHubReleaseAsset => "github-release-asset",
            Self::NativeFetcher(name) | Self::Ecosystem(name) => name,
            Self::Unknown => "unknown",
        }
    }

    pub fn is_runnable(&self) -> bool {
        matches!(self, Self::UpdateScript)
    }

    /// Native update = discover tags via `git ls-remote` on the evaluated src
    /// URL, then let Nix compute src/dependency hashes. That works for any
    /// git-hosted source, so most fetchers qualify; registry-based sources
    /// (PyPI, crates.io, ...) and exotic VCSes still need an updateScript.
    pub fn is_native_candidate(&self) -> bool {
        match self {
            Self::FetchFromGitHub | Self::GitHubReleaseAsset => true,
            Self::NativeFetcher(name) => matches!(
                *name,
                "fetchurl"
                    | "fetchzip"
                    | "fetchgit"
                    | "builtins.fetchGit"
                    | "fetchFromGitLab"
                    | "fetchFromGitea"
                    | "fetchFromForgejo"
                    | "fetchFromSourcehut"
                    | "fetchFromBitbucket"
                    | "fetchFromCodeberg"
                    | "fetchFromGitiles"
                    | "fetchFromRepoOrCz"
                    | "fetchFromSavannah"
            ),
            Self::Ecosystem(name) => matches!(
                *name,
                "buildGoModule" | "rust/cargo" | "npm" | "pnpm" | "yarn" | "maven" | "mix"
            ),
            _ => false,
        }
    }

    pub fn note(&self) -> &'static str {
        match self {
            Self::UpdateScript => "run update script",
            Self::FetchFromGitHub | Self::GitHubReleaseAsset => {
                "native updater: git tags -> version; Nix computes src/dependency hashes"
            }
            Self::NativeFetcher(_) | Self::Ecosystem(_) => {
                if self.is_native_candidate() {
                    "native updater: git tags -> version; Nix computes src/dependency hashes"
                } else {
                    "skip: not git-hosted; add an updateScript"
                }
            }
            Self::Unknown => "skip: needs updateScript or supported native fetcher",
        }
    }
}

pub fn file_for_attr(root: &Path, attr: &str) -> Option<PathBuf> {
    let pkgs = root.join("pkgs");

    for candidate in common_attr_paths(&pkgs, attr) {
        if candidate.exists() {
            return Some(candidate);
        }
    }

    let mut files = Vec::new();
    collect_nix_files(&pkgs, &mut files).ok()?;
    files
        .into_iter()
        .find(|file| attr_from_pkg_path(&pkgs, file) == attr)
}

pub fn by_maintainer(root: &Path, maintainer: &str) -> Result<Vec<Candidate>, String> {
    match by_maintainer_evaluated(root, maintainer) {
        Ok(candidates) => Ok(candidates),
        Err(eval_err) => match by_maintainer_source_scan(root, maintainer) {
            Ok(candidates) => Ok(candidates),
            Err(scan_err) => Err(format!(
                "maintainer evaluation failed:\n{eval_err}\n\nsource scan fallback failed:\n{scan_err}"
            )),
        },
    }
}

fn by_maintainer_evaluated(root: &Path, maintainer: &str) -> Result<Vec<Candidate>, String> {
    let packages = nix::packages_by_maintainer(root, maintainer)?;
    let mut out = Vec::new();

    for package in packages {
        let file = package
            .position
            .as_deref()
            .and_then(|position| file_from_position(root, position))
            .or_else(|| file_for_attr(root, &package.attr_path));

        let backend = if package.has_update_script {
            Backend::UpdateScript
        } else if let Some(file) = file.as_deref() {
            fs::read_to_string(file)
                .map(|text| classify(&text))
                .unwrap_or(Backend::Unknown)
        } else {
            Backend::Unknown
        };

        out.push(Candidate {
            attr_path: package.attr_path,
            file,
            backend,
        });
    }

    out.sort_by(|a, b| a.attr_path.cmp(&b.attr_path));
    Ok(out)
}

fn by_maintainer_source_scan(root: &Path, maintainer: &str) -> Result<Vec<Candidate>, String> {
    let pkgs_dir = root.join("pkgs");
    let mut files = Vec::new();
    collect_nix_files(&pkgs_dir, &mut files)?;

    let mut out = Vec::new();
    for file in files {
        let text =
            fs::read_to_string(&file).map_err(|e| format!("read {}: {e}", file.display()))?;
        if !has_maintainer(&text, maintainer) {
            continue;
        }
        out.push(Candidate {
            attr_path: attr_from_pkg_path(&pkgs_dir, &file),
            file: Some(file),
            backend: classify(&text),
        });
    }
    out.sort_by(|a, b| a.attr_path.cmp(&b.attr_path));
    Ok(out)
}

fn classify(text: &str) -> Backend {
    let text = &without_line_comments(text);
    if text.contains("passthru.updateScript") || text.contains("updateScript") {
        return Backend::UpdateScript;
    }

    // Ecosystem/build helpers with extra dependency hashes. These usually wrap
    // one of the source fetchers below, so classify them first.
    if text.contains("buildGoModule") {
        return Backend::Ecosystem("buildGoModule");
    }
    if text.contains("fetchCargoVendor") || text.contains("cargoHash") || text.contains("cargoDeps")
    {
        return Backend::Ecosystem("rust/cargo");
    }
    if text.contains("fetchNpmDeps") || text.contains("npmDepsHash") || text.contains("npmDeps") {
        return Backend::Ecosystem("npm");
    }
    if text.contains("fetchPnpmDeps") {
        return Backend::Ecosystem("pnpm");
    }
    if text.contains("fetchYarnDeps") || text.contains("fetchYarnBerryDeps") {
        return Backend::Ecosystem("yarn");
    }
    if text.contains("fetchPypi") {
        return Backend::Ecosystem("fetchPypi");
    }
    if text.contains("fetchCrate") {
        return Backend::Ecosystem("fetchCrate");
    }
    if text.contains("fetchMavenDeps") || text.contains("fetchedMavenDeps") {
        return Backend::Ecosystem("maven");
    }
    if text.contains("fetchMixDeps") {
        return Backend::Ecosystem("mix");
    }
    if text.contains("fetchRebar3Deps") {
        return Backend::Ecosystem("rebar3");
    }

    if text.contains("fetchFromGitHub") {
        return Backend::FetchFromGitHub;
    }
    if (text.contains("fetchurl") || text.contains("fetchzip"))
        && text.contains("github.com/")
        && text.contains("/releases/download/")
    {
        return Backend::GitHubReleaseAsset;
    }

    for (needle, name) in FETCHERS {
        if text.contains(needle) {
            return Backend::NativeFetcher(name);
        }
    }
    Backend::Unknown
}

fn without_line_comments(text: &str) -> String {
    text.lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Check whether `maintainer` appears inside a `maintainers = [...]` block.
///
/// This avoids false positives from maintainer handles that show up in URLs,
/// comments, or source strings outside the maintainers declaration.
///
/// Handles both `maintainers = [ ... ]` and `maintainers = with lib.maintainers; [ ... ]`.
fn has_maintainer(text: &str, maintainer: &str) -> bool {
    let mut rest = text;
    while let Some(pos) = rest.find("maintainers") {
        rest = &rest[pos + "maintainers".len()..];

        // Scan forward (up to 1000 chars) to find a maintainer list `[...]`.
        // Skip over intermediate `;` characters that separate non-list
        // references (e.g. `lib.maintainers`).
        let mut scan = rest;
        loop {
            let window = &scan[..scan.len().min(1000)];
            let semi = window.find(';');
            let bracket = window.find('[');

            match (semi, bracket) {
                (Some(s), Some(b)) if s < b => {
                    // ';' before '[' → a reference like `lib.maintainers`.
                    // Skip past the ';' and keep scanning.
                    scan = &scan[s + 1..];
                }
                (_, Some(b)) => {
                    // '[' comes before any ';' (or there is no ';').
                    // This is the maintainers declaration list.
                    let content = &scan[b + 1..];
                    let mut depth = 1;
                    let mut end = 0;
                    for (i, ch) in content.char_indices() {
                        match ch {
                            '[' => depth += 1,
                            ']' => {
                                depth -= 1;
                                if depth == 0 {
                                    end = i;
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    if content[..end].contains(maintainer) {
                        return true;
                    }
                    rest = &scan[b + 1 + end..];
                    break;
                }
                (Some(s), None) => {
                    // Only ';', no '[' in window. Skip past this occurrence.
                    rest = &scan[s + 1..];
                    break;
                }
                (None, None) => {
                    // Neither '[' nor ';' in window. Give up.
                    return false;
                }
            }
        }
    }
    false
}

const FETCHERS: &[(&str, &str)] = &[
    ("fetchurl", "fetchurl"),
    ("fetchzip", "fetchzip"),
    ("fetchpatch", "fetchpatch"),
    ("fetchDebianPatch", "fetchDebianPatch"),
    ("fetchRadiclePatch", "fetchRadiclePatch"),
    ("fetchgit", "fetchgit"),
    ("fetchGit", "builtins.fetchGit"),
    ("fetchTree", "builtins.fetchTree"),
    ("fetchTarball", "builtins.fetchTarball"),
    ("fetchClosure", "fetchClosure"),
    ("fetchFromGitLab", "fetchFromGitLab"),
    ("fetchFromGitea", "fetchFromGitea"),
    ("fetchFromForgejo", "fetchFromForgejo"),
    ("fetchFromSourcehut", "fetchFromSourcehut"),
    ("fetchFromBitbucket", "fetchFromBitbucket"),
    ("fetchFromCodeberg", "fetchFromCodeberg"),
    ("fetchFromGitiles", "fetchFromGitiles"),
    ("fetchFromRepoOrCz", "fetchFromRepoOrCz"),
    ("fetchFromSavannah", "fetchFromSavannah"),
    ("fetchFromRadicle", "fetchFromRadicle"),
    ("fetchcvs", "fetchcvs"),
    ("fetchsvn", "fetchsvn"),
    ("fetchhg", "fetchhg"),
    ("fetchfossil", "fetchfossil"),
    ("fetchbzr", "fetchbzr"),
    ("fetchtorrent", "fetchtorrent"),
    ("fetchFirefoxAddon", "fetchFirefoxAddon"),
    ("fetchNuGet", "fetchNuGet"),
    ("fetchDartDeps", "fetchDartDeps"),
    ("fetchHex", "fetchHex"),
    ("fetchPackagist", "fetchPackagist"),
    ("dockerTools.pullImage", "dockerTools.pullImage"),
    ("fetchdocker", "dockerTools.pullImage"),
];

fn common_attr_paths(pkgs: &Path, attr: &str) -> Vec<PathBuf> {
    let prefix = &attr[..attr.len().min(2)];
    vec![
        pkgs.join("by-name")
            .join(prefix)
            .join(attr)
            .join("package.nix"),
        pkgs.join("by-name")
            .join(prefix)
            .join(attr)
            .join("default.nix"),
        pkgs.join(prefix).join(format!("{attr}.nix")),
        pkgs.join(prefix).join(attr).join("default.nix"),
        pkgs.join(prefix).join(attr).join("package.nix"),
    ]
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
    let file_name = rel.file_name().and_then(|s| s.to_str());
    if matches!(file_name, Some("default.nix" | "package.nix")) {
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

fn file_from_position(root: &Path, position: &str) -> Option<PathBuf> {
    let file = position_file(position)?;
    if file.starts_with(root) && file.exists() {
        return Some(file);
    }

    let parts = file.components().collect::<Vec<_>>();
    let pkgs_index = parts
        .iter()
        .position(|part| part.as_os_str().to_string_lossy() == "pkgs")?;
    let mut mapped = root.join("pkgs");
    for part in &parts[pkgs_index + 1..] {
        mapped.push(part.as_os_str());
    }
    mapped.exists().then_some(mapped)
}

fn position_file(position: &str) -> Option<PathBuf> {
    let (file, _) = position.rsplit_once(':')?;
    Some(PathBuf::from(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_prefers_update_script() {
        let backend = classify(r#"{ passthru.updateScript = ./update.sh; src = fetchurl {}; }"#);
        assert_eq!(backend.name(), "update-script");
    }

    #[test]
    fn classify_common_fetchers() {
        assert_eq!(
            classify("src = fetchFromGitHub {};").name(),
            "fetchFromGitHub"
        );
        assert_eq!(
            classify(r#"src = fetchurl { url = "https://github.com/o/r/releases/download/v${version}/x"; };"#).name(),
            "github-release-asset"
        );
        assert_eq!(classify("src = fetchtorrent {};").name(), "fetchtorrent");
    }

    #[test]
    fn attr_from_paths_supports_default_and_by_name_package_files() {
        let pkgs = Path::new("/repo/pkgs");
        assert_eq!(
            attr_from_pkg_path(pkgs, Path::new("/repo/pkgs/ar/arcbrush.nix")),
            "arcbrush"
        );
        assert_eq!(
            attr_from_pkg_path(pkgs, Path::new("/repo/pkgs/li/lidarr/default.nix")),
            "lidarr"
        );
        assert_eq!(
            attr_from_pkg_path(pkgs, Path::new("/repo/pkgs/by-name/fo/foo/package.nix")),
            "foo"
        );
    }

    #[test]
    fn position_file_strips_line_and_column() {
        assert_eq!(
            position_file("/nix/store/source/pkgs/by-name/fo/foo/package.nix:12"),
            Some(PathBuf::from(
                "/nix/store/source/pkgs/by-name/fo/foo/package.nix"
            ))
        );
    }

    #[test]
    fn has_maintainer_finds_handle_in_maintainers_list() {
        assert!(has_maintainer(
            "maintainers = with lib.maintainers; [ 74k1 jtojnar ];",
            "74k1"
        ));
    }

    #[test]
    fn has_maintainer_avoids_false_positive_in_urls() {
        // A maintainer handle that appears in a URL or comment outside the
        // maintainers list should not match.
        assert!(!has_maintainer(
            "url = \"https://github.com/74k1/tixpkgs\";\nmaintainers = with lib.maintainers; [ jtojnar ];",
            "74k1"
        ));
    }

    #[test]
    fn has_maintainer_finds_handle_in_multiple_blocks() {
        assert!(has_maintainer(
            "maintainers = [ someone ];\n# other stuff\nmaintainers = [ 74k1 ];",
            "74k1"
        ));
    }

    #[test]
    fn has_maintainer_handles_lib_reference() {
        // `lib.maintainers` is a reference, not a declaration. The actual
        // maintainers list follows after the `;` in a second `[ ... ]`.
        assert!(has_maintainer(
            "maintainers = with lib.maintainers; [ 74k1 jtojnar ];",
            "74k1"
        ));
    }

    #[test]
    fn has_maintainer_returns_false_for_empty_list() {
        assert!(!has_maintainer("maintainers = [ ];", "74k1"));
    }
}
