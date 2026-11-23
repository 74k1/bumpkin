//! PR creation via the `gh` CLI, or curl against the GitHub / Gitea / Forgejo
//! REST API.

use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

pub enum Forge {
    Gh,
    Api { token: String, pulls_url: String },
}

/// Pick the backend from `--forge` (auto, gh, api) and the `origin` URL.
/// `auto` uses `gh` for github.com when installed, otherwise the REST API.
pub fn resolve(
    mode: &str,
    api_url: Option<&str>,
    token: Option<&str>,
    repo_url: &str,
) -> Result<Forge, String> {
    let (host, owner, repo) = parse_repo_url(repo_url)?;
    let github = host == "github.com";
    let gh = match mode {
        "gh" => true,
        "auto" => github && Command::new("gh").arg("--version").output().is_ok(),
        "api" => false,
        other => return Err(format!("unknown forge: {other} (expected auto, gh, or api)")),
    };
    if gh {
        return Ok(Forge::Gh);
    }
    let token = token.ok_or(
        "the forge REST API needs a token (set GITHUB_TOKEN, or forgeTokenFile in the NixOS module)",
    )?;
    let api_base = match api_url {
        Some(url) => url.trim_end_matches('/').to_string(),
        None if github => "https://api.github.com".to_string(),
        // Assume a Gitea/Forgejo-style self-hosted instance.
        None => format!("https://{host}/api/v1"),
    };
    Ok(Forge::Api {
        token: token.to_string(),
        pulls_url: format!("{api_base}/repos/{owner}/{repo}/pulls"),
    })
}

