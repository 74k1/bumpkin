use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub(crate) fn nix_cmd() -> Command {
    let mut cmd = Command::new("nix");
    cmd.args(["--extra-experimental-features", "nix-command flakes"]);
    cmd
}

#[derive(Debug)]
pub struct Package {
    pub attr_path: String,
    /// Package file inside the checkout (from `meta.position`), if known.
    pub file: Option<PathBuf>,
    pub has_update_script: bool,
}

/// Evaluated facts about one package, straight from Nix instead of scraping
/// the .nix file: where it is defined and where its source lives.
#[derive(Debug, Default)]
pub struct PackageInfo {
    pub file: Option<PathBuf>,
    pub name: String,
    pub pname: String,
    pub version: String,
    /// Output store path (it need not exist yet).
    pub out_path: String,
    pub changelog: String,
    pub homepage: String,
    /// `src.gitRepoUrl` (set by fetchFromGitHub and friends), if any.
    pub git_repo_url: String,
    /// `src.urls` (or `[src.url]`) of the source derivation.
    pub urls: Vec<String>,
}

fn installable(root: &Path, package: &str) -> String {
    format!("path:{}#{package}", root.display())
}

/// `path:` flake ref of a checkout, for `builtins.getFlake`.
fn flake_ref(root: &Path) -> Result<String, String> {
    if !root.join("flake.nix").exists() {
        return Err("expected a flake root containing flake.nix".to_string());
    }
    let root = root
        .canonicalize()
        .map_err(|e| format!("canonicalize {}: {e}", root.display()))?;
    Ok(escape_nix_string(&format!("path:{}", root.display())))
}

