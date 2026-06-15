//! Repology version oracle - queries the free repology.org API to discover
//! newer versions for packages without updateScripts.
//!
//! Rate limit: ~1 request/second. No API key required.

use std::process::Command;

/// Try to find a newer version via repology. Returns `Some(version)` if a
/// newer version is found, or `None` if not found / no newer version / error.
pub fn latest_version(pname: &str) -> Option<String> {
    let url = format!("https://repology.org/api/v1/project/{pname}");
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "10",
            "-H",
            "User-Agent: bumpkin (https://github.com/74k1/bumpkin)",
            &url,
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let body = String::from_utf8_lossy(&out.stdout);
    // Repology returns a JSON array of package entries per repo.
    // We look for entries with "status":"newest" and extract "version".
    let mut best: Option<(Vec<u32>, String)> = None;

    let mut pos = 0;
    while pos < body.len() {
        // Find next "status":"newest"
        let status_pos = body[pos..].find("\"status\":\"newest\"");
        let Some(sp) = status_pos else { break };
        let entry_start = body[..pos + sp].rfind('{');
        let Some(es) = entry_start else {
            pos += sp + 1;
            continue;
        };
        let entry_end = body[pos + sp..].find("},");
        let Some(ee) = entry_end else {
            pos += sp + 1;
            continue;
        };
        let entry = &body[es..pos + sp + ee + 1];

        // Extract version from this entry
        if let Some(v_start) = entry.find("\"version\":\"") {
            let v = &entry[v_start + 11..];
            if let Some(v_end) = v.find('"') {
                let version = &v[..v_end];
                let parts = parse_version_parts(version);
                match &best {
                    Some((best_p, _)) if parts <= *best_p => {}
                    _ => {
                        best = Some((parts, version.to_string()));
                    }
                }
            }
        }
        pos += sp + 1;
    }
    best.map(|(_, v)| v)
}

fn parse_version_parts(v: &str) -> Vec<u32> {
    let parts: Vec<&str> = v
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return vec![];
    }
    parts.iter().filter_map(|p| p.parse().ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_parts_works() {
        assert_eq!(parse_version_parts("1.2.3"), vec![1, 2, 3]);
        assert_eq!(parse_version_parts("3.1.2.4938"), vec![3, 1, 2, 4938]);
        assert_eq!(parse_version_parts("v1.0.0"), vec![1, 0, 0]);
        assert_eq!(parse_version_parts("unstable-2024-01-01"), vec![2024, 1, 1]);
        assert!(parse_version_parts("hello").is_empty());
    }
}
