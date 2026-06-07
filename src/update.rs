use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{forge, git, nix, packages, repology};

pub struct CommitOptions {
    pub commit: bool,
    pub signed: bool,
    pub push: bool,
    pub pr: bool,
    pub signing_key: Option<String>,
    pub gpg_format: Option<String>,
    pub forge: String,
    pub forge_api_url: Option<String>,
    pub no_build: Vec<String>,
}

/// Per-machine blocklist from the BUMPKIN_SKIP env var (comma-separated).
pub fn env_skip() -> Vec<String> {
    env::var("BUMPKIN_SKIP")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn dry_run_package(root: &Path, package: &str) -> Result<(), String> {
    tracing::debug!("==> {package}");
    let worktree = TempWorktree::create(root)?;
    run_update_script(worktree.path(), package)?;

    if git::clean(worktree.path())? {
        tracing::info!("{package}: no changes");
        return Ok(());
    }

    tracing::debug!("--- diffstat ---");
    git::print_diff_stat(worktree.path())?;
    tracing::debug!("--- diff ---");
    git::print_diff(worktree.path())?;
    tracing::debug!("--- build ---");
    if nix::build_package(worktree.path(), package)? {
        tracing::info!("{package}: build ok");
    } else {
        tracing::error!("{package}: build failed");
    }
    Ok(())
}

fn ensure_on_branch(branch: &str) -> Result<(), String> {
    if branch.is_empty() {
        return Err(
            "HEAD is detached (no current branch); commit mode needs a branch to return to — \
             check out a branch first"
                .to_string(),
        );
    }
    Ok(())
}

pub fn update_maintainer(
    root: &Path,
    maintainer: &str,
    commit: &CommitOptions,
    skip: &[String],
) -> Result<(), String> {
    let main_branch = git::current_branch(root)?;

    if commit.commit {
        ensure_on_branch(&main_branch)?;
        if !git::clean(root)? {
            return Err(
                "working tree has uncommitted or untracked changes; commit/stash them before update"
                    .to_string(),
            );
        }
    }

    let candidates = packages::by_maintainer(root, maintainer)?;
    let env_skip = env_skip();
    let candidates: Vec<_> = candidates
        .into_iter()
        .filter(|c| !skip.contains(&c.attr_path))
        .filter(|c| !env_skip.contains(&c.attr_path))
        .collect();
    if candidates.is_empty() {
        tracing::info!("No packages found for maintainer `{maintainer}`.");
        return Ok(());
    }

    // Ensure we're on the main branch and up to date at the start.
    if commit.commit {
        tracing::debug!("=== syncing main branch ===");
        git::checkout_branch(root, &main_branch)?;
        git::pull_ff_only(root)?;
    }

    let mut summary = BatchSummary::default();
    // Track which source files were already updated this run to avoid duplicate
    // PRs for packages that share the same .nix file (e.g. waterfox + waterfox-unwrapped).
    let mut updated_files: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for candidate in candidates {
        tracing::debug!("==> {} ({})", candidate.attr_path, candidate.backend.name());
        if !(candidate.backend.is_runnable() || candidate.backend.is_native_candidate()) {
            tracing::info!(
                "{}: skipped ({})",
                candidate.attr_path,
                candidate.backend.note()
            );
            summary.skipped += 1;
            continue;
        }

        if let Some(file) = candidate.file.as_deref()
            && updated_files.contains(file)
        {
            tracing::debug!("skipped: source file already updated by a prior package in this run");
            tracing::info!("{}: skipped (already updated)", candidate.attr_path);
            summary.skipped += 1;
            continue;
        }

        let outcome = if commit.commit {
            // Per-package branch flow: branch -> update -> build -> commit -> push -> cleanup.
            commit_update_one(root, &main_branch, &candidate.attr_path, commit)
        } else {
            // Temp worktree flow: no mutation of the checkout.
            batch_update_one(root, &candidate.attr_path)
        };
        match outcome {
            Ok(BatchOutcome::NoChanges) => {
                tracing::info!("{}: no changes", candidate.attr_path);
                summary.no_changes += 1;
            }
            Ok(BatchOutcome::UpdatedBuildOk) => {
                if let Some(file) = candidate.file.as_deref() {
                    updated_files.insert(file.to_path_buf());
                }
                tracing::info!("{}: updated, build ok", candidate.attr_path);
                summary.updated += 1;
            }
            Ok(BatchOutcome::UpdatedBuildFailed) => {
                if let Some(file) = candidate.file.as_deref() {
                    updated_files.insert(file.to_path_buf());
                }
                tracing::warn!("{}: updated, build failed", candidate.attr_path);
                summary.build_failed += 1;
            }
            Err(err) => {
                tracing::error!("{}: failed: {err}", candidate.attr_path);
                summary.failed += 1;
            }
        }
    }

    // Return to the main branch.
    if commit.commit {
        let _ = git::checkout_branch(root, &main_branch);
    }

    tracing::info!("--- summary ---");
    tracing::info!("updated/build ok: {}", summary.updated);
    tracing::info!("updated/build failed: {}", summary.build_failed);
    tracing::info!("no changes: {}", summary.no_changes);
    tracing::info!("failed: {}", summary.failed);
    tracing::info!("skipped: {}", summary.skipped);
    Ok(())
}

/// Commit-mode: branch off main, update in-place, build, commit, optionally push.
fn commit_update_one(
    root: &Path,
    main_branch: &str,
    package: &str,
    commit: &CommitOptions,
) -> Result<BatchOutcome, String> {
    // Ensure a clean starting point on main.
    git::checkout_branch(root, main_branch)?;
    if !git::clean(root)? {
        git::reset_hard(root, &format!("origin/{main_branch}"))?;
    }

    let branch_name = format!("bumpkin/{package}");
    // Remove any leftover branch from a previous run.
    let _ = git::delete_branch(root, &branch_name);
    git::create_branch(root, &branch_name)?;

    let old_version = nix::package_version(root, package).unwrap_or_else(|_| "unknown".to_string());

    run_update_script(root, package)?;

    if git::clean(root)? {
        git::checkout_branch(root, main_branch)?;
        let _ = git::delete_branch(root, &branch_name);
        return Ok(BatchOutcome::NoChanges);
    }

    let changed_paths = git::changed_paths(root)?;
    let new_version = nix::package_version(root, package).unwrap_or_else(|_| "unknown".to_string());
    let title = pr_title(package, &old_version, &new_version);

    tracing::debug!("diffstat:");
    git::print_diff_stat(root)?;

    let build_skipped = commit.no_build.iter().any(|s| s == package);

    let (build_ok, build_log) = if build_skipped {
        tracing::info!("{}: build skipped (no-build)", package);
        (
            true,
            "Build intentionally skipped (in no-build list).\n".to_string(),
        )
    } else {
        nix::build_package_with_log(root, package)?
    };

    if let Some(format) = commit.gpg_format.as_deref() {
        git::run(root, &["config", "gpg.format", format])?;
    }
    if let Some(key) = commit.signing_key.as_deref() {
        git::run(root, &["config", "user.signingkey", key])?;
    }
    let (nixpkgs_input, nixpkgs_rev) = nix::flake_input_info(root).unwrap_or_default();
    let nixpkgs_in = if nixpkgs_input.is_empty() {
        None
    } else {
        Some(nixpkgs_input.as_str())
    };
    let nixpkgs_rv = if nixpkgs_rev.is_empty() {
        None
    } else {
        Some(nixpkgs_rev.as_str())
    };
    let body = pr_body(
        package,
        &old_version,
        &new_version,
        build_ok,
        build_skipped,
        Some(&build_log),
        nixpkgs_in,
        nixpkgs_rv,
    );
    git::commit_paths(root, &changed_paths, &title, &body, commit.signed)?;

    if commit.push {
        match git::push_branch(root, &branch_name) {
            Ok(()) => {
                tracing::debug!("pushed branch {branch_name}");
                if commit.pr {
                    let token = std::env::var("GITHUB_TOKEN").ok();
                    let remote_url = git::remote_url(root)?;
                    let forge_backend = forge::resolve(
                        &commit.forge,
                        commit.forge_api_url.as_deref(),
                        token.as_deref(),
                        &remote_url,
                    );
                    match forge_backend {
                        Ok(backend) => {
                            let (owner, repo) = owner_repo_from_url(&remote_url)?;
                            match backend.find_existing_pr(
                                root,
                                &branch_name,
                                main_branch,
                                &owner,
                                &repo,
                            ) {
                                Ok(Some(pr_num)) => {
                                    tracing::info!("{}: PR #{pr_num} already exists", package);
                                }
                                Ok(None) => {
                                    tracing::info!("{}: opening PR...", package);
                                    match backend.create_pr(
                                        root,
                                        &branch_name,
                                        main_branch,
                                        &title,
                                        &body,
                                        &owner,
                                        &repo,
                                    ) {
                                        Ok(url) => tracing::info!("{}: PR created: {url}", package),
                                        Err(err) => tracing::warn!("PR creation failed: {err}"),
                                    }
                                }
                                Err(err) => tracing::warn!("find existing PR failed: {err}"),
                            }
                        }
                        Err(err) => tracing::warn!("forge backend not available: {err}"),
                    }
                }
            }
            Err(err) => {
                tracing::warn!("push failed: {err}");
            }
        }
    }

    let outcome = if build_ok {
        BatchOutcome::UpdatedBuildOk
    } else {
        BatchOutcome::UpdatedBuildFailed
    };

    // Return to main and delete the per-package branch.
    // Force-checkout discards any uncommitted changes (from failed builds, etc.).
    git::checkout_branch(root, main_branch)?;
    let _ = git::delete_branch(root, &branch_name);
    Ok(outcome)
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

    if commit.push || commit.pr {
        // Per-package branch flow, same as batch mode: branch -> update ->
        // build -> commit -> push -> PR -> return to the current branch.
        let main_branch = git::current_branch(root)?;
        ensure_on_branch(&main_branch)?;
        match commit_update_one(root, &main_branch, package, &commit)? {
            BatchOutcome::NoChanges => tracing::info!("{package}: no changes"),
            BatchOutcome::UpdatedBuildOk => tracing::info!("{package}: updated, build ok"),
            BatchOutcome::UpdatedBuildFailed => {
                tracing::warn!("{package}: updated, build failed")
            }
        }
        return Ok(());
    }

    let old_version = nix::package_version(root, package).unwrap_or_else(|_| "unknown".to_string());
    run_update_script(root, package)?;

    let changed_paths = git::changed_paths(root)?;
    if changed_paths.is_empty() {
        tracing::info!("No diff after running update script.");
        return Ok(());
    }

    let new_version = nix::package_version(root, package).unwrap_or_else(|_| "unknown".to_string());
    let title = pr_title(package, &old_version, &new_version);

    tracing::info!("--- suggested PR title ---\n{title}");
    tracing::info!("--- diffstat ---");
    git::print_diff_stat(root)?;

    let build_skipped = commit.no_build.iter().any(|s| s == package);
    if build_skipped {
        tracing::info!("--- build skipped (no-build) ---");
    } else {
        tracing::info!("--- build ---");
        if !nix::build_package(root, package)? {
            return Err("build failed; leaving changes in working tree".to_string());
        }
    }
    let (nixpkgs_input, nixpkgs_rev) = nix::flake_input_info(root).unwrap_or_default();
    let nixpkgs_in = if nixpkgs_input.is_empty() {
        None
    } else {
        Some(nixpkgs_input.as_str())
    };
    let nixpkgs_rv = if nixpkgs_rev.is_empty() {
        None
    } else {
        Some(nixpkgs_rev.as_str())
    };
    let body = pr_body(
        package,
        &old_version,
        &new_version,
        true,
        build_skipped,
        None,
        nixpkgs_in,
        nixpkgs_rv,
    );
    tracing::info!("--- suggested PR body ---\n{body}");

    if commit.commit {
        if let Some(format) = commit.gpg_format.as_deref() {
            git::run(root, &["config", "gpg.format", format])?;
        }
        if let Some(key) = commit.signing_key.as_deref() {
            git::run(root, &["config", "user.signingkey", key])?;
        }
        git::commit_paths(root, &changed_paths, &title, &body, commit.signed)?;
        tracing::info!(
            "Committed{}: {title}",
            if commit.signed { " signed" } else { "" }
        );
    } else {
        tracing::info!(
            "No commit created. Re-run with --commit to commit, or --commit --signed for a signed commit."
        );
    }

    Ok(())
}

/// Run a package's updateScript directly (no CLI arg parsing needed — clap handles it).
pub fn run_update_script_direct(root: &Path, package: &str) -> Result<(), String> {
    run_update_script(root, package)
}

fn run_update_script(root: &Path, package: &str) -> Result<(), String> {
    tracing::debug!("{package}: trying updateScript...");
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
            tracing::debug!("{package}: no runnable updateScript:\n{err}");
            native_fetcher_update(root, package)
        }
    }
}

