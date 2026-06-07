//! Forge abstraction for PR creation.
//!
//! Supports GitHub, Gitea, and Forgejo via two backends:
//! - `GitHubCli`: wraps the `gh` binary
//! - `CompatibleApi`: direct curl calls to the REST API (zero deps, works with Gitea/Forgejo)

use std::{path::Path, process::Command};

/// Resolve which forge backend to use from options.
pub fn resolve(
    mode: &str,
    api_url: Option<&str>,
    token: Option<&str>,
    repo_url: &str,
) -> Result<ForgeBackend, String> {
    match mode {
        "github-cli" => Ok(ForgeBackend::GitHubCli(GitHubCli)),

        "github-api" => {
            let token = token
                .ok_or("forge `github-api` requires a token (set GITHUB_TOKEN or ghTokenFile)")?;
            CompatibleApi::new(token, repo_url, Some("https://api.github.com"))
                .map(ForgeBackend::CompatibleApi)
        }

        "api" => {
            let token =
                token.ok_or("forge `api` requires a token (set GITHUB_TOKEN or ghTokenFile)")?;
            let base = api_url.ok_or("forge `api` requires --forge-api-url")?;
            CompatibleApi::new(token, repo_url, Some(base)).map(ForgeBackend::CompatibleApi)
        }

        "auto" => {
            // Prefer gh CLI if available; otherwise use curl against GitHub API.
            if Command::new("gh").arg("--version").output().is_ok() {
                return Ok(ForgeBackend::GitHubCli(GitHubCli));
            }
            let token = token
                .ok_or("forge `auto` fell back to GitHub API but no token is available (set GITHUB_TOKEN or ghTokenFile)")?;
            CompatibleApi::new(token, repo_url, Some("https://api.github.com"))
                .map(ForgeBackend::CompatibleApi)
        }

        other => Err(format!(
            "unknown forge: {other} (expected auto, github-cli, github-api, or api)"
        )),
    }
}

/// Resolved forge backend.
pub enum ForgeBackend {
    GitHubCli(GitHubCli),
    CompatibleApi(CompatibleApi),
}

impl ForgeBackend {
    pub fn find_existing_pr(
        &self,
        root: &Path,
        head: &str,
        base: &str,
        owner: &str,
        repo: &str,
    ) -> Result<Option<u32>, String> {
        match self {
            ForgeBackend::GitHubCli(gh) => gh.find_existing_pr(root, head),
            ForgeBackend::CompatibleApi(api) => api.find_existing_pr(head, base, owner, repo),
        }
    }

    pub fn create_pr(
        &self,
        root: &Path,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
        owner: &str,
        repo: &str,
    ) -> Result<String, String> {
        match self {
            ForgeBackend::GitHubCli(gh) => gh.create_pr(root, head, base, title, body),
            ForgeBackend::CompatibleApi(api) => api.create_pr(head, base, title, body, owner, repo),
        }
    }
}

// --- GitHub CLI backend ---

pub struct GitHubCli;

impl GitHubCli {
    fn find_existing_pr(&self, root: &Path, head: &str) -> Result<Option<u32>, String> {
        let out = Command::new("gh")
            .args([
                "pr",
                "list",
                "--head",
                head,
                "--state",
                "open",
                "--json",
                "number",
                "--jq",
                ".[0].number",
            ])
            .current_dir(root)
            .output()
            .map_err(|e| format!("gh pr list: {e}"))?;

        if !out.status.success() {
            let msg = String::from_utf8_lossy(&out.stderr);
            // If there are no PRs, gh exits 0 with empty output; non-zero
            // usually means auth or network issues.
            return Err(format!("gh pr list failed: {}", msg.trim()));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            Ok(None)
        } else {
            trimmed
                .parse::<u32>()
                .map(Some)
                .map_err(|e| format!("parse gh pr number from {trimmed:?}: {e}"))
        }
    }

