// Logs go to stderr; stdout is reserved for data (`list`, `list --json`).
macro_rules! log {
    ($level:literal, $($arg:tt)*) => { eprintln!("{:>5} {}", $level, format!($($arg)*)) };
}
macro_rules! info { ($($arg:tt)*) => { log!("INFO", $($arg)*) }; }
macro_rules! warn { ($($arg:tt)*) => { log!("WARN", $($arg)*) }; }
macro_rules! error { ($($arg:tt)*) => { log!("ERROR", $($arg)*) }; }
macro_rules! debug { ($($arg:tt)*) => { if crate::verbose() { log!("DEBUG", $($arg)*) } }; }

mod forge;
mod git;
mod nix;
mod update;

use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

static VERBOSE: AtomicBool = AtomicBool::new(false);

pub(crate) fn verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

const HELP: &str = "\
Small Rust updater/PR steward for Nix flake package sets

Usage: bumpkin [OPTIONS] <COMMAND>

Commands:
  list               List packages maintained by --maintainer
  dry-run            Update + build in a temp worktree; the checkout is not touched
  run-update-script  Run the updater for --package and exit
  update             Update --package (in place) or all packages of --maintainer

Options:
  -C, --root <PATH>          Root of the flake repository [default: .]
  -p, --package <ATTR>       Single package
  -m, --maintainer <NAME>    Maintainer handle
  -v, --verbose              Show internal steps and diffs
      --json                 list: output JSON
      --skip <A,B>           Packages to skip (also: BUMPKIN_SKIP env var)
      --no-build [<A,B>]     Packages to update without building (no value: all)
      --commit               Commit (batch mode: on per-package branches)
      --signed               Sign commits (git commit -S)
      --push                 Push per-package branches to origin (needs --commit)
      --pr                   Open pull requests (needs --push)
      --signing-key <KEY>    Git user.signingkey for the commit
      --gpg-format <FORMAT>  Git gpg.format for the commit (openpgp or ssh)
      --forge <FORGE>        auto, github-cli, github-api, or api [default: auto]
      --forge-api-url <URL>  REST API base URL (default: derived from origin)
      --branch-prefix <P>    Per-package branch prefix [default: bumpkin/]
  -h, --help                 Print help
  -V, --version              Print version";

#[derive(Debug, Default)]
pub struct Args {
    pub command: String,
    pub root: PathBuf,
    pub verbose: bool,
    pub json: bool,
    pub package: Option<String>,
    pub maintainer: Option<String>,
    pub commit: bool,
    pub signed: bool,
    pub push: bool,
    pub pr: bool,
    pub skip: Vec<String>,
    /// Packages to update without building; `*` means all.
    pub no_build: Vec<String>,
    pub signing_key: Option<String>,
    pub gpg_format: Option<String>,
    pub forge: String,
    pub forge_api_url: Option<String>,
    /// Per-package branches are named `{branch_prefix}{package}`.
    pub branch_prefix: String,
}