/// Native fetcher update, driven by Nix instead of per-forge implementations:
///
/// 1. `nix eval` the package's `src` to find where the source actually lives
///    (`gitRepoUrl` / `urls`) — no .nix text scraping for the source location.
/// 2. `git ls-remote --tags` against that URL works for any git host
///    (GitHub, GitLab, sourcehut, Codeberg, Gitea, ...).
/// 3. Rewrite the version, set the src + dependency hashes to the fake hash,
///    and let `nix build` report the real ones — Nix runs the fetcher itself,
///    so this supports every fetcher without bot-side prefetch logic.
fn native_fetcher_update(root: &Path, package: &str) -> Result<(), String> {
    tracing::debug!("{package}: trying native fetcher...");
    let file = packages::file_for_attr(root, package)
        .ok_or_else(|| format!("could not find package file for {package}"))?;
    let mut text =
        fs::read_to_string(&file).map_err(|e| format!("read {}: {e}", file.display()))?;
    let old_version =
        extract_assignment(&text, "version").ok_or("could not find version assignment")?;

    // The src must be derived from the version (rev/tag/url referencing
    // ${version}); otherwise bumping the version would not change the source.
    let Some(prefix) = version_linked_prefix(&text) else {
        return Err(repology_hint(
            &text,
            &old_version,
            "src is not version-linked (no rev/tag/url containing ${version})",
        ));
    };

    let src = nix::src_info(root, package)?;
    let Some(git_url) = git_url_from_src(&src.git_repo_url, &src.urls) else {
        return Err(repology_hint(
            &text,
            &old_version,
            "could not derive a git URL from the evaluated src",
        ));
    };

    tracing::debug!("{package}: listing tags on {git_url}");
    let tags = ls_remote_tags(&git_url)?;
    let latest = best_version(&tags, &prefix, &old_version);
    if latest == old_version {
        tracing::debug!("{package}: already at latest version {latest}");
        return Ok(());
    }
    tracing::debug!("{package}: {old_version} -> {latest}");

    let old_version_assignment = format!("version = \"{old_version}\";");
    if !text.contains(&old_version_assignment) {
        return Err("could not replace version assignment safely".to_string());
    }
    text = text.replacen(
        &old_version_assignment,
        &format!("version = \"{latest}\";"),
        1,
    );

    // Fake out the src hash and all known dependency hashes, then let Nix
    // compute the real ones.
    text = replace_src_hash(&text, FAKE_HASH)?;
    let dep_hashes = dependency_hash_keys(&text);
    text = replace_dep_hashes_with_fake(&text, &dep_hashes);
    fs::write(&file, text).map_err(|e| format!("write {}: {e}", file.display()))?;

    refresh_fake_hashes(root, package, &file)
}

