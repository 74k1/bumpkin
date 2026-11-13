use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{Args, forge, git, nix};

// Per-package outcomes; also the labels of the batch summary.
const NO_CHANGES: &str = "no changes";
const BUILD_OK: &str = "updated, build ok";
const BUILD_FAILED: &str = "updated, build failed";

/// Error prefix for packages the native updater cannot handle (as opposed to
/// an update that was attempted and failed); batch mode counts these as skipped.
const UNSUPPORTED: &str = "unsupported: ";

fn unsupported(reason: &str) -> String {
    format!("{UNSUPPORTED}{reason}; add an updateScript")
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

fn log_outcome(package: &str, outcome: &str) {
    if outcome == BUILD_FAILED {
        warn!("{package}: {outcome}");
    } else {
        info!("{package}: {outcome}");
    }
}

fn ensure_on_branch(branch: &str) -> Result<(), String> {
    if branch.is_empty() {
        return Err(
            "HEAD is detached (no current branch); commit mode needs a branch to return to - \
             check out a branch first"
                .to_string(),
        );
    }
    Ok(())
}

fn ensure_clean(root: &Path) -> Result<(), String> {
    if !git::clean(root)? {
        return Err(
            "working tree has uncommitted or untracked changes; commit/stash them before update"
                .to_string(),
        );
    }
    Ok(())
}

fn skip_build(a: &Args, package: &str) -> bool {
    a.no_build.iter().any(|s| s == "*" || s == package)
}

fn commit(root: &Path, paths: &[String], title: &str, body: &str, a: &Args) -> Result<(), String> {
    let config = [
        ("gpg.format", a.gpg_format.as_deref()),
        ("user.signingkey", a.signing_key.as_deref()),
    ];
    git::commit_paths(root, paths, title, body, a.signed, &config)
}

pub fn dry_run_package(root: &Path, package: &str) -> Result<(), String> {
    log_outcome(package, worktree_update_one(root, package)?);
    Ok(())
}

pub fn update_maintainer(root: &Path, maintainer: &str, a: &Args) -> Result<(), String> {
    let main_branch = git::current_branch(root)?;
    if a.commit {
        ensure_on_branch(&main_branch)?;
        ensure_clean(root)?;
    }

    let packages: Vec<_> = nix::packages_by_maintainer(root, maintainer)?
        .into_iter()
        .filter(|p| !a.skip.contains(&p.attr_path))
        .collect();
    if packages.is_empty() {
        info!("No packages found for maintainer `{maintainer}`.");
        return Ok(());
    }

    // Ensure we're on the main branch and up to date at the start.
    if a.commit {
        git::checkout_branch(root, &main_branch)?;
        git::pull_ff_only(root)?;
    }

    let mut counts: HashMap<&str, usize> = HashMap::new();
    // Track which source files were already updated this run to avoid duplicate
    // PRs for packages that share the same .nix file (e.g. waterfox + waterfox-unwrapped).
    let mut updated_files: HashSet<PathBuf> = HashSet::new();
    for package in packages {
        let name = &package.attr_path;
        debug!("==> {name}");
        let label = if package.file.as_ref().is_some_and(|f| updated_files.contains(f)) {
            info!("{name}: skipped (source file already updated by a prior package)");
            "skipped"
        } else {
            let result = if a.commit {
                // Per-package branch flow: branch -> update -> build -> commit -> push -> cleanup.
                commit_update_one(root, &main_branch, name, a)
            } else {
                // Temp worktree flow: no mutation of the checkout.
                worktree_update_one(root, name)
            };
            match result {
                Ok(outcome) => {
                    log_outcome(name, outcome);
                    if let Some(file) = package.file.filter(|_| outcome != NO_CHANGES) {
                        updated_files.insert(file);
                    }
                    outcome
                }
                Err(err) => match err.strip_prefix(UNSUPPORTED) {
                    Some(reason) => {
                        info!("{name}: skipped ({reason})");
                        "skipped"
                    }
                    None => {
                        error!("{name}: failed: {err}");
                        "failed"
                    }
                },
            }
        };
        *counts.entry(label).or_default() += 1;
    }

    // Return to the main branch.
    if a.commit {
        let _ = git::checkout_branch(root, &main_branch);
    }

    info!("--- summary ---");
    for label in [BUILD_OK, BUILD_FAILED, NO_CHANGES, "failed", "skipped"] {
        info!("{label}: {}", counts.get(label).copied().unwrap_or(0));
    }
    Ok(())
}

/// Commit-mode: branch off main, update in-place, build, commit, optionally push.
fn commit_update_one(
    root: &Path,
    main_branch: &str,
    package: &str,
    a: &Args,
) -> Result<&'static str, String> {
    git::checkout_branch(root, main_branch)?;
    let branch = format!("{}{package}", a.branch_prefix);
    // Remove any leftover branch from a previous run.
    let _ = git::delete_branch(root, &branch);
    git::create_branch(root, &branch)?;

    let old_version = nix::package_version(root, package);
    run_update_script(root, package)?;

    let changed_paths = git::changed_paths(root)?;
    if changed_paths.is_empty() {
        git::checkout_branch(root, main_branch)?;
        let _ = git::delete_branch(root, &branch);
        return Ok(NO_CHANGES);
    }
    let new_version = nix::package_version(root, package);
    let title = pr_title(package, &old_version, &new_version);
    if crate::verbose() {
        git::print_diff_stat(root)?;
    }

    let (build_ok, build_log) = if skip_build(a, package) {
        info!("{package}: build skipped (no-build)");
        (None, String::new())
    } else {
        let (ok, log) = nix::build_package_with_log(root, package)?;
        (Some(ok), log)
    };
    let body = pr_body(root, package, &old_version, &new_version, build_ok, &build_log);
    commit(root, &changed_paths, &title, &body, a)?;

    // Skip the force-push when the remote branch already has this exact
    // content, so an open PR is not churned on every run.
    let pushed = a.push
        && (git::matches_remote(root, &branch)
            || git::push_branch(root, &branch)
                .map_err(|err| warn!("{package}: push failed: {err}"))
                .is_ok());
    if pushed && a.pr {
        let pr = git::remote_url(root).and_then(|remote| {
            let token = env::var("GITHUB_TOKEN").ok();
            forge::resolve(&a.forge, a.forge_api_url.as_deref(), token.as_deref(), &remote)?
                .create_pr(root, &branch, main_branch, &title, &body)
        });
        match pr {
            Ok(url) => info!("{package}: PR created: {url}"),
            Err(err) if err.contains("already exists") => {
                info!("{package}: PR already exists, branch updated")
            }
            Err(err) => warn!("{package}: PR creation failed: {err}"),
        }
    }

    // Return to main. Only drop the per-package branch once its commit is
    // durably stored on the remote; on push failure (or when --push was not
    // requested) keep the branch so the commit survives for inspection/retry.
    git::checkout_branch(root, main_branch)?;
    if pushed {
        let _ = git::delete_branch(root, &branch);
    } else {
        warn!("{package}: keeping branch {branch} (commit not on remote)");
    }
    Ok(if build_ok == Some(false) { BUILD_FAILED } else { BUILD_OK })
}