pub fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let list = |v: String| v.split(',').map(str::to_string).collect::<Vec<_>>();
    let mut a = Args {
        root: ".".into(),
        forge: "auto".into(),
        branch_prefix: "bumpkin/".into(),
        ..Default::default()
    };
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut optional = || inline.clone().or_else(|| args.next_if(|v| !v.starts_with('-')));
        let mut value = || optional().ok_or(format!("{flag} needs a value"));
        match flag {
            "-C" | "--root" => a.root = value()?.into(),
            "-p" | "--package" => a.package = Some(value()?),
            "-m" | "--maintainer" => a.maintainer = Some(value()?),
            "-v" | "--verbose" => a.verbose = true,
            "--json" => a.json = true,
            "--skip" => a.skip = list(value()?),
            "--no-build" => a.no_build = list(optional().unwrap_or("*".into())),
            "--commit" => a.commit = true,
            "--signed" => a.signed = true,
            "--push" => a.push = true,
            "--pr" => a.pr = true,
            "--signing-key" => a.signing_key = Some(value()?),
            "--gpg-format" => a.gpg_format = Some(value()?),
            "--forge" => a.forge = value()?,
            "--forge-api-url" => a.forge_api_url = Some(value()?),
            "--branch-prefix" => a.branch_prefix = value()?,
            "-h" | "--help" => {
                println!("{HELP}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("bumpkin {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            command if !command.starts_with('-') && a.command.is_empty() => {
                a.command = command.to_string()
            }
            other => return Err(format!("unexpected argument `{other}` (see --help)")),
        }
    }
    Ok(a)
}

pub fn run(mut a: Args) -> Result<(), String> {
    VERBOSE.store(a.verbose, Ordering::Relaxed);
    a.skip.extend(update::env_skip());

    match a.command.as_str() {
        "list" => {
            let maintainer = a.maintainer.ok_or("list needs --maintainer <name>")?;
            let packages = nix::packages_by_maintainer(&a.root, &maintainer)?;
            let rows: Vec<_> = packages
                .iter()
                .map(|p| {
                    let backend = if p.has_update_script { "update-script" } else { "native" };
                    (&p.attr_path, backend, p.file.as_ref().map(|f| f.display().to_string()))
                })
                .collect();
            if a.json {
                let objects: Vec<_> = rows
                    .iter()
                    .map(|(attr, backend, file)| {
                        let file = file
                            .as_deref()
                            .map_or("null".to_string(), |f| format!("\"{}\"", json_escape(f)));
                        format!(
                            "  {{\"attr_path\":\"{}\",\"backend\":\"{backend}\",\"file\":{file}}}",
                            json_escape(attr)
                        )
                    })
                    .collect();
                println!("[\n{}\n]", objects.join(",\n"));
            } else {
                for (attr, backend, file) in rows {
                    println!("{attr}\t{backend}\t{}", file.as_deref().unwrap_or("unknown"));
                }
            }
            Ok(())
        }

        "run-update-script" => {
            let package = a.package.ok_or("run-update-script needs --package <attr>")?;
            update::run_update_script(&a.root, &package)
        }

        "dry-run" | "update" => {
            // These imply each other; catch misuse instead of silently ignoring flags.
            if a.push && !a.commit {
                return Err("--push requires --commit".to_string());
            }
            if a.pr && !a.push {
                return Err("--pr requires --push".to_string());
            }
            // Leftover branches are force-deleted before each run, so an empty
            // prefix could delete an unrelated branch named like a package.
            if a.branch_prefix.is_empty() {
                return Err("--branch-prefix must not be empty".to_string());
            }
            let dry = a.command == "dry-run";
            if dry && a.commit {
                return Err("dry-run does not take --commit".to_string());
            }

            if let Some(package) = a.package.as_deref() {
                if dry {
                    update::dry_run_package(&a.root, package)
                } else {
                    update::update_package(&a.root, package, &a)
                }
            } else if let Some(maintainer) = a.maintainer.as_deref() {
                // Without --commit this runs in temp worktrees, i.e. a dry run.
                update::update_maintainer(&a.root, maintainer, &a)
            } else {
                Err(format!("{} needs --package <attr> or --maintainer <name>", a.command))
            }
        }

        "" => Err("missing command (see --help)".to_string()),
        other => Err(format!("unknown command `{other}` (see --help)")),
    }
}

pub(crate) fn json_escape(value: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(args: &[&str]) -> Args {
        parse(args.iter().map(|s| s.to_string())).unwrap()
    }

    #[test]
    fn parses_commands_and_flags() {
        let a = parse_ok(&["-C", "/tmp/flake", "-v", "list", "-m", "x", "--json"]);
        assert_eq!((a.command.as_str(), a.verbose, a.json), ("list", true, true));
        assert_eq!(a.root, PathBuf::from("/tmp/flake"));
        assert_eq!(a.maintainer.as_deref(), Some("x"));

        let a = parse_ok(&["update", "-p", "foo", "--commit", "--signed", "--push", "--pr"]);
        assert!(a.commit && a.signed && a.push && a.pr);
        assert_eq!((a.forge.as_str(), a.branch_prefix.as_str()), ("auto", "bumpkin/"));
    }

    #[test]
    fn parses_values_lists_and_bare_no_build() {
        let a = parse_ok(&["update", "--branch-prefix=upkeep/", "--skip", "a,b", "--no-build", "c"]);
        assert_eq!(a.branch_prefix, "upkeep/");
        assert_eq!((a.skip, a.no_build), (vec!["a".to_string(), "b".into()], vec!["c".to_string()]));

        let a = parse_ok(&["update", "--no-build", "--commit"]);
        assert_eq!(a.no_build, vec!["*".to_string()]);
        assert!(a.commit);
    }

    #[test]
    fn rejects_unknown_and_missing_values() {
        let parse_err = |args: &[&str]| parse(args.iter().map(|s| s.to_string())).is_err();
        assert!(parse_err(&["update", "--nope"]));
        assert!(parse_err(&["update", "--package"]));
        assert!(parse_err(&["update", "extra"]));
    }
}
