mod git;
mod nix;
mod packages;
mod update;

use std::{env, path::PathBuf};

#[derive(Debug, Default)]
struct Options {
    root: PathBuf,
    package: Option<String>,
    maintainer: Option<String>,
    commit: bool,
    signed: bool,
    signing_key: Option<String>,
    gpg_format: Option<String>,
}

pub fn run() -> Result<(), String> {
    let args = env::args().collect::<Vec<_>>();
    let command = args.get(1).map(String::as_str).ok_or_else(usage)?;
    let opts = parse_options(&args[2..])?;

    match command {
        "dry-run" => {
            if let Some(package) = opts.package.as_deref() {
                update::dry_run_package(&opts.root, package)
            } else if let Some(maintainer) = opts.maintainer.as_deref() {
                let candidates = packages::by_maintainer(&opts.root, maintainer)?;
                if candidates.is_empty() {
                    println!("No packages found for maintainer `{maintainer}`.");
                }
                for candidate in candidates {
                    if candidate.has_update_script {
                        update::dry_run_package(&opts.root, &candidate.attr_path)?;
                    } else {
                        println!(
                            "\n==> {} ({})\nskip: no passthru.updateScript/updateScript\n",
                            candidate.attr_path,
                            candidate.file.display()
                        );
                    }
                }
                Ok(())
            } else {
                Err("dry-run needs --package <attr> or --maintainer <name>".to_string())
            }
        }
        "run-update-script" => {
            let package = opts.package.as_deref().ok_or("run-update-script needs --package <attr>")?;
            update::run_update_script_cmd(&[
                "--root".to_string(),
                opts.root.display().to_string(),
                "--package".to_string(),
                package.to_string(),
            ])
        }
        "update" => {
            let package = opts.package.as_deref().ok_or("update needs --package <attr>")?;
            update::update_package(&opts.root, package, update::CommitOptions {
                commit: opts.commit,
                signed: opts.signed,
                signing_key: opts.signing_key,
                gpg_format: opts.gpg_format,
            })
        }
        "list" => {
            let maintainer = opts.maintainer.as_deref().ok_or("list needs --maintainer <name>")?;
            for candidate in packages::by_maintainer(&opts.root, maintainer)? {
                let updater = if candidate.has_update_script { "update-script" } else { "no-update-script" };
                println!("{}\t{}\t{}", candidate.attr_path, updater, candidate.file.display());
            }
            Ok(())
        }
        other => Err(format!("unknown command: {other}\n\n{}", usage())),
    }
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut opts = Options {
        root: env::current_dir().map_err(|e| format!("current dir: {e}"))?,
        ..Options::default()
    };
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
                opts.maintainer = Some(args.get(i).ok_or("missing value for --maintainer")?.clone());
            }
            "--commit" => opts.commit = true,
            "--signed" => opts.signed = true,
            "--signing-key" => {
                i += 1;
                opts.signing_key = Some(args.get(i).ok_or("missing value for --signing-key")?.clone());
            }
            "--gpg-format" => {
                i += 1;
                opts.gpg_format = Some(args.get(i).ok_or("missing value for --gpg-format")?.clone());
            }
            "--help" | "-h" => return Err(usage()),
            other => return Err(format!("unknown argument: {other}")),
        }
        i += 1;
    }
    Ok(opts)
}

fn usage() -> String {
    "usage:\n  bumpkin list --maintainer <name> [--root <flake>]\n  bumpkin dry-run --package <attr> [--root <flake>]\n  bumpkin dry-run --maintainer <name> [--root <flake>]\n  bumpkin update --package <attr> [--root <flake>] [--commit] [--signed]".to_string()
}