/// Update and build in a temp worktree, leaving the checkout untouched.
fn worktree_update_one(root: &Path, package: &str) -> Result<&'static str, String> {
    let worktree = TempWorktree::create(root)?;
    run_update_script(worktree.path(), package)?;
    if git::clean(worktree.path())? {
        return Ok(NO_CHANGES);
    }
    // git writes the diff straight to stdout, so gate it on the log level.
    if crate::verbose() {
        git::print_diff_stat(worktree.path())?;
        git::print_diff(worktree.path())?;
    }
    let built = nix::build_package(worktree.path(), package)?;
    Ok(if built { BUILD_OK } else { BUILD_FAILED })
}

pub fn update_package(root: &Path, package: &str, a: &Args) -> Result<(), String> {
    ensure_clean(root)?;

    if a.push {
        // Per-package branch flow, same as batch mode: branch -> update ->
        // build -> commit -> push -> PR -> return to the current branch.
        let main_branch = git::current_branch(root)?;
        ensure_on_branch(&main_branch)?;
        let result = commit_update_one(root, &main_branch, package, a);
        let _ = git::checkout_branch(root, &main_branch);
        log_outcome(package, result?);
        return Ok(());
    }

    let old_version = nix::package_version(root, package);
    run_update_script(root, package)?;

    let changed_paths = git::changed_paths(root)?;
    if changed_paths.is_empty() {
        info!("No diff after running update script.");
        return Ok(());
    }

    let new_version = nix::package_version(root, package);
    let title = pr_title(package, &old_version, &new_version);
    info!("--- suggested PR title ---\n{title}");
    git::print_diff_stat(root)?;

    let build_ok = if skip_build(a, package) {
        info!("--- build skipped (no-build) ---");
        None
    } else if nix::build_package(root, package)? {
        Some(true)
    } else {
        return Err("build failed; leaving changes in working tree".to_string());
    };
    let body = pr_body(root, package, &old_version, &new_version, build_ok, "");
    info!("--- suggested PR body ---\n{body}");

    if a.commit {
        commit(root, &changed_paths, &title, &body, a)?;
        info!("Committed{}: {title}", if a.signed { " signed" } else { "" });
    } else {
        info!(
            "No commit created. Re-run with --commit to commit, or --commit --signed for a signed commit."
        );
    }
    Ok(())
}