fn eval(expr: &str) -> Result<String, String> {
    let out = nix_cmd()
        .args(["eval", "--raw", "--impure", "--expr", expr])
        .output()
        .map_err(|e| format!("nix eval: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    String::from_utf8(out.stdout).map_err(|e| format!("decode nix eval output: {e}"))
}

/// Map a flake-relative `file:line` position to a file in the checkout.
fn checkout_file(root: &Path, position: &str) -> Option<PathBuf> {
    let file = root.join(position.rsplit_once(':')?.0);
    file.exists().then_some(file)
}

/// `nix store diff-closures` between two built outputs; "" if unavailable.
pub fn closure_diff(old: &str, new: &str) -> String {
    let out = nix_cmd().args(["store", "diff-closures", old, new]).output();
    let text = out.map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
    strip_ansi(&text.unwrap_or_default())
}

/// Drop `ESC [ ... m` colour sequences (Nix emits them even when piped).
fn strip_ansi(text: &str) -> String {
    let mut parts = text.split("\x1b[");
    let mut out = parts.next().unwrap_or_default().to_string();
    for part in parts {
        out.push_str(part.split_once('m').map_or(part, |(_, rest)| rest));
    }
    out
}

#[test]
fn strip_ansi_removes_colour_codes() {
    assert_eq!(strip_ansi("a: \x1b[31;1m1 KiB\x1b[0m\nb"), "a: 1 KiB\nb");
}

/// Other `packages.<system>` outputs whose build closure contains `package`,
/// i.e. that are rebuilt by its update. Empty for legacyPackages-only sets.
pub fn dependents(root: &Path, package: &str) -> Vec<String> {
    let drvs = flake_ref(root).and_then(|flake| eval(&drv_paths_expr(&flake))).unwrap_or_default();
    let drvs: Vec<(&str, &str)> = drvs.lines().filter_map(|line| line.split_once('\t')).collect();
    let Some((_, target)) = drvs.iter().find(|(name, _)| *name == package) else {
        return Vec::new();
    };
    let closure = Command::new("nix-store")
        .args(["--query", "--referrers-closure", target])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    drvs.iter()
        .filter(|(name, drv)| *name != package && closure.lines().any(|line| line == *drv))
        .map(|(name, _)| name.to_string())
        .collect()
}

fn drv_paths_expr(flake_ref: &str) -> String {
    format!(
        r#"
let
  flake = builtins.getFlake "{flake_ref}";
  {PRELUDE}
  line = name:
    let r = builtins.tryEval flakePackages.${{name}}.drvPath;
    in if r.success then name + "\t" + r.value else "";
in builtins.concatStringsSep "\n" (map line (builtins.attrNames flakePackages))
"#
    )
}

pub fn build_package(root: &Path, package: &str) -> Result<bool, String> {
    nix_cmd()
        .args(["build", "--no-link", &installable(root, package)])
        .status()
        .map(|s| s.success())
        .map_err(|e| format!("run nix build: {e}"))
}

/// Build and capture the full output (hash mismatches, PR body diagnostics).
pub fn build_package_with_log(root: &Path, package: &str) -> Result<(bool, String), String> {
    let out = nix_cmd()
        .args(["build", "--no-link", &installable(root, package)])
        .output()
        .map_err(|e| format!("run nix build: {e}"))?;
    let log = [out.stdout, out.stderr].concat();
    Ok((out.status.success(), String::from_utf8_lossy(&log).into_owned()))
}

/// Delete every unrooted store path. Builds use `--no-link`, so this includes
/// everything the run built or copied. Best-effort: a failure is only logged.
pub fn collect_garbage() {
    match nix_cmd().args(["store", "gc"]).output() {
        Ok(out) if out.status.success() => {
            debug!("gc: {}", String::from_utf8_lossy(&out.stderr).trim())
        }
        Ok(out) => warn!("nix store gc failed: {}", String::from_utf8_lossy(&out.stderr).trim()),
        Err(e) => warn!("run nix store gc: {e}"),
    }
}

/// Locked rev of the flake's `nixpkgs` input for PR bodies ("" if there is
/// none); evaluated once per run.
pub fn nixpkgs_rev(root: &Path) -> &'static str {
    static REV: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    REV.get_or_init(|| {
        let expr = |flake| format!(r#"(builtins.getFlake "{flake}").inputs.nixpkgs.rev or """#);
        flake_ref(root).and_then(|flake| eval(&expr(flake))).unwrap_or_default()
    })
}

pub fn package_info(root: &Path, package: &str) -> Result<PackageInfo, String> {
    let stdout = eval(&package_info_expr(&flake_ref(root)?, package))
        .map_err(|e| format!("eval package info failed:\n{e}"))?;
    let mut info = PackageInfo::default();
    for (key, value) in stdout.lines().filter_map(|line| line.split_once('\t')) {
        match key {
            "file" => info.file = checkout_file(root, value),
            "name" => info.name = value.to_string(),
            "pname" => info.pname = value.to_string(),
            "version" => info.version = value.to_string(),
            "out" => info.out_path = value.to_string(),
            "changelog" => info.changelog = value.to_string(),
            "homepage" => info.homepage = value.to_string(),
            "gitRepoUrl" => info.git_repo_url = value.to_string(),
            "url" if !value.is_empty() => info.urls.push(value.to_string()),
            _ => {}
        }
    }
    Ok(info)
}

/// Run the built package's binaries with `--version` / `-V` / `--help` and
/// return the first invocation (e.g. `foo --version`) that prints the package
/// version. This runs as a derivation, so inside the Nix sandbox: no network,
/// none of bumpkin's secrets.
pub fn version_check(root: &Path, package: &str) -> Option<String> {
    // Never run upstream binaries unless Nix actually sandboxes builds
    // (off by default on macOS, or with `sandbox = false`).
    let sandbox = nix_cmd().args(["config", "show", "sandbox"]).output().ok()?;
    if !matches!(String::from_utf8_lossy(&sandbox.stdout).trim(), "true" | "relaxed") {
        return None;
    }
    let expr = version_check_expr(&flake_ref(root).ok()?, package);
    let out = nix_cmd()
        .args(["build", "--no-link", "--print-out-paths", "--impure", "--timeout", "60"])
        .args(["--expr", &expr])
        .output()
        .ok()?;
    let hit = std::fs::read_to_string(String::from_utf8_lossy(&out.stdout).trim()).ok()?;
    Some(hit.trim().rsplit('/').next()?.to_string())
}

fn version_check_expr(flake_ref: &str, package: &str) -> String {
    format!(
        r#"
let
  flake = builtins.getFlake "{flake_ref}";
  {PRELUDE}
  package = flakePackages."{package}" or legacyPackages."{package}";
in derivation {{
  name = "bumpkin-version-check";
  inherit system;
  builder = "/bin/sh";
  args = [ "-c" ''
    for bin in ${{package}}/bin/*; do
      for flag in --version -V --help; do
        case "$("$bin" $flag 2>&1 </dev/null)" in
          *"${{package.version}}"*) echo "$bin $flag" > $out; exit 0 ;;
        esac
      done
    done
    exit 1
  '' ];
}}
"#,
        package = escape_nix_string(package)
    )
}

pub fn build_update_script(root: &Path, package: &str) -> Result<String, String> {
    let expr = update_script_expr(&flake_ref(root)?, package);
    let out = nix_cmd()
        .args(["build", "--no-link", "--print-out-paths", "--impure", "--expr", &expr])
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

/// Packages whose `meta.maintainers` contain `maintainer`, sorted by name.
pub fn packages_by_maintainer(root: &Path, maintainer: &str) -> Result<Vec<Package>, String> {
    let stdout = eval(&maintainer_query_expr(&flake_ref(root)?, maintainer))?;
    let mut packages = Vec::new();
    for line in stdout.lines() {
        let mut fields = line.splitn(3, '\t');
        let attr_path = fields.next().unwrap_or_default().to_string();
        let file = fields.next().and_then(|position| checkout_file(root, position));
        let has_update_script = fields.next() == Some("1");
        if !attr_path.is_empty() {
            packages.push(Package { attr_path, file, has_update_script });
        }
    }
    Ok(packages)
}

/// Shared bindings; expects `flake` in scope. Not passed through `format!`,
/// so braces are literal here.
const PRELUDE: &str = r#"
  system = builtins.currentSystem;
  flakePackages = flake.packages.${system} or {};
  legacyPackages = flake.legacyPackages.${system} or {};
  # `meta.position` relative to the flake root; "" when unknown or outside it.
  relPosition = package:
    let
      r = builtins.tryEval (package.meta.position or "");
      position = if r.success then r.value else "";
      prefix = builtins.toString flake.outPath + "/";
      n = builtins.stringLength prefix;
    in if builtins.substring 0 n position == prefix then builtins.substring n (-1) position else "";
"#;

fn package_info_expr(flake_ref: &str, package: &str) -> String {
    format!(
        r#"
let
  flake = builtins.getFlake "{flake_ref}";
  {PRELUDE}
  package = flakePackages."{package}" or legacyPackages."{package}" or (throw "package `{package}` not found");
  src = package.src or null;
  str = value: let r = builtins.tryEval (builtins.toString value); in if r.success then r.value else "";
  urls =
    let r = builtins.tryEval (
      if src == null then []
      else if src ? urls then map builtins.toString src.urls
      else if src ? url then [ (builtins.toString src.url) ]
      else []);
    in if r.success then r.value else [];
in builtins.concatStringsSep "\n" ([
  ("file\t" + relPosition package)
  ("name\t" + str (package.name or ""))
  ("pname\t" + str (package.pname or ""))
  ("version\t" + str (package.version or ""))
  ("out\t" + str (package.outPath or ""))
  ("changelog\t" + str (package.meta.changelog or ""))
  ("homepage\t" + str (package.meta.homepage or ""))
  ("gitRepoUrl\t" + (if src == null then "" else str (src.gitRepoUrl or "")))
] ++ map (u: "url\t" + u) urls)
"#,
        package = escape_nix_string(package)
    )
}

fn update_script_expr(flake_ref: &str, package: &str) -> String {
    format!(
        r#"
let
  flake = builtins.getFlake "{flake_ref}";
  {PRELUDE}
  inputNames = builtins.attrNames (flake.inputs or {{}});
  wrapperPkgs =
    if legacyPackages ? writeShellScriptBin then legacyPackages
    else let
      pkgsInputs = builtins.filter (name:
        let
          input = flake.inputs.${{name}} or {{}};
          probe = builtins.tryEval (builtins.hasAttr "writeShellScriptBin" (import input {{}}));
        in input ? outPath && probe.success && probe.value
      ) inputNames;
    in if pkgsInputs != [] then import flake.inputs.${{builtins.head pkgsInputs}} {{ inherit system; }}
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
  # nix-update-script defaults to `import ./. {{}}`, which only works for repos
  # with a default.nix (like nixpkgs). Flake-only repos need `--flake`, and
  # nix-update then resolves `packages.<system>.<attr>`, so legacy-only
  # packages get their full attr path.
  isNixUpdate = value: builtins.baseNameOf (scriptPart (builtins.head value)) == "nix-update";
  hasImportFlag = value: builtins.any (arg: builtins.elem arg [ "--flake" "-F" "--file" "-f" ]) (builtins.tail value);
  needsFlakeFlag = value:
    isNixUpdate value && !(hasImportFlag value) && !(builtins.pathExists (flake.outPath + "/default.nix"));
  withFlakeFlag = value:
    if needsFlakeFlag value then [ (builtins.head value) "--flake" ] ++ builtins.tail value else value;
  scriptPrelude = value:
    if needsFlakeFlag value && !(flakePackages ? "{package}") then
      "export UPDATE_NIX_ATTR_PATH=" + quote "legacyPackages.${{system}}.{package}" + "\n"
    else "";
  normalize = value:
    if builtins.isList value then
      if value == [] then throw "empty updateScript list"
      else if wrapperPkgs != null && wrapperPkgs ? writeShellScriptBin then
        wrapperPkgs.writeShellScriptBin "bumpkin-update-script" (scriptPrelude value + builtins.concatStringsSep " " (map (part: quote (scriptPart part)) (withFlakeFlag value)))
      else throw "updateScript is a list but no nixpkgs-like input found for writeShellScriptBin; add a nixpkgs input or legacyPackages"
    else value;
in normalize script
"#,
        package = escape_nix_string(package)
    )
}

fn maintainer_query_expr(flake_ref: &str, maintainer: &str) -> String {
    format!(
        r#"
let
  flake = builtins.getFlake "{flake_ref}";
  {PRELUDE}
  packages = legacyPackages // flakePackages;
  needle = "{maintainer}";
  altNeedle =
    if builtins.substring 0 1 needle == "_"
    then builtins.substring 1 (builtins.stringLength needle - 1) needle
    else "_" + needle;

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
    builtins.any (value: value == needle || value == altNeedle) (maintainerStrings maintainer);

  packageMatches = package:
    let result = builtins.tryEval (
      let
        meta = package.meta or {{}};
        maintainers = meta.maintainers or [];
        maintainerList = if builtins.isList maintainers then maintainers else [ maintainers ];
      in builtins.any maintainerMatches maintainerList
    );
    in result.success && result.value;

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
    in name + "\t" + relPosition package + "\t" + (if packageHasUpdateScript package then "1" else "0");
in builtins.concatStringsSep "\n" (map lineFor matchingNames)
"#,
        maintainer = escape_nix_string(maintainer)
    )
}

fn escape_nix_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
