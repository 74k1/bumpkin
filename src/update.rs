use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{git, nix, packages};

pub struct CommitOptions {
    pub commit: bool,
    pub signed: bool,
    pub signing_key: Option<String>,
    pub gpg_format: Option<String>,
}

pub fn dry_run_package(root: &Path, package: &str) -> Result<(), String> {
    println!("\n==> {package}");
    let worktree = TempWorktree::create(root)?;
    run_update_script(worktree.path(), package)?;

    if git::clean(worktree.path())? {
        println!("No changes.");
        return Ok(());
    }

    println!("--- diffstat ---");
    git::print_diff_stat(worktree.path())?;
    println!("--- diff ---");
    git::print_diff(worktree.path())?;
    println!("--- build ---");
    if nix::build_package(worktree.path(), package)? {
        println!("build: ok");
    } else {
        println!("build: failed");
    }
    Ok(())
}

pub fn update_maintainer(root: &Path, maintainer: &str) -> Result<(), String> {
    let candidates = packages::by_maintainer(root, maintainer)?;
    if candidates.is_empty() {
        println!("No packages found for maintainer `{maintainer}`.");
        return Ok(());
    }

    let mut summary = BatchSummary::default();
    for candidate in candidates {
        println!(
            "\n==> {} ({})",
            candidate.attr_path,
            candidate.backend.name()
        );
        if !(candidate.backend.is_runnable() || candidate.backend.is_native_candidate()) {
            println!("skipped: {}", candidate.backend.note());
            summary.skipped += 1;
            continue;
        }

        match batch_update_one(root, &candidate.attr_path) {
            Ok(BatchOutcome::NoChanges) => {
                println!("no changes");
                summary.no_changes += 1;
            }
            Ok(BatchOutcome::UpdatedBuildOk) => {
                println!("updated: build ok");
                summary.updated += 1;
            }
            Ok(BatchOutcome::UpdatedBuildFailed) => {
                println!("updated: build failed");
                summary.build_failed += 1;
            }
            Err(err) => {
                println!("failed: {err}");
                summary.failed += 1;
            }
        }
    }

    println!("\n--- summary ---");
    println!("updated/build ok: {}", summary.updated);
    println!("updated/build failed: {}", summary.build_failed);
    println!("no changes: {}", summary.no_changes);
    println!("failed: {}", summary.failed);
    println!("skipped: {}", summary.skipped);
    Ok(())
}

fn batch_update_one(root: &Path, package: &str) -> Result<BatchOutcome, String> {
    let worktree = TempWorktree::create(root)?;
    run_update_script(worktree.path(), package)?;
    if git::clean(worktree.path())? {
        return Ok(BatchOutcome::NoChanges);
    }
    if nix::build_package(worktree.path(), package)? {
        Ok(BatchOutcome::UpdatedBuildOk)
    } else {
        Ok(BatchOutcome::UpdatedBuildFailed)
    }
}

#[derive(Default)]
struct BatchSummary {
    updated: usize,
    build_failed: usize,
    no_changes: usize,
    failed: usize,
    skipped: usize,
}

enum BatchOutcome {
    NoChanges,
    UpdatedBuildOk,
    UpdatedBuildFailed,
}