fn dir_is_writable(dir: &Path) -> bool {
    // Mode-bit checks lie for group-writable dirs like /nix/store (1775).
    // Probe with an actual create, which respects the effective uid.
    let probe = dir.join(".bumpkin-write-probe");
    let created = fs::OpenOptions::new().write(true).create_new(true).open(&probe);
    created.is_ok() && fs::remove_file(&probe).is_ok()
}

/// Pure-flake evaluation flattens bare-path updateScripts (e.g. `./update.sh`)
/// into a single read-only store file named after the source tree, so scripts
/// that write next to themselves fail. If the checkout's package directory
/// contains a file with identical content, return that copy - writable
/// directory, same script. This mirrors how nixpkgs' update.py runs scripts
/// from the checkout rather than the store.
fn writable_script_copy(package_file: Option<&Path>, store_script: &Path) -> Option<PathBuf> {
    if !store_script.is_file() || dir_is_writable(store_script.parent()?) {
        return None;
    }
    let store_bytes = fs::read(store_script).ok()?;
    let entries = fs::read_dir(package_file?.parent()?).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().is_some_and(|ext| ext == "nix") {
            continue;
        }
        if fs::read(&path).is_ok_and(|bytes| bytes == store_bytes) {
            return Some(path);
        }
    }
    None
}

/// Run the package's updateScript, or the native updater when it has none.
pub fn run_update_script(root: &Path, package: &str) -> Result<(), String> {
    let info = nix::package_info(root, package)?;
    debug!("{package}: trying updateScript...");
    match nix::build_update_script(root, package) {
        Ok(script_out) => {
            let script = find_executable_in_output(Path::new(&script_out))?;
            let script = writable_script_copy(info.file.as_deref(), &script).unwrap_or(script);
            // Same environment as nixpkgs' `maintainers/scripts/update.py`
            // (nix-update reads the attr path from it).
            let env = [
                ("UPDATE_NIX_ATTR_PATH", package),
                ("UPDATE_NIX_NAME", &info.name),
                ("UPDATE_NIX_PNAME", &info.pname),
                ("UPDATE_NIX_OLD_VERSION", &info.version),
            ];
            let status = Command::new(&script)
                .current_dir(root)
                .envs(env.into_iter().filter(|(_, value)| !value.is_empty()))
                .status()
                .map_err(|e| format!("run {}: {e}", script.display()))?;
            // No native fallback here: a broken updateScript should surface,
            // not be papered over by a different update path.
            if status.success() {
                Ok(())
            } else {
                Err(format!(
                    "updateScript {} exited with {status}; native updater not attempted",
                    script.display()
                ))
            }
        }
        Err(err) => {
            debug!("{package}: no runnable updateScript:\n{err}");
            native_fetcher_update(root, package, &info)
        }
    }
}

