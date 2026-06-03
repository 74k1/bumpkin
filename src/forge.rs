//! Forge abstraction for PR creation.
//!
//! Supports GitHub, Gitea, and Forgejo via two backends:
//! - `GitHubCli`: wraps the `gh` binary
//! - `CompatibleApi`: direct curl calls to the REST API (zero deps, works with Gitea/Forgejo)

use std::{
    path::Path,
    process::Command,
};

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
            let token = token
                .ok_or("forge `api` requires a token (set GITHUB_TOKEN or ghTokenFile)")?;
            let base = api_url
                .ok_or("forge `api` requires --forge-api-url")?;
            CompatibleApi::new(token, repo_url, Some(base))
                .map(ForgeBackend::CompatibleApi)
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

        other => Err(format!("unknown forge: {other} (expected auto, github-cli, github-api, or api)")),
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
                "pr", "list", "--head", head, "--state", "open",
                "--json", "number", "--jq", ".[0].number",
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
                "pr", "create",
                "--head", head,
                "--base", base,
                "--title", title,
                "--body", body,
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
        _repo: &str,
    ) -> Result<Option<u32>, String> {
        // GET /repos/{owner}/{repo}/pulls?head={owner}:{head}&state=open&base={base}
        let url = format!(
            "{}/repos/{owner}/{_repo}/pulls?head={owner}:{head}&state=open&base={base}",
            self.api_base, owner = owner, _repo = _repo, head = head, base = base
        );

        let out = curl_get(&url, &self.token)?;
        let stdout = String::from_utf8_lossy(&out.stdout);

        // The response is a JSON array. Extract the first PR number.
        let number = stdout
            .lines()
            .find(|line| line.trim().starts_with("\"number\":"))
            .or_else(|| stdout.lines().find(|line| line.contains("\"number\"")))
            .and_then(|line| {
                line.split(':')
                    .nth(1)
                    .map(|v| v.trim().trim_end_matches(',').parse::<u32>().ok())
                    .flatten()
            });

        if let Some(number) = number {
            // Only consider it found if we got a valid JSON array response
            if stdout.trim().starts_with('[') {
                return Ok(Some(number));
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
            return Err(format!("create PR failed ({}):\n{stdout}\n{stderr}", out.status));
        }

        // Extract the html_url from the JSON response.
        extract_field(&stdout, "html_url").ok_or_else(|| {
            format!("could not parse PR URL from response:\n{stdout}")
        })
    }
}

// --- Helpers ---

fn curl_get(url: &str, token: &str) -> Result<std::process::Output, String> {
    Command::new("curl")
        .args([
            "-sS",
            "-H", &format!("Authorization: Bearer {token}"),
            "-H", "Accept: application/vnd.github+json",
            url,
        ])
        .output()
        .map_err(|e| format!("curl {url}: {e}"))
}

fn curl_post(url: &str, token: &str, body: &str) -> Result<std::process::Output, String> {
    Command::new("curl")
        .args([
            "-sS",
            "-X", "POST",
            "-H", &format!("Authorization: Bearer {token}"),
            "-H", "Accept: application/vnd.github+json",
            "-H", "Content-Type: application/json",
            "-d", body,
            url,
        ])
        .output()
        .map_err(|e| format!("curl {url}: {e}"))
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

/// Extract a string field value from a JSON response.
fn extract_field(json: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":\"");
    let start = json.find(&needle)? + needle.len();
    let mut chars = json[start..].chars();
    let mut out = String::new();
    let mut escaping = false;
    for ch in &mut chars {
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
        let (host, owner, repo) =
            parse_repo_url("git@github.com:74k1/tixpkgs.git").unwrap();
        assert_eq!(host, "github.com");
        assert_eq!(owner, "74k1");
        assert_eq!(repo, "tixpkgs");
    }

    #[test]
    fn parse_repo_url_https() {
        let (host, owner, repo) =
            parse_repo_url("https://github.com/NixOS/nixpkgs.git").unwrap();
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
        let (host, owner, repo) =
            parse_repo_url("https://github.com/a/b").unwrap();
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
}
