pub(crate) mod forge;
mod git;
mod nix;
pub(crate) mod packages;
pub(crate) mod repology;
mod update;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "bumpkin",
    about = "Small Rust updater/PR steward for Nix flake package sets",
    version
)]
pub struct Cli {
    /// Root path of the flake repository
    #[arg(
        short = 'C',
        long = "root",
        global = true,
        default_value = ".",
        value_name = "PATH"
    )]
    pub root: PathBuf,

    /// Show detailed output (internal steps, diffs, etc.)
    #[arg(short = 'v', long = "verbose", global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// List packages maintained by a maintainer
    List {
        /// Maintainer handle (e.g., github username)
        #[arg(short = 'm', long = "maintainer")]
        maintainer: Option<String>,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Dry-run: show what would change without mutating the repo
    DryRun {
        /// Single package to check
        #[arg(short = 'p', long = "package")]
        package: Option<String>,

        /// Maintainer whose packages to check
        #[arg(short = 'm', long = "maintainer")]
        maintainer: Option<String>,

        /// Comma-separated package names to skip
        #[arg(long, value_delimiter = ',')]
        skip: Vec<String>,
    },

    /// Run a package's updateScript directly and exit
    RunUpdateScript {
        /// Package attribute name
        #[arg(short = 'p', long = "package")]
        package: String,
    },

    /// Update packages to latest versions
    Update {
        /// Single package to update
        #[arg(short = 'p', long = "package")]
        package: Option<String>,

        /// Maintainer whose packages to update
        #[arg(short = 'm', long = "maintainer")]
        maintainer: Option<String>,

        /// Create a per-package commit on a dedicated branch
        #[arg(long)]
        commit: bool,

        /// Sign commits with GPG or SSH
        #[arg(long)]
        signed: bool,

        /// Push per-package branches to origin
        #[arg(long)]
        push: bool,

        /// Create pull requests for pushed branches
        #[arg(long)]
        pr: bool,

        /// Comma-separated package names to skip
        #[arg(long, value_delimiter = ',')]
        skip: Vec<String>,

        /// Comma-separated packages to skip building (still update version/hash)
        #[arg(long = "no-build", value_delimiter = ',')]
        no_build: Vec<String>,

        /// GPG/SSH signing key identifier
        #[arg(long = "signing-key")]
        signing_key: Option<String>,

        /// Git gpg.format value (openpgp or ssh)
        #[arg(long = "gpg-format")]
        gpg_format: Option<String>,

        /// Forge backend: auto, github-cli, github-api, or api
        #[arg(long)]
        forge: Option<String>,

        /// API base URL when forge is api
        #[arg(long = "forge-api-url")]
        forge_api_url: Option<String>,
    },
}

pub use config::Config;

/// Load the first config file found (.bumpkin.nix, XDG, /var/bumpkin).
/// Called by main before tracing is initialized, so `verbose = true` in the
/// config can take effect.
pub fn load_config() -> Result<Config, String> {
    config::load_first().map(Option::unwrap_or_default)
}