pub fn update_package(root: &Path, package: &str, commit: CommitOptions) -> Result<(), String> {
    if !git::clean(root)? {
        return Err(
            "working tree has uncommitted or untracked changes; commit/stash them before update"
                .to_string(),
        );
    }

    let old_version = nix::package_version(root, package).unwrap_or_else(|_| "unknown".to_string());
    run_update_script(root, package)?;

    let changed_paths = git::changed_paths(root)?;
    if changed_paths.is_empty() {
        println!("No diff after running update script.");
        return Ok(());
    }

    let new_version = nix::package_version(root, package).unwrap_or_else(|_| "unknown".to_string());
    let (title, body) = pr_text(package, &old_version, &new_version);

    println!("\n--- suggested PR title ---\n{title}");
    println!("\n--- suggested PR body ---\n{body}");
    println!("\n--- diffstat ---");
    git::print_diff_stat(root)?;

    println!("\n--- build ---");
    if !nix::build_package(root, package)? {
        return Err("build failed; leaving changes in working tree".to_string());
    }

    if commit.commit {
        if let Some(format) = commit.gpg_format.as_deref() {
            git::run(root, &["config", "gpg.format", format])?;
        }
        if let Some(key) = commit.signing_key.as_deref() {
            git::run(root, &["config", "user.signingkey", key])?;
        }
        git::commit_paths(root, &changed_paths, &title, &body, commit.signed)?;
        println!(
            "Committed{}: {title}",
            if commit.signed { " signed" } else { "" }
        );
    } else {
        println!("\nNo commit created. Re-run with --commit to commit, or --commit --signed for a signed commit.");
    }

    Ok(())
}

pub fn run_update_script_cmd(args: &[String]) -> Result<(), String> {
    let mut package = None;
    let mut root = env::current_dir().map_err(|e| format!("current dir: {e}"))?;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--package" | "-p" => {
                i += 1;
                package = args.get(i).cloned();
            }
            "--root" | "-C" => {
                i += 1;
                root = PathBuf::from(args.get(i).ok_or("missing value for --root")?);
            }
            other => return Err(format!("unknown run-update-script argument: {other}")),
        }
        i += 1;
    }
    let package =
        package.ok_or("usage: bumpkin run-update-script --package <attr> [--root <repo>]")?;
    run_update_script(&root, &package)
}

fn run_update_script(root: &Path, package: &str) -> Result<(), String> {
    println!("Trying package-owned updateScript for {package}...");
    match nix::build_update_script(root, package) {
        Ok(script_out) => {
            let script = find_executable_in_output(Path::new(&script_out))?;
            let status = Command::new(&script)
                .current_dir(root)
                .status()
                .map_err(|e| format!("run {}: {e}", script.display()))?;
            if status.success() {
                Ok(())
            } else {
                Err(format!("{} exited with {status}", script.display()))
            }
        }
        Err(err) => {
            println!("No runnable updateScript:\n{err}");
            native_fetcher_update(root, package)
        }
    }
}

fn native_fetcher_update(root: &Path, package: &str) -> Result<(), String> {
    println!("Trying native fetcher update for {package}...");
    let file = packages::file_for_attr(root, package)
        .ok_or_else(|| format!("could not find package file for {package}"))?;
    let mut text =
        fs::read_to_string(&file).map_err(|e| format!("read {}: {e}", file.display()))?;
    let old_version =
        extract_assignment(&text, "version").ok_or("could not find version assignment")?;

    let source = if text.contains("fetchFromGitHub") {
        let owner =
            extract_assignment(&text, "owner").ok_or("could not find fetchFromGitHub owner")?;
        let repo =
            extract_assignment(&text, "repo").ok_or("could not find fetchFromGitHub repo")?;
        let rev_template = extract_assignment(&text, "rev")
            .or_else(|| extract_assignment(&text, "tag"))
            .unwrap_or_else(|| "${version}".to_string());
        let prefix = version_prefix(&rev_template);
        NativeSource::GitHubArchive {
            owner,
            repo,
            prefix,
        }
    } else if text.contains("fetchurl") || text.contains("fetchzip") {
        let url = extract_assignment(&text, "url").ok_or("could not find fetchurl/fetchzip url")?;
        let (owner, repo, prefix) = github_release_from_url(&url).ok_or("native fetchurl support currently requires a GitHub releases/download URL containing ${version} or ${finalAttrs.version}")?;
        NativeSource::GitHubReleaseAsset {
            owner,
            repo,
            prefix,
            url_template: url,
            unpack: text.contains("fetchzip"),
        }
    } else {
        return Err("native updater currently supports fetchFromGitHub and GitHub release fetchurl/fetchzip only".to_string());
    };

    let latest = latest_github_tag(source.owner(), source.repo(), source.prefix(), &old_version)?;
    if latest == old_version {
        println!("Already at latest detected version {latest}.");
        return Ok(());
    }
    let src_hash = source.prefetch(&latest)?;

    let old_version_assignment = format!("version = \"{old_version}\";");
    if !text.contains(&old_version_assignment) {
        return Err("could not replace version assignment safely".to_string());
    }
    text = text.replacen(
        &old_version_assignment,
        &format!("version = \"{latest}\";"),
        1,
    );
    text = replace_src_hash(&text, &src_hash)?;
    let dep_hashes = dependency_hash_keys(&text);
    if !dep_hashes.is_empty() {
        text = replace_dep_hashes_with_fake(&text, &dep_hashes);
    }
    fs::write(&file, text).map_err(|e| format!("write {}: {e}", file.display()))?;

    if !dep_hashes.is_empty() {
        let wanted = build_and_extract_wanted_hashes(root, package)?;
        let mut text =
            fs::read_to_string(&file).map_err(|e| format!("read {}: {e}", file.display()))?;
        for hash in wanted {
            text = text.replacen(
                "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                &hash,
                1,
            );
        }
        if text.contains("sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=") {
            return Err("could not refresh every dependency hash".to_string());
        }
        fs::write(&file, text).map_err(|e| format!("write {}: {e}", file.display()))?;
    }

    Ok(())
}

