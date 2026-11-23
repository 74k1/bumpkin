// Logs go to stderr; stdout is reserved for data (`list`).
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
  update             Update --package or all packages of --maintainer. Without
                     --commit: --package edits the checkout in place,
                     --maintainer is a dry run in temp worktrees

Options:
  -C, --root <PATH>          Root of the flake repository [default: .]
  -p, --package <ATTR>       Single package
  -m, --maintainer <NAME>    Maintainer handle
  -v, --verbose              Show internal steps and diffs
      --skip <A,B>           Packages to skip (also: BUMPKIN_SKIP env var)
      --no-build [<A,B>]     Packages to update without building (no value: all)
      --gc                   Collect Nix store garbage after each package
      --commit               Commit each update on its own branch
      --signed               Sign commits (git commit -S)
      --push                 Push per-package branches to origin (needs --commit)
      --pr                   Open pull requests (needs --push)
      --merge                Merge new PRs once checks succeed (needs --pr;
                             Gitea/Forgejo REST API only)
      --signing-key <KEY>    Git user.signingkey for the commit
      --gpg-format <FORMAT>  Git gpg.format for the commit (openpgp or ssh)
      --forge <FORGE>        auto, gh, or api [default: auto]
      --forge-api-url <URL>  REST API base URL (default: derived from origin)
      --branch-prefix <P>    Per-package branch prefix [default: bumpkin/]
  -h, --help                 Print help
  -V, --version              Print version";

#[derive(Debug, Default)]
pub struct Args {
    pub command: String,
    pub root: PathBuf,
    pub verbose: bool,
    pub package: Option<String>,
    pub maintainer: Option<String>,
    pub commit: bool,
    pub signed: bool,
    pub push: bool,
    pub pr: bool,
    pub merge: bool,
    pub skip: Vec<String>,
    /// Packages to update without building; `*` means all.
    pub no_build: Vec<String>,
    pub gc: bool,
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
            "--skip" => a.skip = list(value()?),
            "--no-build" => a.no_build = list(optional().unwrap_or("*".into())),
            "--gc" => a.gc = true,
            "--commit" => a.commit = true,
            "--signed" => a.signed = true,
            "--push" => a.push = true,
            "--pr" => a.pr = true,
            "--merge" => a.merge = true,
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
            for p in nix::packages_by_maintainer(&a.root, &maintainer)? {
                let backend = if p.has_update_script { "update-script" } else { "native" };
                let file = p.file.map_or("unknown".to_string(), |f| f.display().to_string());
                println!("{}\t{backend}\t{file}", p.attr_path);
            }
            Ok(())
        }

        "update" => {
            // These imply each other; catch misuse instead of silently ignoring flags.
            if a.push && !a.commit {
                return Err("--push requires --commit".to_string());
            }
            if a.pr && !a.push {
                return Err("--pr requires --push".to_string());
            }
            if a.merge && !a.pr {
                return Err("--merge requires --pr".to_string());
            }
            // Leftover branches are force-deleted before each run, so an empty
            // prefix could delete an unrelated branch named like a package.
            if a.branch_prefix.is_empty() {
                return Err("--branch-prefix must not be empty".to_string());
            }
            if let Some(package) = a.package.as_deref() {
                let result = update::update_package(&a.root, package, &a);
                if a.gc {
                    nix::collect_garbage();
                }
                result
            } else if let Some(maintainer) = a.maintainer.as_deref() {
                update::update_maintainer(&a.root, maintainer, &a)
            } else {
                Err("update needs --package <attr> or --maintainer <name>".to_string())
            }
        }

        "" => Err("missing command (see --help)".to_string()),
        other => Err(format!("unknown command `{other}` (see --help)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(args: &[&str]) -> Args {
        parse(args.iter().map(|s| s.to_string())).unwrap()
    }

    #[test]
    fn parses_commands_and_flags() {
        let a = parse_ok(&["-C", "/tmp/flake", "-v", "list", "-m", "x"]);
        assert_eq!((a.command.as_str(), a.verbose), ("list", true));
        assert_eq!(a.root, PathBuf::from("/tmp/flake"));
        assert_eq!(a.maintainer.as_deref(), Some("x"));

        let a = parse_ok(&["update", "-p", "foo", "--commit", "--signed", "--push", "--pr", "--merge"]);
        assert!(a.commit && a.signed && a.push && a.pr && a.merge);
        assert_eq!((a.forge.as_str(), a.branch_prefix.as_str()), ("auto", "bumpkin/"));
    }

    #[test]
    fn parses_values_lists_and_bare_no_build() {
        let a = parse_ok(&["update", "--branch-prefix=upkeep/", "--skip", "a,b", "--no-build", "c"]);
        assert_eq!(a.branch_prefix, "upkeep/");
        assert_eq!((a.skip, a.no_build), (vec!["a".to_string(), "b".into()], vec!["c".to_string()]));

        let a = parse_ok(&["update", "--no-build", "--commit", "--gc"]);
        assert_eq!(a.no_build, vec!["*".to_string()]);
        assert!(a.commit && a.gc);
    }

    #[test]
    fn rejects_unknown_and_missing_values() {
        let parse_err = |args: &[&str]| parse(args.iter().map(|s| s.to_string())).is_err();
        assert!(parse_err(&["update", "--nope"]));
        assert!(parse_err(&["update", "--package"]));
        assert!(parse_err(&["update", "extra"]));
    }
}