impl Forge {
    /// Open a PR and return its URL. If the branch already has an open PR,
    /// every backend fails with a message containing "already exists".
    pub fn create_pr(
        &self,
        root: &Path,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<String, String> {
        match self {
            Forge::Gh => {
                let out = Command::new("gh")
                    .args(["pr", "create", "--head", head, "--base", base])
                    .args(["--title", title, "--body", body])
                    .current_dir(root)
                    .output()
                    .map_err(|e| format!("gh pr create: {e}"))?;
                if out.status.success() {
                    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
                } else {
                    Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
                }
            }
            Forge::Api { token, pulls_url } => {
                let json = format!(
                    r#"{{"title":"{}","head":"{}","base":"{}","body":"{}"}}"#,
                    json_escape(title),
                    json_escape(head),
                    json_escape(base),
                    json_escape(body),
                );
                let response = curl_post(pulls_url, token, &json)?;
                pr_url(&response).ok_or(response)
            }
        }
    }

    /// Merge a PR by its URL, or schedule the merge until its checks succeed.
    /// Gitea/Forgejo only: GitHub has no REST endpoint for auto-merge.
    pub fn merge_pr(&self, url: &str) -> Result<(), String> {
        let Forge::Api { token, pulls_url } = self else {
            return Err("merging needs the Gitea/Forgejo REST API (--forge api)".to_string());
        };
        let index = url.rsplit('/').next().unwrap_or_default();
        let json = r#"{"Do":"merge","merge_when_checks_succeed":true,"delete_branch_after_merge":true}"#;
        let merge_url = format!("{pulls_url}/{index}/merge");
        // A new PR is unmergeable until the forge's async conflict check ends.
        for _ in 0..5 {
            match curl_post(&merge_url, token, json) {
                Err(err) if err.contains("try again later") => {
                    std::thread::sleep(std::time::Duration::from_secs(3))
                }
                result => return result.map(drop),
            }
        }
        curl_post(&merge_url, token, json).map(drop)
    }
}

/// POST JSON with the Authorization header passed via a stdin config file
/// instead of argv, so the token is not visible in /proc/<pid>/cmdline.
fn curl_post(url: &str, token: &str, body: &str) -> Result<String, String> {
    let mut child = Command::new("curl")
        .args(["-sS", "--fail-with-body", "--config", "-", "-X", "POST", "-d", body])
        .args(["-H", "Accept: application/vnd.github+json"])
        .args(["-H", "Content-Type: application/json", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl {url}: {e}"))?;
    let escaped = token.replace('\\', "\\\\").replace('"', "\\\"");
    child
        .stdin
        .take()
        .ok_or("curl stdin unavailable")?
        .write_all(format!("header = \"Authorization: Bearer {escaped}\"\n").as_bytes())
        .map_err(|e| format!("write curl config: {e}"))?;
    let out = child.wait_with_output().map_err(|e| format!("curl {url}: {e}"))?;
    let output = String::from_utf8_lossy(&[out.stdout, out.stderr].concat()).into_owned();
    if out.status.success() { Ok(output) } else { Err(output) }
}

fn json_escape(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out
}

/// The PR's `html_url` in a create-PR response. Nested objects (`user`,
/// `repo`) carry their own `html_url`, so only a pull path counts.
fn pr_url(json: &str) -> Option<String> {
    json.split("\"html_url\"").skip(1).find_map(|rest| {
        let value = rest.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
        let url = &value[..value.find('"')?];
        url.contains("/pull").then(|| url.replace("\\/", "/"))
    })
}

/// Parse `(host, owner, repo)` from a git remote URL: `scheme://[user@]host[:port]/owner/repo`
/// (https, ssh, git+ssh) or scp-style `[user@]host:owner/repo`.
pub fn parse_repo_url(url: &str) -> Result<(String, String, String), String> {
    let split = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/'),
        None => url.split_once(':'),
    };
    let (authority, path) = split.ok_or(format!("could not parse git remote URL: {url}"))?;
    // Drop user@ and :port; the API lives on standard ports.
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host.split_once(':').map_or(host, |(h, _)| h);
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/').ok_or(format!("missing owner/repo in {url}"))?;
    Ok((host.to_string(), owner.to_string(), repo.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_repo_url_variants() {
        let parsed = |url| {
            let (host, owner, repo) = parse_repo_url(url).unwrap();
            format!("{host} {owner} {repo}")
        };
        assert_eq!(parsed("git@github.com:74k1/tixpkgs.git"), "github.com 74k1 tixpkgs");
        assert_eq!(
            parsed("ssh://forge@git.yukume.com/74k1/tixpkgs-unfree.git"),
            "git.yukume.com 74k1 tixpkgs-unfree"
        );
        assert_eq!(
            parsed("git+ssh://git@host.example.com:2222/owner/repo.git"),
            "host.example.com owner repo"
        );
        assert_eq!(parsed("https://github.com/NixOS/nixpkgs.git"), "github.com NixOS nixpkgs");
        assert_eq!(parsed("https://github.com/a/b"), "github.com a b");
        assert!(parse_repo_url("/local/path").is_err());
    }

    #[test]
    fn resolve_picks_api_base_from_host() {
        let pulls = |mode, api_url, repo_url| match resolve(mode, api_url, Some("t"), repo_url) {
            Ok(Forge::Api { pulls_url, .. }) => pulls_url,
            _ => panic!("expected API backend"),
        };
        assert_eq!(
            pulls("api", None, "git@github.com:o/r.git"),
            "https://api.github.com/repos/o/r/pulls"
        );
        assert_eq!(
            pulls("auto", None, "https://gitea.example.com/o/r.git"),
            "https://gitea.example.com/api/v1/repos/o/r/pulls"
        );
        assert_eq!(
            pulls("api", Some("https://x.example/api/v1/"), "https://gitea.example.com/o/r"),
            "https://x.example/api/v1/repos/o/r/pulls"
        );
        assert!(resolve("api", None, None, "https://gitea.example.com/o/r").is_err());
        assert!(resolve("nope", None, Some("t"), "https://github.com/o/r").is_err());
    }

    #[test]
    fn pr_url_skips_nested_html_urls() {
        // Gitea puts the user object (with its own html_url) before the PR's.
        let json = r#"{"id":1,"user":{"html_url":"https://g.example/o"},"html_url": "https://g.example/o/r/pulls/7"}"#;
        assert_eq!(pr_url(json).as_deref(), Some("https://g.example/o/r/pulls/7"));
        assert_eq!(
            pr_url("{\n  \"html_url\": \"https://github.com/o/r/pull/1\"\n}").as_deref(),
            Some("https://github.com/o/r/pull/1")
        );
        assert_eq!(pr_url(r#"{"message":"A pull request already exists for o:b."}"#), None);
    }
}
