# AGENTS.md

Guidance for coding agents working on Bumpkin.

## Agent behavior

These guidelines bias toward caution over speed; use judgment for trivial tasks.

- Think before coding: state assumptions, surface ambiguity and tradeoffs, ask when unclear, and push back on overcomplicated or risky requests.
- Keep solutions simple: implement only what was asked, avoid speculative abstractions/configuration, and rewrite if the change is larger than the problem warrants.
- Make surgical changes: touch only lines directly related to the request, match existing style, and do not clean up unrelated code. Remove only unused code that your change created.
- Work toward verifiable goals: for multi-step work, state a brief plan with checks; prefer tests or concrete commands that prove the change works.

## Project

Bumpkin is a Rust/Nix flake upkeep bot. It should work for arbitrary flake package sets, not only `NixOS/nixpkgs`. Do not preserve `nixpkgs-upkeep` behavior for its own sake; use it only as historical reference.

## Principles

- Keep Rust dependency count low.
- Do not add `unsafe` unless there is a very strong reason.
- Prefer package-owned `passthru.updateScript` / `updateScript` over bot-side upstream heuristics.
- Do not brute-force upstream version URLs.
- Default local CLI flow should update/build/print PR text without committing or opening PRs.
- Signed commits should use Git's normal signing machinery (`git commit -S`) rather than custom crypto.
- Keep state in logs/PRs; avoid adding a database unless explicitly requested.
- Dry-run commands must not mutate repositories.
- Batch `update --maintainer` supports `--commit` / `--signed` / `--push` (per-package branches). Without these flags it uses temp worktrees and does not mutate the checkout.

## Update priority order

1. package-owned `updateScript` / `passthru.updateScript`
2. Native updater, driven by Nix rather than per-forge clients:
   - `nix eval` the package `src` for `gitRepoUrl`/`urls` (no .nix text scraping
     for the source location)
   - new versions via `git ls-remote --tags` on the derived git URL (works on
     any git host: GitHub, GitLab, sourcehut, Codeberg, Gitea, Bitbucket, ...)
   - hashes via the fake-hash trick: write `sha256-AAAA...=` for src and known
     dependency hashes, run `nix build`, read the `got:` hashes from the
     mismatch errors, loop until none remain. Nix runs the fetcher, so every
     fetcher is supported without bot-side prefetch code.
   - requires a version-linked source: a `rev`/`tag`/`url` assignment
     referencing `${version}` (or bare `version`/`finalAttrs.version`);
     pinned-rev packages are skipped.
   - the package file comes from `meta.position` (no hardcoded `pkgs/` layout).
   - `rev`/`tag`/`url` and the src hash are looked up inside the
     `src = fetcher { ... }` attrset first (whole file as fallback), so
     `fetchpatch` URLs/hashes are not mistaken for the source.

## Project layout

```
src/
  main.rs        # Parse args, run, error exit
  lib.rs         # Log macros, hand-rolled CLI parser (Args, --help), command dispatch
  update.rs      # Update logic: per-package update, batch maintainer update, temp-worktree
                 #   dry run, native updater (git ls-remote tags + Nix-computed hashes),
                 #   fake-hash refresh loop (src + cargoHash, vendorHash, npmDepsHash, etc.)
  nix.rs         # Nix evaluation: maintainer query, package info (file via meta.position,
                 #   name/version, src gitRepoUrl/urls), build, updateScript,
                 #   flake input info (nixpkgs rev for PR bodies)
  git.rs         # Git helpers: branch, commit, push, diff, remote_url
  forge.rs       # PR creation via gh CLI or curl against the GitHub / Gitea / Forgejo REST API
modules/nixos/
  bumpkin.nix    # NixOS module: systemd timers + oneshot services
```

## Dependencies

None. Keep it that way unless there is a very strong reason.

- CLI parsing is the `match` in `lib.rs::parse`; `--help` is the `HELP` const. Keep both in sync.
- Logging is the `info!` / `warn!` / `error!` / `debug!` macros in `lib.rs` (stderr).
  `--verbose` / `-v` enables `debug!` (internal steps, diffs, "trying..." messages).
  `println!` is retained for data output (JSON, tab-separated list).
- Runtime tools: `nix`, `git`, `curl`, optionally `gh`.

## NixOS module

The module at `modules/nixos/bumpkin.nix` creates one systemd timer + oneshot service per
maintainer. Each service iterates over `packageSets`, clones or fast-forwards each repo,
then runs `bumpkin update --root <path> --maintainer <name>`.

Options exposed to NixOS:
- `services.bumpkin.enable`
- `services.bumpkin.maintainers` - list of maintainer handles
- `services.bumpkin.packageSets` - list of flake refs (string sugar) or attrsets with `repo`, `branch`, `path`, `forge`, `forgeApiUrl`, `noBuild`, `branchPrefix`, `gitUserName`, `gitUserEmail`
- `services.bumpkin.branchPrefix` - per-package branch prefix (default `bumpkin/`); per-packageSet `branchPrefix` overrides it
- `services.bumpkin.actions.commit` / `.signed` / `.push` / `.pr` - batch commit/push/PR behaviour
- `services.bumpkin.forgeTokenFile` - path to forge PAT file (GitHub, Gitea, Forgejo)
- `services.bumpkin.gpgKeyFile` - path to ASCII-armored GPG key for sign+import
- `services.bumpkin.git.userName` / `.userEmail` / `.gpgFormat` / `.signingKey` / `.sshKeyFile`
- `services.bumpkin.schedule` / `.randomizedDelaySec` - timer tuning
- `services.bumpkin.package` - the bumpkin derivation (auto-set by `nixosModules.default`)