fn find_executable_in_output(out: &Path) -> Result<PathBuf, String> {
    let bin = out.join("bin");
    if bin.is_dir() {
        for entry in fs::read_dir(&bin).map_err(|e| format!("read {}: {e}", bin.display()))? {
            let path = entry.map_err(|e| format!("read bin entry: {e}"))?.path();
            if path.is_file() {
                return Ok(path);
            }
        }
    }
    if out.is_file() {
        return Ok(out.to_path_buf());
    }
    Err(format!("could not find executable in {}", out.display()))
}

enum NativeSource {
    GitHubArchive {
        owner: String,
        repo: String,
        prefix: String,
    },
    GitHubReleaseAsset {
        owner: String,
        repo: String,
        prefix: String,
        url_template: String,
        unpack: bool,
    },
}

impl NativeSource {
    fn owner(&self) -> &str {
        match self {
            Self::GitHubArchive { owner, .. } | Self::GitHubReleaseAsset { owner, .. } => owner,
        }
    }

    fn repo(&self) -> &str {
        match self {
            Self::GitHubArchive { repo, .. } | Self::GitHubReleaseAsset { repo, .. } => repo,
        }
    }

    fn prefix(&self) -> &str {
        match self {
            Self::GitHubArchive { prefix, .. } | Self::GitHubReleaseAsset { prefix, .. } => prefix,
        }
    }

    fn prefetch(&self, version: &str) -> Result<String, String> {
        match self {
            Self::GitHubArchive {
                owner,
                repo,
                prefix,
            } => prefetch_url(
                &format!("https://github.com/{owner}/{repo}/archive/{prefix}{version}.tar.gz"),
                true,
            ),
            Self::GitHubReleaseAsset {
                url_template,
                unpack,
                ..
            } => prefetch_url(&render_version_template(url_template, version), *unpack),
        }
    }
}

fn extract_assignment(text: &str, name: &str) -> Option<String> {
    let needle = format!("{name} = \"");
    let start = text.find(&needle)? + needle.len();
    let end = text[start..].find('"')?;
    Some(text[start..start + end].to_string())
}

fn latest_github_tag(
    owner: &str,
    repo: &str,
    prefix: &str,
    current: &str,
) -> Result<String, String> {
    let url = format!("https://github.com/{owner}/{repo}.git");
    let out = Command::new("git")
        .args(["ls-remote", "--tags", "--refs", &url])
        .output()
        .map_err(|e| format!("list GitHub tags: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    let mut best = current.to_string();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some(tag) = line.rsplit('/').next() else {
            continue;
        };
        let Some(version) = tag.strip_prefix(prefix) else {
            continue;
        };
        if parse_version(version).is_some() && version_gt(version, &best) {
            best = version.to_string();
        }
    }
    Ok(best)
}