/// Native fetcher update, driven by Nix instead of per-forge implementations:
///
/// 1. `nix eval` the package's `src` to find where the source actually lives
///    (`gitRepoUrl` / `urls`) - no .nix text scraping for the source location.
/// 2. `git ls-remote --tags` against that URL works for any git host
///    (GitHub, GitLab, sourcehut, Codeberg, Gitea, ...).
/// 3. Rewrite the version, set the src + dependency hashes to the fake hash,
///    and let `nix build` report the real ones - Nix runs the fetcher itself,
///    so this supports every fetcher without bot-side prefetch logic.
fn native_fetcher_update(root: &Path, package: &str, info: &nix::PackageInfo) -> Result<(), String> {
    debug!("{package}: trying native fetcher...");
    let Some(file) = info.file.as_deref() else {
        return Err(unsupported("no package file (meta.position) inside the flake"));
    };
    let mut text = fs::read_to_string(file).map_err(|e| format!("read {}: {e}", file.display()))?;
    let Some(old_version) = extract_assignment(&text, "version") else {
        return Err(unsupported("could not find a version assignment"));
    };

    // The src must be derived from the version (rev/tag/url referencing
    // ${version}); otherwise bumping the version would not change the source.
    // Look in the src fetcher first so an unrelated url (patches, etc.) can't
    // win; fall back to the whole file for src attrs bound elsewhere.
    let Some(prefix) = version_linked_prefix(src_scope(&text), &old_version)
        .or_else(|| version_linked_prefix(&text, &old_version))
    else {
        return Err(unsupported(
            "src is not version-linked (no rev/tag/url containing ${version})",
        ));
    };
    let Some(git_url) = git_url_from_src(&info.git_repo_url, &info.urls) else {
        return Err(unsupported("could not derive a git URL from the evaluated src"));
    };

    debug!("{package}: listing tags on {git_url}");
    let tags = ls_remote_tags(&git_url)?;
    let latest = best_version(&tags, &prefix, &old_version);
    if latest == old_version {
        debug!("{package}: already at latest version {latest}");
        return Ok(());
    }
    debug!("{package}: {old_version} -> {latest}");

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

    // Keep rev/tag in sync when it is a literal (not a template or
    // bare variable reference).  Only rewrite when the old value equals
    // `prefix + old_version`.
    for key in ["rev", "tag"] {
        let Some(value) = extract_assignment(src_scope(&text), key) else {
            continue;
        };
        if value == format!("{prefix}{old_version}") {
            let old_assignment = format!("{key} = \"{value}\";");
            let new_assignment = format!("{key} = \"{prefix}{latest}\";");
            let (start, end) = src_block_range(&text).unwrap_or((0, text.len()));
            let scope = text[start..end].replacen(&old_assignment, &new_assignment, 1);
            text.replace_range(start..end, &scope);
        }
    }
    text = replace_dep_hashes_with_fake(&text);
    fs::write(file, text).map_err(|e| format!("write {}: {e}", file.display()))?;

    refresh_fake_hashes(root, package, file)
}

/// The well-known fake hash accepted by all Nix fetchers.
const FAKE_HASH: &str = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

/// Repeatedly build the package, replacing fake hashes with the hashes Nix
/// reports it wanted. Nix typically reports one fixed-output mismatch per
/// build (later fetches depend on earlier ones), so loop until none remain.
fn refresh_fake_hashes(root: &Path, package: &str, file: &Path) -> Result<(), String> {
    loop {
        let mut text =
            fs::read_to_string(file).map_err(|e| format!("read {}: {e}", file.display()))?;
        let remaining = text.matches(FAKE_HASH).count();
        if remaining == 0 {
            return Ok(());
        }
        debug!("{package}: building to compute {remaining} remaining hash(es)...");
        let (built, log) = nix::build_package_with_log(root, package)?;
        let wanted = extract_wanted_hashes(&log);
        if wanted.is_empty() {
            return Err(if built {
                "build did not report a hash mismatch for the remaining fake hash(es)".to_string()
            } else {
                format!("could not extract wanted hash from build output\n{log}")
            });
        }
        for hash in &wanted {
            text = text.replacen(FAKE_HASH, hash, 1);
        }
        fs::write(file, &text).map_err(|e| format!("write {}: {e}", file.display()))?;
    }
}