## Useful commands

```sh
nix develop -c cargo check
nix develop -c cargo test --lib
nix develop -c cargo build --release
./target/release/bumpkin list --maintainer 74k1 --root $HOME/dev/tixpkgs
./target/release/bumpkin dry-run --maintainer 74k1 --root $HOME/dev/tixpkgs
./target/release/bumpkin dry-run --maintainer 74k1 --root $HOME/dev/tixpkgs --verbose  # debug details
./target/release/bumpkin run-update-script --package arcbrush --root $HOME/dev/tixpkgs
./target/release/bumpkin update --package arcbrush --root $HOME/dev/tixpkgs
./target/release/bumpkin update --package arcbrush --root $HOME/dev/tixpkgs --commit --signed

# Batch maintainer: dry-run (temp worktrees, no mutation)
./target/release/bumpkin update --root $HOME/dev/tixpkgs --maintainer 74k1

# Batch maintainer: per-package branches, commit, push
./target/release/bumpkin update --root $HOME/dev/tixpkgs --maintainer 74k1 --commit --signed --push

# Per-machine build blocklist (env var, comma-separated)
BUMPKIN_SKIP=waterfox,waterfox-unwrapped ./target/release/bumpkin update --maintainer 74k1 --root $HOME/dev/tixpkgs
./target/release/bumpkin update --maintainer 74k1 --root $HOME/dev/tixpkgs --commit --no-build waterfox,waterfox-unwrapped

# NixOS module test
nix flake check
```

## Current caveats

- Maintainer scans evaluate `packages.$system.*.meta.maintainers`; there is no source-scan fallback.
- `run-update-script` supports flake package outputs, checks `passthru.updateScript` plus top-level `updateScript`.
- Bare-path updateScripts flattened into the read-only store are run from a byte-identical
  copy in the checkout's package directory (probed write access, content match), so scripts
  that write next to themselves work.
- Per-package branches are `<branchPrefix><package>` (`--branch-prefix` / config `branchPrefix`, default `bumpkin/`). The prefix must be non-empty because leftover branches with that name are force-deleted at the start of each package.
- updateScripts get `UPDATE_NIX_ATTR_PATH` / `UPDATE_NIX_NAME` / `UPDATE_NIX_PNAME` /
  `UPDATE_NIX_OLD_VERSION` like nixpkgs' `update.py`. List-style `nix-update` scripts
  (`nix-update-script`) get `--flake` injected when the repo has no root `default.nix`
  and no `--flake`/`--file` arg is present; legacyPackages-only packages get
  `UPDATE_NIX_ATTR_PATH=legacyPackages.<system>.<name>` in that mode.
- A failing updateScript is reported as an error; the native updater is only used when no updateScript exists.
- Diffs are printed only with `--verbose` (git writes them straight to stdout).
- `update --maintainer --commit` runs per-package branches sequentially; it does not parallelize across packages (Nix builds are single-instance anyway).
- `update --package` supports `--commit --push --pr` too (per-package branch flow, same as batch mode); `--push` requires `--commit`, `--pr` requires `--push`.
- Commit mode refuses to run from a detached HEAD (no branch to return to).
- Registry-based sources (`fetchPypi`, `fetchCrate`, ...) and non-git VCSes still need updateScripts; the native updater handles git-hosted sources only.
- Non-numeric versions (rc/beta tags, `unstable-…` dates) are skipped by the native updater.
- Multi-platform builds are not supported; bumpkin builds only on the local system.
- No CVE checking or rebuild-impact estimation (unlike nixpkgs-update).
- PR bodies carry two best-effort extras: an upstream compare link (git URL + tag prefix from the package file; none for sourcehut or when no prefix is found) and a version check that runs the built `bin/*` with `--version` / `-V` / `--help` inside a Nix derivation (60s timeout); it is skipped unless `nix config show sandbox` is `true`/`relaxed`, so upstream binaries never run unsandboxed. Neither blocks the PR.
- No PR lookup: per-package branches are force-pushed (an open PR is updated in place) and a create-PR response containing "already exists" is treated as success. `auto` forge = `gh` for github.com, otherwise the REST API of the `origin` host (`https://<host>/api/v1` for non-GitHub).
- Commit mode discards leftovers of a failed update (`checkout -f` + `git clean -fd`); it never resets the main branch.
- Signing options (`--signing-key`, `--gpg-format`) are passed as one-off `git -c`, never written to the repo config.
- Packages the native updater cannot handle return an `unsupported: ` error and count as skipped in batch mode.
- Dependency hash refresh covers: `cargoHash`, `vendorHash`, `npmDepsHash`, `yarnHash`, `pomHash`, `mvnHash`, `mixHash`, `nugetHash`, `dotnetHash`; multiple hashes per package are resolved iteratively.
- `noBuild` (`--no-build`) is per-packageSet in the NixOS module, comma-separated at the CLI. A bare `--no-build` skips all builds. Skipped packages still get version/hash updates, commits, and PRs; only the `nix build` step is omitted.
- `BUMPKIN_SKIP` (comma-separated env var) applies to both `dry-run` and `update`.
- `nix flake check` builds the package (incl. cargo tests) and evaluates the NixOS module with a minimal config (`checks.<system>.nixos-module`).
