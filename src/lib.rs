pub(crate) mod forge;
pub(crate) mod repology;
mod git;
mod nix;
pub(crate) mod packages;
mod update;

use std::{env, path::PathBuf};

#[derive(Clone, Debug, Default)]
struct Options {
    root: PathBuf,
    package: Option<String>,
    maintainer: Option<String>,
    commit: bool,
    signed: bool,
    push: bool,
    pr: bool,
    skip: Vec<String>,
    signing_key: Option<String>,
    gpg_format: Option<String>,
    json: bool,
    forge: Option<String>,
    forge_api_url: Option<String>,
}

pub fn run() -> Result<(), String> {
    let args = env::args().collect::<Vec<_>>();
    let Some(command) = args.get(1).map(String::as_str) else {
        println!("{}", usage());
        return Ok(());
    };
    if matches!(command, "--help" | "-h" | "help") {
        println!("{}", usage());
        return Ok(());
    }

    let opts = options_with_config(&args[2..])?;

    match command {
        "dry-run" => {
            if let Some(package) = opts.package.as_deref() {
                update::dry_run_package(&opts.root, package)
            } else if let Some(maintainer) = opts.maintainer.as_deref() {
                let candidates: Vec<_> = packages::by_maintainer(&opts.root, maintainer)?
                    .into_iter()
                    .filter(|c| !opts.skip.iter().any(|s| c.attr_path == *s))
                    .collect();
                if candidates.is_empty() {
                    println!("No packages found for maintainer `{maintainer}`.");
                }
                for candidate in candidates {
                    if candidate.backend.is_runnable() || candidate.backend.is_native_candidate() {
                        if let Err(err) = update::dry_run_package(&opts.root, &candidate.attr_path)
                        {
                            println!(
                                "\n==> {} ({})\nfailed: {err}\n",
                                candidate.attr_path,
                                display_file(candidate.file.as_deref())
                            );
                        }
                    } else {
                        println!(
                            "\n==> {} ({})\nbackend: {}\n{}\n",
                            candidate.attr_path,
                            display_file(candidate.file.as_deref()),
                            candidate.backend.name(),
                            candidate.backend.note()
                        );
                    }
                }
                Ok(())
            } else {
                Err("dry-run needs --package <attr> or --maintainer <name>".to_string())
            }
        }
        "run-update-script" => {
            let package = opts
                .package
                .as_deref()
                .ok_or("run-update-script needs --package <attr>")?;
            update::run_update_script_cmd(&[
                "--root".to_string(),
                opts.root.display().to_string(),
                "--package".to_string(),
                package.to_string(),
            ])
        }
        "update" => {
            if let Some(package) = opts.package.as_deref() {
                update::update_package(
                    &opts.root,
                    package,
                    update::CommitOptions {
                        commit: opts.commit,
                        signed: opts.signed,
                        push: false,
                        pr: false,
                        signing_key: opts.signing_key,
                        gpg_format: opts.gpg_format,
                        forge: opts.forge.clone().unwrap_or_else(|| "auto".to_string()),
                        forge_api_url: opts.forge_api_url.clone(),
                    },
                )
            } else if let Some(maintainer) = opts.maintainer.as_deref() {
                update::update_maintainer(
                    &opts.root,
                    maintainer,
                    &update::CommitOptions {
                        commit: opts.commit,
                        signed: opts.signed,
                        push: opts.push,
                        pr: opts.pr,
                        signing_key: opts.signing_key.clone(),
                        gpg_format: opts.gpg_format.clone(),
                        forge: opts.forge.clone().unwrap_or_else(|| "auto".to_string()),
                        forge_api_url: opts.forge_api_url.clone(),
                    },
                    &opts.skip,
                )
            } else {
                Err("update needs --package <attr> or --maintainer <name>".to_string())
            }
        }
        "list" => {
            let maintainer = opts
                .maintainer
                .as_deref()
                .ok_or("list needs --maintainer <name>")?;
            let candidates = packages::by_maintainer(&opts.root, maintainer)?;
            if opts.json {
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
        other => Err(format!("unknown command: {other}\n\n{}", usage())),
    }
}

fn options_with_config(args: &[String]) -> Result<Options, String> {
    let cwd = env::current_dir().map_err(|e| format!("current dir: {e}"))?;
    let mut base = Options {
        root: cwd,
        ..Options::default()
    };

    if let Some(config) = config::load_first()? {
        apply_config(&mut base, config);
    }

    parse_options(args, base)
}

fn apply_config(opts: &mut Options, config: config::Config) {
    if let Some(root) = config.root {
        opts.root = PathBuf::from(root);
    }
    if let Some(package) = config.package {
        opts.package = Some(package);
    }
    if let Some(maintainer) = config.maintainer {
        opts.maintainer = Some(maintainer);
    }
    if let Some(commit) = config.commit {
        opts.commit = commit;
    }
    if let Some(push) = config.push {
        opts.push = push;
    }
    if let Some(pr) = config.pr {
        opts.pr = pr;
    }
    if let Some(skip) = config.skip {
        opts.skip = skip;
    }
    if let Some(signed) = config.signed {
        opts.signed = signed;
    }
    if let Some(signing_key) = config.signing_key {
        opts.signing_key = Some(signing_key);
    }
    if let Some(gpg_format) = config.gpg_format {
        opts.gpg_format = Some(gpg_format);
    }
    if let Some(forge) = config.forge {
        opts.forge = Some(forge);
    }
    if let Some(forge_api_url) = config.forge_api_url {
        opts.forge_api_url = Some(forge_api_url);
    }
}

fn parse_options(args: &[String], mut opts: Options) -> Result<Options, String> {
    let mut positional_root = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" | "-C" => {
                i += 1;
                opts.root = PathBuf::from(args.get(i).ok_or("missing value for --root")?);
            }
            "--package" | "-p" => {
                i += 1;
                opts.package = Some(args.get(i).ok_or("missing value for --package")?.clone());
            }
            "--maintainer" | "-m" => {
                i += 1;
                opts.maintainer =
                    Some(args.get(i).ok_or("missing value for --maintainer")?.clone());
            }
            "--commit" => opts.commit = true,
            "--signed" => opts.signed = true,
            "--push" => opts.push = true,
            "--pr" => opts.pr = true,
            "--skip" => {
                i += 1;
                opts.skip = args
                    .get(i)
                    .ok_or("missing value for --skip")?
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            "--json" => opts.json = true,
            "--signing-key" => {
                i += 1;
                opts.signing_key = Some(
                    args.get(i)
                        .ok_or("missing value for --signing-key")?
                        .clone(),
                );
            }
            "--gpg-format" => {
                i += 1;
                opts.gpg_format =
                    Some(args.get(i).ok_or("missing value for --gpg-format")?.clone());
            }
            "--forge" => {
                i += 1;
                opts.forge = Some(args.get(i).ok_or("missing value for --forge")?.clone());
            }
            "--forge-api-url" => {
                i += 1;
                opts.forge_api_url =
                    Some(args.get(i).ok_or("missing value for --forge-api-url")?.clone());
            }
            "--help" | "-h" => return Err(usage()),
            other if other.starts_with('-') => return Err(format!("unknown argument: {other}")),
            root => {
                if positional_root {
                    return Err(format!("unexpected extra positional argument: {root}"));
                }
                opts.root = PathBuf::from(root);
                positional_root = true;
            }
        }
        i += 1;
    }
    Ok(opts)
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

fn usage() -> String {
    "usage:\n  bumpkin list [root] --maintainer <name> [--json]\n  bumpkin dry-run [root] --package <attr>\n  bumpkin dry-run [root] --maintainer <name> [--skip pkg1,pkg2]\n  bumpkin run-update-script [root] --package <attr>\n  bumpkin update [root] --package <attr> [--commit] [--signed]\n  bumpkin update [root] --maintainer <name> [--commit] [--signed] [--push] [--pr] [--skip pkg1,pkg2]\n\n[root] can also be passed as --root <flake> or -C <flake>.".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn parse_positional_root() {
        let opts = parse_options(
            &strings(&["../pkgs", "--package", "arcbrush"]),
            Options::default(),
        )
        .unwrap();
        assert_eq!(opts.root, PathBuf::from("../pkgs"));
        assert_eq!(opts.package.as_deref(), Some("arcbrush"));
    }

    #[test]
    fn parse_rejects_multiple_positional_roots() {
        let err = parse_options(&strings(&["one", "two"]), Options::default()).unwrap_err();
        assert!(err.contains("unexpected extra positional argument"));
    }
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
                _ => {}
            }
        }

        Ok(config)
    }

    fn escape_nix_path_segment(value: &str) -> String {
        value.replace('\\', "\\\\").replace('"', "\\\"")
    }
}