/// The well-known fake hash accepted by all Nix fetchers.
const FAKE_HASH: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

/// Repeatedly build the package, replacing fake hashes with the hashes Nix
/// reports it wanted. Nix typically reports one fixed-output mismatch per
/// build (later fetches depend on earlier ones), so loop until none remain.
fn refresh_fake_hashes(root: &Path, package: &str, file: &Path) -> Result<(), String> {
    loop {
        let text = fs::read_to_string(file).map_err(|e| format!("read {}: {e}", file.display()))?;
        let remaining = text.matches(FAKE_HASH).count();
        if remaining == 0 {
            return Ok(());
        }
        tracing::debug!("{package}: building to compute {remaining} remaining hash(es)...");
        let wanted = build_and_extract_wanted_hashes(root, package)?;
        if wanted.is_empty() {
            return Err(
                "build did not report a hash mismatch for the remaining fake hash(es)".to_string(),
            );
        }
        let mut text = text;
        for hash in &wanted {
            text = text.replacen(FAKE_HASH, hash, 1);
        }
        fs::write(file, &text).map_err(|e| format!("write {}: {e}", file.display()))?;
        if text.matches(FAKE_HASH).count() >= remaining {
            return Err("build output did not resolve any fake hash".to_string());
        }
    }
}

/// Consult repology for a version hint before giving up, so the error tells
/// the user whether an update is actually pending.
fn repology_hint(text: &str, old_version: &str, reason: &str) -> String {
    if let Some(pname) = extract_assignment(text, "pname")
        && let Some(repology_ver) = repology::latest_version(&pname)
        && repology_ver != old_version
    {
        return format!(
            "{reason}; Repology knows {pname} = {repology_ver} (nix has {old_version}) — add an updateScript"
        );
    }
    format!("{reason}; add an updateScript")
}