/// Find the tag prefix from the first rev/tag/url assignment that references
/// the version, e.g. `rev = "v${version}"` -> "v",
/// `url = ".../releases/download/app-${version}/x.tar.gz"` -> "app-".
fn version_linked_prefix(text: &str, old_version: &str) -> Option<String> {
    for key in ["rev", "tag", "url"] {
        let Some(template) = extract_assignment(text, key) else {
            continue;
        };
        // Template-linked: ${version} or ${finalAttrs.version} placeholder.
        for placeholder in ["${version}", "${finalAttrs.version}"] {
            if let Some((before, _)) = template.split_once(placeholder) {
                let prefix = before.rsplit(['/', '=']).next().unwrap_or(before);
                return Some(prefix.to_string());
            }
        }
        // Bare variable reference: rev = version; or rev = finalAttrs.version;
        if template == "version" || template == "finalAttrs.version" {
            return Some(String::new());
        }
        // Literal rev/tag: the value ends with the current version (e.g.
        // "v1.2.3" with version "1.2.3" -> prefix "v").
        if let Some(prefix) = template.strip_suffix(old_version) {
            return Some(prefix.to_string());
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
    // A plain repo URL (e.g. from fetchgit) - but not a two-segment tarball URL.
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

/// Byte offset just past the first `name = ` that starts at an attribute
/// boundary, so e.g. `rev` does not match `prev`.
fn assignment_value_start(text: &str, name: &str) -> Option<usize> {
    let needle = format!("{name} = ");
    let mut from = 0;
    while let Some(pos) = text[from..].find(&needle) {
        let abs = from + pos;
        let at_boundary = abs == 0
            || !matches!(
                text.as_bytes()[abs - 1],
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'\''
            );
        if at_boundary {
            return Some(abs + needle.len());
        }
        from = abs + needle.len();
    }
    None
}

/// Extract the value of a `name = "value";` assignment.
fn extract_assignment(text: &str, name: &str) -> Option<String> {
    let start = assignment_value_start(text, name)?;
    let rest = &text[start..];
    let after = rest.trim_start();
    let leading_ws = rest.len() - after.len();
    if after.starts_with('"') {
        // Quoted string assignment: name = "..."
        let inner_start = start + leading_ws + 1;
        let end = text[inner_start..].find('"')?;
        return Some(text[inner_start..inner_start + end].to_string());
    }
    // Bare variable reference: name = ident;
    // Match a single Nix identifier (letters, digits, dots, underscores).
    for (i, c) in after.char_indices() {
        if !matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '.') {
            return Some(after[..i].to_string());
        }
    }
    Some(after.to_string())
}

/// Byte range of the `{ ... }` argument of `src = fetcher { ... }`, so lookups
/// of rev/tag/url/hash do not pick up patches or other fetchers in the file.
/// `None` when src is not a fetcher call with an inline attrset.
fn src_block_range(text: &str) -> Option<(usize, usize)> {
    let start = assignment_value_start(text, "src")?;
    let open = start + text[start..].find('{')?;
    if text[start..open].contains(';') {
        return None;
    }
    let mut depth = 0usize;
    for (i, ch) in text[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((open, open + i + 1));
                }
            }
            _ => {}
        }
    }
    None
}

/// The src fetcher's attrset if there is one, otherwise the whole file.
fn src_scope(text: &str) -> &str {
    src_block_range(text).map_or(text, |(start, end)| &text[start..end])
}