    fn create_pr(
        &self,
        root: &Path,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<String, String> {
        let out = Command::new("gh")
            .args([
                "pr", "create", "--head", head, "--base", base, "--title", title, "--body", body,
            ])
            .current_dir(root)
            .output()
            .map_err(|e| format!("gh pr create: {e}"))?;

        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(format!(
                "gh pr create failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
}

// --- Compatible REST API backend (GitHub / Gitea / Forgejo) ---

pub struct CompatibleApi {
    token: String,
    api_base: String,
    pulls_url: String,
}

impl CompatibleApi {
    /// Create a new API backend.
    ///
    /// `api_base` is the base URL for the REST API, e.g.:
    /// - GitHub: `https://api.github.com`
    /// - Gitea:   `https://gitea.example.com/api/v1`
    /// - Forgejo: `https://forgejo.example.com/api/v1`
    ///
    /// `repo_url` is the git remote URL (used to infer owner/repo).
    pub fn new(token: &str, repo_url: &str, api_base: Option<&str>) -> Result<Self, String> {
        let (host, owner, repo) = parse_repo_url(repo_url)?;
        let api_base = match api_base {
            Some(url) => url.trim_end_matches('/').to_string(),
            None => default_api_base(&host),
        };
        let pulls_url = format!("{api_base}/repos/{owner}/{repo}/pulls");

        Ok(CompatibleApi {
            token: token.to_string(),
            api_base,
            pulls_url,
        })
    }

    fn find_existing_pr(
        &self,
        head: &str,
        base: &str,
        owner: &str,
        repo: &str,
    ) -> Result<Option<u32>, String> {
        // GitHub supports filtering by head=owner:branch; Gitea/Forgejo ignore
        // unknown query params, so the head match below is done client-side.
        let url = format!(
            "{}/repos/{owner}/{repo}/pulls?state=open&base={base}&head={owner}:{head}&per_page=100&limit=100",
            self.api_base
        );

        let out = curl_get(&url, &self.token)?;
        let body = String::from_utf8_lossy(&out.stdout);
        let trimmed = body.trim();
        if !trimmed.starts_with('[') {
            return Err(format!("unexpected pulls response:\n{trimmed}"));
        }

        for object in top_level_objects(trimmed) {
            let head_ref = extract_object(object, "head")
                .and_then(|head_obj| extract_string_at_depth1(head_obj, "ref"));
            if head_ref.as_deref() == Some(head) {
                return Ok(extract_u32_at_depth1(object, "number"));
            }
        }
        Ok(None)
    }

    fn create_pr(
        &self,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
        _owner: &str,
        _repo: &str,
    ) -> Result<String, String> {
        let json = serde_json_pr_body(head, base, title, body);

        let out = curl_post(&self.pulls_url, &self.token, &json)?;
        let stdout = String::from_utf8_lossy(&out.stdout);

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(format!(
                "create PR failed ({}):\n{stdout}\n{stderr}",
                out.status
            ));
        }

        // Extract the html_url from the JSON response.
        extract_field(&stdout, "html_url")
            .ok_or_else(|| format!("could not parse PR URL from response:\n{stdout}"))
    }
}

// --- Helpers ---

fn curl_get(url: &str, token: &str) -> Result<std::process::Output, String> {
    curl_with_token(&[], url, token)
}

fn curl_post(url: &str, token: &str, body: &str) -> Result<std::process::Output, String> {
    curl_with_token(
        &[
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "-d",
            body,
        ],
        url,
        token,
    )
}

/// Run curl with the Authorization header passed via a stdin config file
/// instead of argv, so the token is not visible in /proc/<pid>/cmdline.
fn curl_with_token(
    extra_args: &[&str],
    url: &str,
    token: &str,
) -> Result<std::process::Output, String> {
    let mut child = Command::new("curl")
        .args([
            "-sS",
            "--config",
            "-",
            "-H",
            "Accept: application/vnd.github+json",
        ])
        .args(extra_args)
        .arg(url)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl {url}: {e}"))?;
    {
        use std::io::Write;
        let escaped = token.replace('\\', "\\\\").replace('"', "\\\"");
        child
            .stdin
            .as_mut()
            .ok_or("curl stdin unavailable")?
            .write_all(format!("header = \"Authorization: Bearer {escaped}\"\n").as_bytes())
            .map_err(|e| format!("write curl config: {e}"))?;
    }
    child
        .wait_with_output()
        .map_err(|e| format!("curl {url}: {e}"))
}

/// Split a top-level JSON array into its object slices, tracking strings and
/// escapes so braces inside PR titles/bodies don't confuse the count.
fn top_level_objects(json: &str) -> Vec<&str> {
    let mut objects = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut in_string = false;
    let mut escaping = false;
    for (i, ch) in json.char_indices() {
        if escaping {
            escaping = false;
            continue;
        }
        match ch {
            '\\' if in_string => escaping = true,
            '"' => in_string = !in_string,
            '{' if !in_string => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            '}' if !in_string => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    objects.push(&json[start..=i]);
                }
            }
            _ => {}
        }
    }
    objects
}

/// Visit each position of an object that sits at nesting depth 1 (i.e. a key
/// of the object itself, not of a nested object) and return the first one
/// where the callback produces a value.
fn find_at_depth1<T>(object: &str, mut visit: impl FnMut(usize) -> Option<T>) -> Option<T> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaping = false;
    for (i, ch) in object.char_indices() {
        if escaping {
            escaping = false;
            continue;
        }
        match ch {
            '\\' if in_string => escaping = true,
            '"' => {
                if !in_string
                    && depth == 1
                    && let Some(found) = visit(i)
                {
                    return Some(found);
                }
                in_string = !in_string;
            }
            '{' | '[' if !in_string => depth += 1,
            '}' | ']' if !in_string => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    None
}

/// Position of the value after `"field":` (plus whitespace) for a direct key
/// of `object`. A depth-1 string *value* that merely equals the field name
/// (no colon after it) is skipped.
fn value_pos_at_depth1(object: &str, field: &str) -> Option<usize> {
    let needle = format!("\"{field}\"");
    find_at_depth1(object, |i| {
        let rest = object[i..].strip_prefix(&needle)?;
        let colon = rest.find(|c: char| !c.is_whitespace())?;
        let rest = rest[colon..].strip_prefix(':')?;
        let value = rest.find(|c: char| !c.is_whitespace())?;
        Some(i + needle.len() + colon + 1 + value)
    })
}

/// Slice out the `"field": {...}` object that is a direct key of `object`.
fn extract_object<'a>(object: &'a str, field: &str) -> Option<&'a str> {
    let pos = value_pos_at_depth1(object, field)?;
    let inner = &object[pos..];
    if !inner.starts_with('{') {
        return None;
    }
    top_level_objects(inner).into_iter().next()
}