/// Find the tag prefix from the first rev/tag/url assignment that references
/// the version, e.g. `rev = "v${version}"` -> "v",
/// `url = ".../releases/download/app-${version}/x.tar.gz"` -> "app-".
fn version_linked_prefix(text: &str) -> Option<String> {
    for key in ["rev", "tag", "url"] {
        let Some(template) = extract_assignment(text, key) else {
            continue;
        };
        for placeholder in ["${version}", "${finalAttrs.version}"] {
            if let Some((before, _)) = template.split_once(placeholder) {
                let prefix = before.rsplit(['/', '=']).next().unwrap_or(before);
                return Some(prefix.to_string());
            }
        }
    }
    None
}

/// Pick the git URL for tag discovery from the evaluated src info.
fn git_url_from_src(git_repo_url: &str, urls: &[String]) -> Option<String> {
    if !git_repo_url.is_empty() {
        return Some(git_repo_url.to_string());
    }
    urls.iter().find_map(|url| git_url_from_archive_url(url))
}

/// Map a resolved source/archive URL to the git repository URL it came from.
fn git_url_from_archive_url(url: &str) -> Option<String> {
    let url = url.strip_prefix("git+").unwrap_or(url);
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;

    // GitLab API archive: https://host/api/v4/projects/owner%2Frepo/repository/archive...
    if let Some(p) = path.strip_prefix("api/v4/projects/") {
        let project = p.split('/').next()?;
        let project = project.replace("%2F", "/").replace("%2f", "/");
        return Some(format!("https://{host}/{project}.git"));
    }

    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() < 2 || segments[0].is_empty() || segments[1].is_empty() {
        return None;
    }
    let owner = segments[0];
    let repo = segments[1].trim_end_matches(".git");

    // sourcehut keeps the ~ in the owner and dislikes a .git suffix.
    if host == "git.sr.ht" {
        return Some(format!("https://git.sr.ht/{owner}/{repo}"));
    }
    // A plain repo URL (e.g. from fetchgit) — but not a two-segment tarball URL.
    if segments.len() == 2 {
        let is_archive = [".tar.gz", ".tgz", ".tar.xz", ".tar.bz2", ".tar.zst", ".zip"]
            .iter()
            .any(|ext| segments[1].ends_with(ext));
        if is_archive {
            return None;
        }
        return Some(format!("https://{host}/{owner}/{repo}.git"));
    }
    // Forge archive/release layouts:
    //   GitHub:         /owner/repo/archive/..., /owner/repo/releases/download/...
    //   GitLab web:     /owner/repo/-/archive/...
    //   Gitea/Codeberg: /owner/repo/archive/...
    //   Bitbucket:      /owner/repo/get/...
    match segments[2] {
        "archive" | "releases" | "get" | "-" => Some(format!("https://{host}/{owner}/{repo}.git")),
        _ => None,
    }
}