fn replace_src_hash(text: &str, new_hash: &str) -> Result<String, String> {
    let src_pos = assignment_value_start(text, "src").ok_or("missing src assignment")?;
    // Prefer the src fetcher's own attrset; fall back to "anything after src".
    let (scope_start, scope_end) = src_block_range(text).unwrap_or((src_pos, text.len()));
    let scope = &text[scope_start..scope_end];
    let value_start = ["hash", "sha256"]
        .into_iter()
        .filter_map(|name| assignment_value_start(scope, name))
        .filter(|&pos| scope[pos..].starts_with('"'))
        .min()
        .ok_or("missing src hash")?;
    let start = scope_start + value_start + 1;
    let end = start + text[start..].find('"').ok_or("unterminated src hash")?;
    Ok(format!("{}{new_hash}{}", &text[..start], &text[end..]))
}

const DEPENDENCY_HASH_KEYS: [&str; 9] = [
    "cargoHash",
    "vendorHash",
    "npmDepsHash",
    "yarnHash",
    "pomHash",
    "mvnHash",
    "mixHash",
    "nugetHash",
    "dotnetHash",
];

fn replace_dep_hashes_with_fake(text: &str) -> String {
    let mut out = text.to_string();
    for key in DEPENDENCY_HASH_KEYS {
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

fn extract_wanted_hashes(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("got:"))
        .map(str::trim)
        .filter(|hash| hash.starts_with("sha256-"))
        .map(str::to_string)
        .collect()
}

fn parse_version(version: &str) -> Option<Vec<u64>> {
    version.split('.').map(|p| p.parse().ok()).collect()
}

/// Numeric comparison where missing trailing components count as 0, so
/// `1.2.0` is not newer than `1.2`. Non-numeric versions sort lowest.
fn version_gt(a: &str, b: &str) -> bool {
    match (parse_version(a), parse_version(b)) {
        (Some(mut a), Some(mut b)) => {
            let len = a.len().max(b.len());
            a.resize(len, 0);
            b.resize(len, 0);
            a > b
        }
        (a, b) => a > b,
    }
}

fn pr_title(package: &str, old_version: &str, new_version: &str) -> String {
    format!("feat({package}): {old_version} -> {new_version}")
}

/// `build_ok` is `None` when the build was skipped (no-build list).
fn pr_body(
    root: &Path,
    package: &str,
    old_version: &str,
    new_version: &str,
    build_ok: Option<bool>,
    build_log: &str,
) -> String {
    let os = if env::consts::OS == "macos" { "darwin" } else { env::consts::OS };
    let (check, note) = match build_ok {
        Some(true) => ("x", ""),
        Some(false) => (" ", " failed ⚠️"),
        None => (" ", " skipped ⏩ (in no-build list)"),
    };
    let mut body = format!(
        "Update `{package}` from `{old_version}` to `{new_version}`.\n\n\
         **Built on platform:**\n\n- [{check}] `{}-{os}`{note}\n\n",
        env::consts::ARCH
    );
    let (input, rev) = nix::flake_input_info(root);
    if !input.is_empty() && !rev.is_empty() {
        body.push_str(&format!("**Flake:** `{input}` at `{rev}`\n\n"));
    } else if !input.is_empty() {
        body.push_str(&format!("**Flake:** `{input}`\n\n"));
    }
    body.push_str("Generated by [bumpkin](https://github.com/74k1/bumpkin).");
    if build_ok == Some(false) {
        let lines: Vec<&str> = build_log.lines().collect();
        let tail = lines[lines.len().saturating_sub(20)..].join("\n");
        body.push_str(&format!(
            "\n\n<details>\n<summary>Build log (last 20 lines)</summary>\n\n```\n{tail}\n```\n</details>\n"
        ));
    }
    body
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
        let path = temp.to_str().ok_or("non-utf8 temp path")?;
        git::run(root, &["worktree", "add", "--quiet", "--detach", path, "HEAD"])?;
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
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("start git apply: {e}"))?;
    apply
        .stdin
        .take()
        .ok_or("git apply stdin unavailable")?
        .write_all(&diff.stdout)
        .map_err(|e| format!("write git apply diff: {e}"))?;
    let status = apply
        .wait()
        .map_err(|e| format!("wait for git apply: {e}"))?;
    if !status.success() {
        return Err(format!("git apply exited with {status}"));
    }
    let identity = ["-c", "user.name=bumpkin", "-c", "user.email=bumpkin@example.invalid"];
    git::run(to, &[&identity[..], &["commit", "--quiet", "-m", "bumpkin dry-run baseline"]].concat())
}

