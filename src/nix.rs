use std::{path::Path, process::Command};

use crate::git;

#[derive(Debug)]
pub struct EvaluatedPackage {
    pub attr_path: String,
    pub position: Option<String>,
    pub has_update_script: bool,
}

pub fn package_version(root: &Path, package: &str) -> Result<String, String> {
    let out = Command::new("nix")
        .args([
            "eval",
            "--raw",
            &format!("path:{}#{package}.version", root.display()),
        ])
        .output()
        .map_err(|e| format!("eval package version: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    String::from_utf8(out.stdout)
        .map(|s| s.trim().to_string())
        .map_err(|e| format!("decode package version: {e}"))
}

pub fn build_package(root: &Path, package: &str) -> Result<bool, String> {
    git::status(Command::new("nix").args([
        "build",
        "--no-link",
        &format!("path:{}#{package}", root.display()),
    ]))
}

pub fn build_update_script(root: &Path, package: &str) -> Result<String, String> {
    if !root.join("flake.nix").exists() {
        return Err("expected a flake root containing flake.nix".to_string());
    }

    let root = root
        .canonicalize()
        .map_err(|e| format!("canonicalize {}: {e}", root.display()))?;
    let expr = update_script_expr(&format!("path:{}", root.display()), package);
    let out = Command::new("nix")
        .args([
            "build",
            "--no-link",
            "--print-out-paths",
            "--impure",
            "--expr",
            &expr,
        ])
        .current_dir(&root)
        .output()
        .map_err(|e| format!("build updateScript: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "build updateScript exited with {}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn packages_by_maintainer(
    root: &Path,
    maintainer: &str,
) -> Result<Vec<EvaluatedPackage>, String> {
    if !root.join("flake.nix").exists() {
        return Err("expected a flake root containing flake.nix".to_string());
    }

    let root = root
        .canonicalize()
        .map_err(|e| format!("canonicalize {}: {e}", root.display()))?;
    let expr = maintainer_query_expr(&format!("path:{}", root.display()), maintainer);
    let out = Command::new("nix")
        .args(["eval", "--raw", "--impure", "--expr", &expr])
        .current_dir(&root)
        .output()
        .map_err(|e| format!("eval maintainer query: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }

    let stdout =
        String::from_utf8(out.stdout).map_err(|e| format!("decode maintainer query: {e}"))?;
    let mut packages = Vec::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.splitn(3, '\t');
        let attr_path = fields.next().unwrap_or_default().to_string();
        let position = fields
            .next()
            .filter(|position| !position.is_empty())
            .map(ToOwned::to_owned);
        let has_update_script = fields.next() == Some("1");
        if !attr_path.is_empty() {
            packages.push(EvaluatedPackage {
                attr_path,
                position,
                has_update_script,
            });
        }
    }
    Ok(packages)
}

fn update_script_expr(flake_ref: &str, package: &str) -> String {
    format!(
        r#"
let
  flake = builtins.getFlake "{flake_ref}";
  system = builtins.currentSystem;
  flakePackages = flake.packages.${{system}} or {{}};
  legacyPackages = flake.legacyPackages.${{system}} or {{}};
  wrapperPkgs =
    if legacyPackages ? writeShellScriptBin then legacyPackages
    else if flake.inputs ? nixpkgs then import flake.inputs.nixpkgs {{ inherit system; }}
    else null;
  package = flakePackages."{package}" or legacyPackages."{package}" or (throw "package `{package}` not found");
  script =
    if (package ? passthru) && (package.passthru ? updateScript) then package.passthru.updateScript
    else if package ? updateScript then package.updateScript
    else throw "package `{package}` has no updateScript";
  quote = value: "'" + builtins.replaceStrings [ "'" ] [ "'\\''" ] value + "'";
  scriptPart = value:
    if builtins.isString value then value
    else if builtins.isPath value then builtins.toString value
    else if builtins.isAttrs value && value ? outPath then builtins.toString value
    else throw "unsupported updateScript list element";
  normalize = value:
    if builtins.isList value then
      if value == [] then throw "empty updateScript list"
      else if wrapperPkgs != null && wrapperPkgs ? writeShellScriptBin then
        wrapperPkgs.writeShellScriptBin "bumpkin-update-script" (builtins.concatStringsSep " " (map (part: quote (scriptPart part)) value))
      else builtins.head value
    else value;
in normalize script
"#,
        flake_ref = escape_nix_string(flake_ref),
        package = escape_nix_string(package)
    )
}

fn maintainer_query_expr(flake_ref: &str, maintainer: &str) -> String {
    format!(
        r#"
let
  flake = builtins.getFlake "{flake_ref}";
  system = builtins.currentSystem;
  flakePackages = flake.packages.${{system}} or {{}};
  legacyPackages = flake.legacyPackages.${{system}} or {{}};
  packages = legacyPackages // flakePackages;
  needle = "{maintainer}";

  valueStrings = value:
    if value == null then []
    else if builtins.isString value then [ value ]
    else [ (builtins.toString value) ];

  maintainerStrings = maintainer:
    if builtins.isString maintainer then [ maintainer ]
    else if builtins.isAttrs maintainer then
      valueStrings (maintainer.github or null)
      ++ valueStrings (maintainer.githubId or null)
      ++ valueStrings (maintainer.name or null)
      ++ valueStrings (maintainer.email or null)
      ++ valueStrings (maintainer.handle or null)
      ++ valueStrings (maintainer.matrix or null)
    else [];

  maintainerMatches = maintainer:
    builtins.any (value: value == needle) (maintainerStrings maintainer);

  packageMatches = package:
    let result = builtins.tryEval (
      let
        meta = package.meta or {{}};
        maintainers = meta.maintainers or [];
        maintainerList = if builtins.isList maintainers then maintainers else [ maintainers ];
      in builtins.any maintainerMatches maintainerList
    );
    in result.success && result.value;

  packagePosition = package:
    let result = builtins.tryEval (package.meta.position or "");
    in if result.success then result.value else "";

  packageHasUpdateScript = package:
    let result = builtins.tryEval (
      ((package ? passthru) && (package.passthru ? updateScript))
      || (package ? updateScript)
    );
    in result.success && result.value;

  names = builtins.attrNames packages;
  matchingNames = builtins.filter (name: packageMatches packages.${{name}}) names;
  lineFor = name:
    let package = packages.${{name}};
    in name + "\t" + packagePosition package + "\t" + (if packageHasUpdateScript package then "1" else "0");
in builtins.concatStringsSep "\n" (map lineFor matchingNames)
"#,
        flake_ref = escape_nix_string(flake_ref),
        maintainer = escape_nix_string(maintainer)
    )
}

fn escape_nix_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