/// Extract a string value for a direct key of `object`.
fn extract_string_at_depth1(object: &str, field: &str) -> Option<String> {
    let pos = value_pos_at_depth1(object, field)?;
    let rest = object[pos..].strip_prefix('"')?;
    unescape_until_quote(rest)
}

/// Extract an unsigned integer value for a direct key of `object`.
fn extract_u32_at_depth1(object: &str, field: &str) -> Option<u32> {
    let pos = value_pos_at_depth1(object, field)?;
    let digits: String = object[pos..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// Hand-rolled minimal JSON for the PR body to avoid pulling in serde.
fn serde_json_pr_body(head: &str, base: &str, title: &str, body: &str) -> String {
    let esc = |s: &str| {
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    };
    format!(
        r#"{{"title":"{}","head":"{}","base":"{}","body":"{}"}}"#,
        esc(title),
        esc(head),
        esc(base),
        esc(body),
    )
}

/// Extract a string field value from a JSON response. Tolerates whitespace
/// after the colon (GitHub pretty-prints its responses).
fn extract_field(json: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\"");
    let mut from = 0;
    while let Some(pos) = json[from..].find(&needle) {
        let after = from + pos + needle.len();
        let rest = json[after..].trim_start();
        if let Some(rest) = rest.strip_prefix(':')
            && let Some(rest) = rest.trim_start().strip_prefix('"')
        {
            return unescape_until_quote(rest);
        }
        from = after;
    }
    None
}

/// Unescape a JSON string up to (excluding) its closing quote.
fn unescape_until_quote(text: &str) -> Option<String> {
    let mut out = String::new();
    let mut escaping = false;
    for ch in text.chars() {
        if escaping {
            match ch {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                '/' => out.push('/'),
                other => {
                    out.push('\\');
                    out.push(other);
                }
            }
            escaping = false;
        } else if ch == '\\' {
            escaping = true;
        } else if ch == '"' {
            return Some(out);
        } else {
            out.push(ch);
        }
    }
    None
}

/// Parse owner/repo from a git remote URL.
///
/// Supports HTTPS (`https://host/owner/repo.git`) and SSH
/// (`git@host:owner/repo.git`) URLs.
pub fn parse_repo_url(url: &str) -> Result<(String, String, String), String> {
    // SSH style: git@host:owner/repo.git
    if let Some(rest) = url.strip_prefix("git@") {
        let (host, rest) = rest.split_once(':').ok_or("invalid SSH git URL")?;
        let path = rest.strip_suffix(".git").unwrap_or(rest);
        let mut parts = path.splitn(2, '/');
        let owner = parts.next().ok_or("missing owner in git URL")?;
        let repo = parts.next().ok_or("missing repo in git URL")?;
        return Ok((host.to_string(), owner.to_string(), repo.to_string()));
    }

    // HTTPS style: https://host/owner/repo.git
    if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        let (host, path) = rest.split_once('/').ok_or("invalid HTTPS git URL")?;
        let path = path.strip_suffix(".git").unwrap_or(path);
        let mut parts = path.splitn(2, '/');
        let owner = parts.next().ok_or("missing owner in git URL")?;
        let repo = parts.next().ok_or("missing repo in git URL")?;
        return Ok((host.to_string(), owner.to_string(), repo.to_string()));
    }

    Err(format!("could not parse git remote URL: {url}"))
}