fn prefetch_url(url: &str, unpack: bool) -> Result<String, String> {
    let mut cmd = Command::new("nix");
    cmd.args(["store", "prefetch-file", "--json"]);
    if unpack {
        cmd.arg("--unpack");
    }
    let out = cmd
        .arg(url)
        .output()
        .map_err(|e| format!("prefetch {url}: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    extract_json_hash(&stdout).ok_or_else(|| format!("could not parse prefetch hash from {stdout}"))
}

fn version_prefix(template: &str) -> String {
    if let Some((prefix, _)) = template.split_once("${version}") {
        return prefix.to_string();
    }
    if let Some((prefix, _)) = template.split_once("${finalAttrs.version}") {
        return prefix.to_string();
    }
    template.to_string()
}

fn render_version_template(template: &str, version: &str) -> String {
    template
        .replace("${version}", version)
        .replace("${finalAttrs.version}", version)
}

fn github_release_from_url(url: &str) -> Option<(String, String, String)> {
    let marker = "github.com/";
    let start = url.find(marker)? + marker.len();
    let rest = &url[start..];
    let mut parts = rest.split('/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    if parts.next()? != "releases" || parts.next()? != "download" {
        return None;
    }
    let tag_template = parts.next()?;
    Some((owner, repo, version_prefix(tag_template)))
}

fn extract_json_hash(text: &str) -> Option<String> {
    let needle = "\"hash\":\"";
    let start = text.find(needle)? + needle.len();
    let end = text[start..].find('"')?;
    Some(text[start..start + end].to_string())
}

fn replace_src_hash(text: &str, new_hash: &str) -> Result<String, String> {
    let src_pos = text.find("src = ").ok_or("missing src assignment")?;
    let hash_rel = text[src_pos..]
        .find("hash = \"")
        .ok_or("missing src hash")?;
    let start = src_pos + hash_rel + "hash = \"".len();
    let end = start + text[start..].find('"').ok_or("unterminated src hash")?;
    let mut out = String::with_capacity(text.len() + new_hash.len());
    out.push_str(&text[..start]);
    out.push_str(new_hash);
    out.push_str(&text[end..]);
    Ok(out)
}

fn dependency_hash_keys(text: &str) -> Vec<&'static str> {
    ["cargoHash", "vendorHash", "npmDepsHash"]
        .into_iter()
        .filter(|key| text.contains(&format!("{key} = \"")))
        .collect()
}

fn replace_dep_hashes_with_fake(text: &str, keys: &[&str]) -> String {
    let mut out = text.to_string();
    for key in keys {
        if let Some(pos) = out.find(&format!("{key} = \"")) {
            let start = pos + format!("{key} = \"").len();
            if let Some(end_rel) = out[start..].find('"') {
                out.replace_range(
                    start..start + end_rel,
                    "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                );
            }
        }
    }
    out
}

fn build_and_extract_wanted_hashes(root: &Path, package: &str) -> Result<Vec<String>, String> {
    let out = Command::new("nix")
        .args([
            "build",
            "--no-link",
            &format!("path:{}#{package}", root.display()),
        ])
        .current_dir(root)
        .output()
        .map_err(|e| format!("build for dependency hash: {e}"))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    let hashes = extract_wanted_hashes(&stderr);
    if hashes.is_empty() && !out.status.success() {
        return Err(format!(
            "could not extract wanted dependency hash from build output\n{stderr}"
        ));
    }
    Ok(hashes)
}

fn extract_wanted_hashes(text: &str) -> Vec<String> {
    let mut hashes = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("got:") {
            let hash = rest.trim();
            if hash.starts_with("sha256-") {
                hashes.push(hash.to_string());
            }
        }
    }
    hashes
}

fn parse_version(version: &str) -> Option<Vec<u64>> {
    version.split('.').map(|p| p.parse().ok()).collect()
}

fn version_gt(a: &str, b: &str) -> bool {
    parse_version(a) > parse_version(b)
}

fn pr_text(package: &str, old_version: &str, new_version: &str) -> (String, String) {
    let title =
        if old_version != "unknown" && new_version != "unknown" && old_version != new_version {
            format!("{package}: {old_version} -> {new_version}")
        } else {
            format!("{package}: update")
        };
    let body = format!(
        "Updates `{package}` from `{old_version}` to `{new_version}`.\n\nGenerated by bumpkin."
    );
    (title, body)
}

struct TempWorktree {
    root: PathBuf,
    original: PathBuf,
}

impl TempWorktree {
    fn create(root: &Path) -> Result<Self, String> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| format!("system time: {e}"))?
            .as_nanos();
        let temp = env::temp_dir().join(format!("bumpkin-{}-{stamp}", std::process::id()));
        git::run(
            root,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                temp.to_str().ok_or("non-utf8 temp path")?,
                "HEAD",
            ],
        )?;
        copy_tracked_working_changes(root, &temp)?;
        Ok(Self {
            root: temp,
            original: root.to_path_buf(),
        })
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