impl Drop for TempWorktree {
    fn drop(&mut self) {
        let path = self.root.to_str().unwrap_or("");
        let _ = git::run(&self.original, &["worktree", "remove", "--force", path]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_linked_prefix_from_rev_tag_and_url() {
        assert_eq!(
            version_linked_prefix(r#"rev = "v${version}";"#, "1.0").as_deref(),
            Some("v")
        );
        assert_eq!(
            version_linked_prefix(r#"tag = "release-${finalAttrs.version}";"#, "1.0").as_deref(),
            Some("release-")
        );
        assert_eq!(
            version_linked_prefix(
                r#"url = "https://github.com/o/r/releases/download/app-${version}/x.tar.gz";"#,
                "1.0",
            )
            .as_deref(),
            Some("app-")
        );
        // SHA rev (not version-linked at all).
        assert_eq!(version_linked_prefix(r#"rev = "deadbeef";"#, "1.0"), None);
        // Literal rev ending with the version: recover prefix.
        assert_eq!(version_linked_prefix(r#"rev = "v1.2.3";"#, "1.2.3").as_deref(), Some("v"));
        assert_eq!(version_linked_prefix(r#"rev = "1.2.3";"#, "1.2.3").as_deref(), Some(""));
        // Bare variable reference (rev = version; without quotes).
        assert_eq!(
            version_linked_prefix(r#"rev = version;"#, "1.0").as_deref(),
            Some("")
        );
        assert_eq!(
            version_linked_prefix(r#"rev = finalAttrs.version;"#, "1.0").as_deref(),
            Some("")
        );
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
    fn extract_assignment_handles_bare_variable_reference() {
        assert_eq!(
            extract_assignment(r#"rev = version;"#, "rev").as_deref(),
            Some("version")
        );
        assert_eq!(
            extract_assignment(r#"rev = finalAttrs.version;"#, "rev").as_deref(),
            Some("finalAttrs.version")
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

    // A patch hash (or any other fetcher) after `src = ` must not be mistaken
    // for the src hash, and `outputHash`-style names must not match `hash`.
    const SRC_WITH_PATCH: &str = r#"
  src = fetchFromGitHub {
    owner = "o";
    repo = "r";
    rev = "v1.0";
    hash = "sha256-src";
  };
  patches = [
    (fetchpatch {
      url = "https://example.com/fix-${version}.patch";
      hash = "sha256-patch";
    })
  ];
"#;

    #[test]
    fn src_hash_replacement_stays_inside_src_block() {
        let reordered = SRC_WITH_PATCH.replace(
            "    rev = \"v1.0\";\n    hash = \"sha256-src\";\n",
            "    outputHash = \"x\";\n    rev = \"v1.0\";\n    sha256 = \"sha256-src\";\n",
        );
        let hashed = replace_src_hash(&reordered, "sha256-new").unwrap();
        assert!(hashed.contains("sha256 = \"sha256-new\""));
        assert!(hashed.contains("outputHash = \"x\""));
        assert!(hashed.contains("hash = \"sha256-patch\""));
    }

    #[test]
    fn src_scope_prefers_src_fetcher_over_patch_urls() {
        // The patch url references ${version}, but the src rev is the link.
        assert_eq!(
            version_linked_prefix(src_scope(SRC_WITH_PATCH), "1.0").as_deref(),
            Some("v")
        );
        assert_eq!(src_block_range("src = ./.;\nfoo = { };"), None);
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
        assert!(!version_gt("1.2.0", "1.2"));
        assert!(version_gt("1.2.1", "1.2"));
        assert_eq!(parse_version("1.2.3"), Some(vec![1, 2, 3]));
        assert_eq!(parse_version("1.2-beta"), None);
    }
}