/// Default API base URL for well-known hosts.
fn default_api_base(host: &str) -> String {
    if host == "github.com" {
        "https://api.github.com".to_string()
    } else {
        // Assume Gitea/Forgejo-style self-hosted instance.
        format!("https://{host}/api/v1")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_repo_url_ssh() {
        let (host, owner, repo) = parse_repo_url("git@github.com:74k1/tixpkgs.git").unwrap();
        assert_eq!(host, "github.com");
        assert_eq!(owner, "74k1");
        assert_eq!(repo, "tixpkgs");
    }

    #[test]
    fn parse_repo_url_https() {
        let (host, owner, repo) = parse_repo_url("https://github.com/NixOS/nixpkgs.git").unwrap();
        assert_eq!(host, "github.com");
        assert_eq!(owner, "NixOS");
        assert_eq!(repo, "nixpkgs");
    }

    #[test]
    fn parse_repo_url_gitea() {
        let (host, owner, repo) =
            parse_repo_url("https://gitea.example.com/myorg/mypkgs.git").unwrap();
        assert_eq!(host, "gitea.example.com");
        assert_eq!(owner, "myorg");
        assert_eq!(repo, "mypkgs");
    }

    #[test]
    fn parse_repo_url_no_dotgit() {
        let (host, owner, repo) = parse_repo_url("https://github.com/a/b").unwrap();
        assert_eq!(host, "github.com");
        assert_eq!(owner, "a");
        assert_eq!(repo, "b");
    }

    #[test]
    fn default_api_base_github() {
        assert_eq!(default_api_base("github.com"), "https://api.github.com");
    }

    #[test]
    fn default_api_base_gitea() {
        assert_eq!(
            default_api_base("gitea.example.com"),
            "https://gitea.example.com/api/v1"
        );
    }

    #[test]
    fn serde_pr_body_generates_valid_json() {
        let json = serde_json_pr_body("feat/x", "main", "Test PR", "Body\nline2");
        assert!(json.contains("\"title\":\"Test PR\""));
        assert!(json.contains("\"head\":\"feat/x\""));
        assert!(json.contains("\\n"));
    }

    #[test]
    fn extract_field_parses_html_url() {
        let json = r#"{"html_url":"https://github.com/o/r/pull/1","number":1}"#;
        assert_eq!(
            extract_field(json, "html_url"),
            Some("https://github.com/o/r/pull/1".to_string())
        );
    }

    #[test]
    fn extract_field_handles_escapes() {
        let json = r#"{"message":"line1\nline2\\path"}"#;
        assert_eq!(
            extract_field(json, "message"),
            Some("line1\nline2\\path".to_string())
        );
    }

    #[test]
    fn extract_field_handles_pretty_printed_json() {
        let json = "{\n  \"html_url\": \"https://github.com/o/r/pull/1\"\n}";
        assert_eq!(
            extract_field(json, "html_url"),
            Some("https://github.com/o/r/pull/1".to_string())
        );
    }

    // Compact single-line JSON, as returned by Gitea/Forgejo. The title
    // contains braces and a quoted "number" to stress the scanner.
    const GITEA_PULLS: &str = r#"[{"id":901,"number":17,"title":"chore: weird {braces} and \"number\": 99","head":{"label":"bumpkin/foo","ref":"bumpkin/foo","repo":{"id":3,"full_name":"o/r"}},"base":{"ref":"main"}},{"id":902,"number":18,"title":"feat(bar): 1 -> 2","head":{"ref":"bumpkin/bar","repo":{"id":3}},"base":{"ref":"main"}}]"#;

    #[test]
    fn top_level_objects_split_compact_array() {
        let objects = top_level_objects(GITEA_PULLS.trim());
        assert_eq!(objects.len(), 2);
    }

    #[test]
    fn pr_dedup_matches_head_ref_client_side() {
        let objects = top_level_objects(GITEA_PULLS.trim());
        let found = objects.iter().find_map(|object| {
            let head_ref = extract_object(object, "head")
                .and_then(|head| extract_string_at_depth1(head, "ref"));
            (head_ref.as_deref() == Some("bumpkin/bar"))
                .then(|| extract_u32_at_depth1(object, "number"))
                .flatten()
        });
        assert_eq!(found, Some(18));
    }

    #[test]
    fn pr_dedup_ignores_number_in_strings_and_nested_objects() {
        let objects = top_level_objects(GITEA_PULLS.trim());
        // The first object's number is 17, not the 901 id, the 99 in the
        // title, or the nested repo id.
        assert_eq!(extract_u32_at_depth1(objects[0], "number"), Some(17));
        let head = extract_object(objects[0], "head").unwrap();
        assert_eq!(
            extract_string_at_depth1(head, "ref").as_deref(),
            Some("bumpkin/foo")
        );
    }

    #[test]
    fn pr_dedup_handles_pretty_printed_github_response() {
        let json = "[\n  {\n    \"number\": 5,\n    \"head\": {\n      \"ref\": \"bumpkin/foo\"\n    }\n  }\n]";
        let objects = top_level_objects(json.trim());
        assert_eq!(objects.len(), 1);
        let head = extract_object(objects[0], "head").unwrap();
        assert_eq!(
            extract_string_at_depth1(head, "ref").as_deref(),
            Some("bumpkin/foo")
        );
        assert_eq!(extract_u32_at_depth1(objects[0], "number"), Some(5));
    }
}