pub fn run(cli: Cli, c: Config) -> Result<(), String> {
    let root = if cli.root.as_os_str() != "." {
        cli.root
    } else {
        c.root
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    };

    match cli.command {
        Command::List { maintainer, json } => {
            let m = maintainer
                .or(c.maintainer)
                .ok_or("list needs --maintainer <name>")?;
            let candidates = packages::by_maintainer(&root, &m)?;
            if json {
                print_candidates_json(&candidates);
            } else {
                for candidate in candidates {
                    println!(
                        "{}\t{}\t{}",
                        candidate.attr_path,
                        candidate.backend.name(),
                        display_file(candidate.file.as_deref())
                    );
                }
            }
            Ok(())
        }

        Command::DryRun {
            package,
            maintainer,
            skip,
        } => {
            let pkg = package.or(c.package);
            let mnt = maintainer.or(c.maintainer);
            let mut skip = if skip.is_empty() {
                c.skip.unwrap_or_default()
            } else {
                skip
            };
            skip.extend(update::env_skip());

            if let Some(p) = pkg.as_deref() {
                update::dry_run_package(&root, p)
            } else if let Some(m) = mnt.as_deref() {
                let candidates: Vec<_> = packages::by_maintainer(&root, m)?
                    .into_iter()
                    .filter(|cand| !skip.contains(&cand.attr_path))
                    .collect();
                if candidates.is_empty() {
                    tracing::info!("No packages found for maintainer `{m}`.");
                }
                for candidate in candidates {
                    if candidate.backend.is_runnable() || candidate.backend.is_native_candidate() {
                        if let Err(err) = update::dry_run_package(&root, &candidate.attr_path) {
                            tracing::error!(package = %candidate.attr_path, "{err}");
                        }
                    } else {
                        tracing::debug!(
                            package = %candidate.attr_path,
                            backend = %candidate.backend.name(),
                            file = %display_file(candidate.file.as_deref()),
                            "{}",
                            candidate.backend.note()
                        );
                    }
                }
                Ok(())
            } else {
                Err("dry-run needs --package <attr> or --maintainer <name>".to_string())
            }
        }

        Command::RunUpdateScript { package } => update::run_update_script_direct(&root, &package),

        Command::Update {
            package,
            maintainer,
            commit,
            signed,
            push,
            pr,
            skip,
            no_build,
            signing_key,
            gpg_format,
            forge,
            forge_api_url,
        } => {
            let pkg = package.or(c.package);
            let mnt = maintainer.or(c.maintainer);
            let commit = commit || c.commit.unwrap_or(false);
            let signed = signed || c.signed.unwrap_or(false);
            let push = push || c.push.unwrap_or(false);
            let pr = pr || c.pr.unwrap_or(false);
            let skip = if skip.is_empty() {
                c.skip.unwrap_or_default()
            } else {
                skip
            };
            let no_build = if no_build.is_empty() {
                c.no_build.unwrap_or_default()
            } else {
                no_build
            };
            let signing_key = signing_key.or(c.signing_key);
            let gpg_format = gpg_format.or(c.gpg_format);
            let forge = forge.or(c.forge);
            let forge_api_url = forge_api_url.or(c.forge_api_url);

            // These imply each other; catch misuse instead of silently ignoring flags.
            if push && !commit {
                return Err("--push requires --commit".to_string());
            }
            if pr && !push {
                return Err("--pr requires --push".to_string());
            }

            if let Some(p) = pkg.as_deref() {
                update::update_package(
                    &root,
                    p,
                    update::CommitOptions {
                        commit,
                        signed,
                        push,
                        pr,
                        signing_key,
                        gpg_format,
                        forge: forge.unwrap_or_else(|| "auto".to_string()),
                        forge_api_url,
                        no_build,
                    },
                )
            } else if let Some(m) = mnt.as_deref() {
                update::update_maintainer(
                    &root,
                    m,
                    &update::CommitOptions {
                        commit,
                        signed,
                        push,
                        pr,
                        signing_key,
                        gpg_format,
                        forge: forge.unwrap_or_else(|| "auto".to_string()),
                        forge_api_url,
                        no_build,
                    },
                    &skip,
                )
            } else {
                Err("update needs --package <attr> or --maintainer <name>".to_string())
            }
        }
    }
}

fn display_file(file: Option<&std::path::Path>) -> String {
    file.map(|file| file.display().to_string())
        .unwrap_or_else(|| "evaluated".to_string())
}