/// List tag names on any git remote. Works for every git host, which is why
/// there are no per-forge tag API clients here.
fn ls_remote_tags(url: &str) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .args(["ls-remote", "--tags", "--refs", url])
        .output()
        .map_err(|e| format!("git ls-remote {url}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git ls-remote {url}: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.split('\t').nth(1))
        .filter_map(|r| r.strip_prefix("refs/tags/"))
        .map(str::to_string)
        .collect())
}

/// Highest numeric version among tags matching the prefix; falls back to
/// `current` when nothing newer is found. Non-numeric tags (rc, beta, ...)
/// are skipped.
fn best_version(tags: &[String], prefix: &str, current: &str) -> String {
    let mut best = current.to_string();
    for tag in tags {
        let Some(version) = tag.strip_prefix(prefix) else {
            continue;
        };
        if parse_version(version).is_some() && version_gt(version, &best) {
            best = version.to_string();
        }
    }
    best
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

/// Extract the value of a `name = "value";` assignment. Requires `name` to
/// start at an attribute boundary, so e.g. `rev` does not match `prev`.
fn extract_assignment(text: &str, name: &str) -> Option<String> {
    let needle = format!("{name} = \"");
    let mut from = 0;
    while let Some(pos) = text[from..].find(&needle) {
        let abs = from + pos;
        let at_boundary = abs == 0
            || !matches!(
                text.as_bytes()[abs - 1],
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'\''
            );
        if at_boundary {
            let start = abs + needle.len();
            let end = text[start..].find('"')?;
            return Some(text[start..start + end].to_string());
        }
        from = abs + needle.len();
    }
    None
}

/// Extract (owner, repo) from a git remote URL.
fn owner_repo_from_url(url: &str) -> Result<(String, String), String> {
    let (_, owner, repo) = forge::parse_repo_url(url)?;
    Ok((owner, repo))
}

fn replace_src_hash(text: &str, new_hash: &str) -> Result<String, String> {
    let src_pos = text.find("src = ").ok_or("missing src assignment")?;
    let after = &text[src_pos..];
    let (hash_rel, needle_len) = ["hash = \"", "sha256 = \""]
        .into_iter()
        .filter_map(|needle| after.find(needle).map(|rel| (rel, needle.len())))
        .min_by_key(|(rel, _)| *rel)
        .ok_or("missing src hash")?;
    let start = src_pos + hash_rel + needle_len;
    let end = start + text[start..].find('"').ok_or("unterminated src hash")?;
    let mut out = String::with_capacity(text.len() + new_hash.len());
    out.push_str(&text[..start]);
    out.push_str(new_hash);
    out.push_str(&text[end..]);
    Ok(out)
}

fn dependency_hash_keys(text: &str) -> Vec<&'static str> {
    [
        "cargoHash",
        "vendorHash",
        "npmDepsHash",
        "yarnHash",
        "pomHash",
        "mvnHash",
        "mixHash",
        "nugetHash",
        "dotnetHash",
    ]
    .into_iter()
    .filter(|key| text.contains(&format!("{key} = \"")))
    .collect()
}