fn copy_tracked_working_changes(from: &Path, to: &Path) -> Result<(), String> {
    let diff = Command::new("git")
        .args(["diff", "--binary", "HEAD"])
        .current_dir(from)
        .output()
        .map_err(|e| format!("read working tree diff: {e}"))?;
    if !diff.status.success() {
        return Err(String::from_utf8_lossy(&diff.stderr).into_owned());
    }
    if diff.stdout.is_empty() {
        return Ok(());
    }
    let mut apply = Command::new("git")
        .args(["apply", "--index"])
        .current_dir(to)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("start git apply: {e}"))?;
    {
        use std::io::Write;
        apply
            .stdin
            .as_mut()
            .ok_or("git apply stdin unavailable")?
            .write_all(&diff.stdout)
            .map_err(|e| format!("write git apply diff: {e}"))?;
    }
    let status = apply
        .wait()
        .map_err(|e| format!("wait for git apply: {e}"))?;
    if !status.success() {
        return Err(format!("git apply exited with {status}"));
    }
    git::run(
        to,
        &[
            "-c",
            "user.name=bumpkin",
            "-c",
            "user.email=bumpkin@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "bumpkin dry-run baseline",
        ],
    )
}

impl Drop for TempWorktree {
    fn drop(&mut self) {
        let _ = git::run(
            &self.original,
            &[
                "worktree",
                "remove",
                "--force",
                self.root.to_str().unwrap_or(""),
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_templates_render_supported_placeholders() {
        assert_eq!(version_prefix("v${version}"), "v");
        assert_eq!(version_prefix("release-${finalAttrs.version}"), "release-");
        assert_eq!(
            render_version_template("v${version}.tar.gz", "1.2.3"),
            "v1.2.3.tar.gz"
        );
    }

    #[test]
    fn github_release_url_extracts_owner_repo_and_prefix() {
        assert_eq!(
            github_release_from_url(
                "https://github.com/owner/repo/releases/download/v${version}/asset.tar.gz"
            ),
            Some(("owner".to_string(), "repo".to_string(), "v".to_string()))
        );
    }

    #[test]
    fn wanted_hashes_are_extracted_from_nix_output() {
        let hashes = extract_wanted_hashes(
            "\nerror: hash mismatch\n  specified: sha256-xxx\n       got:    sha256-abc123=\n",
        );
        assert_eq!(hashes, vec!["sha256-abc123=".to_string()]);
    }

    #[test]
    fn numeric_version_ordering_works() {
        assert!(version_gt("1.10.0", "1.9.9"));
        assert!(!version_gt("1.0.0", "1.0.1"));
        assert_eq!(parse_version("1.2.3"), Some(vec![1, 2, 3]));
        assert_eq!(parse_version("1.2-beta"), None);
    }
}