fn print_candidates_json(candidates: &[packages::Candidate]) {
    println!("[");
    for (index, candidate) in candidates.iter().enumerate() {
        let comma = if index + 1 == candidates.len() {
            ""
        } else {
            ","
        };
        println!(
            "  {{\"attr_path\":\"{}\",\"backend\":\"{}\",\"file\":{}}}{comma}",
            json_escape(&candidate.attr_path),
            json_escape(candidate.backend.name()),
            candidate
                .file
                .as_deref()
                .map(|file| format!("\"{}\"", json_escape(&file.display().to_string())))
                .unwrap_or_else(|| "null".to_string())
        );
    }
    println!("]");
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

mod config {
    use std::{
        env,
        path::{Path, PathBuf},
    };

    #[derive(Debug, Default)]
    pub struct Config {
        pub root: Option<String>,
        pub maintainer: Option<String>,
        pub package: Option<String>,
        pub commit: Option<bool>,
        pub signed: Option<bool>,
        pub push: Option<bool>,
        pub pr: Option<bool>,
        pub skip: Option<Vec<String>>,
        pub signing_key: Option<String>,
        pub gpg_format: Option<String>,
        pub forge: Option<String>,
        pub forge_api_url: Option<String>,
        pub no_build: Option<Vec<String>>,
        pub verbose: Option<bool>,
    }

    pub fn load_first() -> Result<Option<Config>, String> {
        for path in search_paths()? {
            if path.exists() {
                return load_from_path(&path).map(Some);
            }
        }
        Ok(None)
    }

    fn search_paths() -> Result<Vec<PathBuf>, String> {
        let cwd = env::current_dir().map_err(|e| format!("current dir: {e}"))?;
        let mut paths = vec![cwd.join(".bumpkin.nix")];

        if let Some(xdg) = env::var_os("XDG_CONFIG_HOME") {
            paths.push(PathBuf::from(xdg).join("bumpkin").join("config.nix"));
        } else if let Some(home) = env::var_os("HOME") {
            paths.push(
                PathBuf::from(home)
                    .join(".config")
                    .join("bumpkin")
                    .join("config.nix"),
            );
        }

        paths.push(PathBuf::from("/var/bumpkin/config.nix"));
        Ok(paths)
    }

    fn load_from_path(path: &Path) -> Result<Config, String> {
        let dir = path
            .parent()
            .ok_or_else(|| format!("config path has no parent: {}", path.display()))?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("config path is not valid UTF-8: {}", path.display()))?;

        let expr = config_expr(file_name);

        let out = crate::nix::nix_cmd()
            .args(["eval", "--raw", "--impure", "--expr", &expr])
            .current_dir(dir)
            .output()
            .map_err(|e| format!("eval {}: {e}", path.display()))?;
        if !out.status.success() {
            return Err(format!(
                "eval {} exited with {}\n{}",
                path.display(),
                out.status,
                String::from_utf8_lossy(&out.stderr)
            ));
        }

        parse_config_output(
            &String::from_utf8(out.stdout).map_err(|e| format!("decode config: {e}"))?,
        )
    }

    fn config_expr(file_name: &str) -> String {
        format!(
            r#"
let
  cfg = import ./{file_name};
  string = name:
    if builtins.hasAttr name cfg then
      let value = cfg.${{name}}; in if value == null then "" else builtins.toString value
    else "";
  list = name:
    if builtins.hasAttr name cfg then
      let value = cfg.${{name}}; in if value == null then "" else builtins.concatStringsSep "," (map builtins.toString value)
    else "";
  bool = name:
    if builtins.hasAttr name cfg then
      let value = cfg.${{name}}; in if value == null then "" else if value then "1" else "0"
    else "";
  line = name: value: name + "\t" + value;
in builtins.concatStringsSep "\n" [
  (line "root" (string "root"))
  (line "maintainer" (string "maintainer"))
  (line "package" (string "package"))
  (line "commit" (bool "commit"))
  (line "signed" (bool "signed"))
  (line "push" (bool "push"))
  (line "pr" (bool "pr"))
  (line "signing_key" (string "signingKey"))
  (line "gpg_format" (string "gpgFormat"))
  (line "forge" (string "forge"))
  (line "forge_api_url" (string "forgeApiUrl"))
  (line "no_build" (list "noBuild"))
  (line "verbose" (bool "verbose"))
]
"#,
            file_name = escape_nix_path_segment(file_name)
        )
    }

    fn parse_config_output(stdout: &str) -> Result<Config, String> {
        let mut config = Config::default();
        for line in stdout.lines() {
            let Some((key, value)) = line.split_once('\t') else {
                continue;
            };
            if value.is_empty() {
                continue;
            }
            match key {
                "root" => config.root = Some(value.to_string()),
                "maintainer" => config.maintainer = Some(value.to_string()),
                "package" => config.package = Some(value.to_string()),
                "commit" => config.commit = Some(value == "1"),
                "signed" => config.signed = Some(value == "1"),
                "push" => config.push = Some(value == "1"),
                "pr" => config.pr = Some(value == "1"),
                "signing_key" => config.signing_key = Some(value.to_string()),
                "gpg_format" => config.gpg_format = Some(value.to_string()),
                "forge" => config.forge = Some(value.to_string()),
                "forge_api_url" => config.forge_api_url = Some(value.to_string()),
                "no_build" => {
                    config.no_build = Some(value.split(",").map(|s| s.to_string()).collect());
                }
                "verbose" => config.verbose = Some(value == "1"),
                _ => {}
            }
        }

        Ok(config)
    }

    fn escape_nix_path_segment(value: &str) -> String {
        value.replace('\\', "\\\\").replace('"', "\\\"")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_list_parses() {
        let cli = Cli::try_parse_from(["bumpkin", "list", "--maintainer", "74k1"]).unwrap();
        assert!(matches!(cli.command, Command::List { .. }));
    }

    #[test]
    fn cli_dry_run_package() {
        let cli = Cli::try_parse_from(["bumpkin", "dry-run", "-p", "foo"]).unwrap();
        assert!(matches!(cli.command, Command::DryRun { .. }));
    }

    #[test]
    fn cli_update_with_commit() {
        let cli = Cli::try_parse_from([
            "bumpkin", "update", "-m", "74k1", "--commit", "--signed", "--push", "--pr",
        ])
        .unwrap();
        assert!(matches!(cli.command, Command::Update { .. }));
    }

    #[test]
    fn cli_root_flag() {
        let cli = Cli::try_parse_from(["bumpkin", "-C", "/tmp/flake", "list", "-m", "x"]).unwrap();
        assert_eq!(cli.root, std::path::PathBuf::from("/tmp/flake"));
    }

    #[test]
    fn cli_verbose_global() {
        let cli = Cli::try_parse_from(["bumpkin", "-v", "list", "-m", "x"]).unwrap();
        assert!(cli.verbose);
    }
}