fn replace_dep_hashes_with_fake(text: &str, keys: &[&str]) -> String {
    let mut out = text.to_string();
    for key in keys {
        let needle = format!("{key} = \"");
        if let Some(pos) = out.find(&needle) {
            let start = pos + needle.len();
            if let Some(end_rel) = out[start..].find('"') {
                out.replace_range(start..start + end_rel, FAKE_HASH);
            }
        }
    }
    out
}

fn build_and_extract_wanted_hashes(root: &Path, package: &str) -> Result<Vec<String>, String> {
    let out = crate::nix::nix_cmd()
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

fn pr_title(package: &str, old_version: &str, new_version: &str) -> String {
    format!("feat({package}): {old_version} -> {new_version}")
}

fn pr_body(
    package: &str,
    old_version: &str,
    new_version: &str,
    build_ok: bool,
    build_skipped: bool,
    build_log: Option<&str>,
    nixpkgs_input: Option<&str>,
    nixpkgs_rev: Option<&str>,
) -> String {
    let current = current_platform();
    let build_note = if build_skipped {
        format!("**Built on platform:**\n\n- [ ] `{current}` skipped ⏩ (in no-build list)\n\n")
    } else if build_ok {
        format!("**Built on platform:**\n\n- [x] `{current}`\n\n")
    } else {
        format!("**Built on platform:**\n\n- [ ] `{current}` failed ⚠️\n\n")
    };
    let mut body =
        format!("Update `{package}` from `{old_version}` to `{new_version}`.\n\n{build_note}");
    if let Some(input) = nixpkgs_input {
        if let Some(rev) = nixpkgs_rev {
            body.push_str(&format!("**Flake:** `{input}` at `{rev}`\n\n"));
        } else {
            body.push_str(&format!("**Flake:** `{input}`\n\n"));
        }
    }
    body.push_str("Generated by [bumpkin](https://github.com/74k1/bumpkin).");
    if !build_ok
        && !build_skipped
        && let Some(log) = build_log
    {
        let tail: Vec<&str> = log.lines().rev().take(20).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        body.push_str("\n\n<details>\n<summary>Build log (last 20 lines)</summary>\n\n```\n");
        body.push_str(&tail.join("\n"));
        body.push_str("\n```\n</details>\n");
    }
    body
}

fn current_platform() -> &'static str {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => "x86_64-linux",
        ("aarch64", "linux") => "aarch64-linux",
        ("x86_64", "macos") => "x86_64-darwin",
        ("aarch64", "macos") => "aarch64-darwin",
        _ => "unknown",
    }
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
    fn version_linked_prefix_from_rev_tag_and_url() {
        assert_eq!(
            version_linked_prefix(r#"rev = "v${version}";"#).as_deref(),
            Some("v")
        );
        assert_eq!(
            version_linked_prefix(r#"tag = "release-${finalAttrs.version}";"#).as_deref(),
            Some("release-")
        );
        assert_eq!(
            version_linked_prefix(
                r#"url = "https://github.com/o/r/releases/download/app-${version}/x.tar.gz";"#
            )
            .as_deref(),
            Some("app-")
        );
        assert_eq!(version_linked_prefix(r#"rev = "deadbeef";"#), None);
    }

    #[test]
    fn git_urls_derived_from_archive_urls() {
        assert_eq!(
            git_url_from_archive_url("https://github.com/o/r/archive/v1.0.tar.gz").as_deref(),
            Some("https://github.com/o/r.git")
        );
        assert_eq!(
            git_url_from_archive_url("https://github.com/o/r/releases/download/v1/x.tar.gz")
                .as_deref(),
            Some("https://github.com/o/r.git")
        );
        assert_eq!(
            git_url_from_archive_url(
                "https://gitlab.com/api/v4/projects/o%2Fr/repository/archive.tar.gz?sha=v1"
            )
            .as_deref(),
            Some("https://gitlab.com/o/r.git")
        );
        assert_eq!(
            git_url_from_archive_url("https://gitlab.com/o/r/-/archive/v1/r-v1.tar.gz").as_deref(),
            Some("https://gitlab.com/o/r.git")
        );
        assert_eq!(
            git_url_from_archive_url("https://git.sr.ht/~o/r/archive/v1.tar.gz").as_deref(),
            Some("https://git.sr.ht/~o/r")
        );
        assert_eq!(
            git_url_from_archive_url("https://codeberg.org/o/r/archive/v1.tar.gz").as_deref(),
            Some("https://codeberg.org/o/r.git")
        );
        assert_eq!(
            git_url_from_archive_url("https://gitea.example.com/o/r.git").as_deref(),
            Some("https://gitea.example.com/o/r.git")
        );
        assert_eq!(
            git_url_from_archive_url("https://example.com/dist/foo-1.0.tar.gz"),
            None
        );
    }

    #[test]
    fn best_version_compares_numerically() {
        let tags: Vec<String> = ["v9.9.9", "v10.0.0", "v10.1.0-rc1", "other"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(best_version(&tags, "v", "9.0.0"), "10.0.0");
        assert_eq!(best_version(&tags, "v", "10.0.0"), "10.0.0");
        assert_eq!(best_version(&tags, "v", "11.0.0"), "11.0.0");
    }

    #[test]
    fn ls_remote_tag_names_keep_slashes() {
        // best_version operates on full tag names, so tags with slashes must
        // survive parsing (refs/tags/releases/v1.2 -> releases/v1.2).
        let tags: Vec<String> = ["releases/1.2.0", "releases/1.10.0"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(best_version(&tags, "releases/", "1.2.0"), "1.10.0");
    }

    #[test]
    fn extract_assignment_respects_attribute_boundaries() {
        assert_eq!(
            extract_assignment(r#"prev = "x"; rev = "v${version}";"#, "rev").as_deref(),
            Some("v${version}")
        );
        assert_eq!(
            extract_assignment(r#"version = "1.2.3";"#, "version").as_deref(),
            Some("1.2.3")
        );
    }

    #[test]
    fn src_hash_replacement_supports_hash_and_sha256() {
        let hashed = replace_src_hash(
            "src = fetchurl {\n  hash = \"sha256-old\";\n};",
            "sha256-new",
        )
        .unwrap();
        assert!(hashed.contains("hash = \"sha256-new\""));
        let sha = replace_src_hash(
            "src = fetchurl {\n  sha256 = \"sha256-old\";\n};",
            "sha256-new",
        )
        .unwrap();
        assert!(sha.contains("sha256 = \"sha256-new\""));
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
