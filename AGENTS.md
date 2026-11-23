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
- Without `--commit`, `update --package` edits the checkout in place, builds and prints the PR text; nothing is committed or pushed.
- Signed commits should use Git's normal signing machinery (`git commit -S`) rather than custom crypto.
- Keep state in logs/PRs; avoid adding a database unless explicitly requested.
- `update --maintainer` without `--commit` is the dry run: temp worktrees, the checkout is not mutated.
- `--commit` always means the per-package branch flow (`<branchPrefix><package>`), for one package or a whole maintainer; `--push` and `--pr` build on it.

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
  update.rs      # Update flows (in place, temp worktree, per-package branch), PR text,
                 #   native updater (git ls-remote tags + Nix-computed hashes)
  nix.rs         # Everything that calls nix: maintainer query, package info, builds,
                 #   updateScript, version check, dependents, closure diff
  git.rs         # Git helpers: branch, commit, push, diff, remote_url
  forge.rs       # PR creation via gh CLI or curl against the GitHub / Gitea / Forgejo REST API
modules/nixos/
  bumpkin.nix    # NixOS module: systemd timers + oneshot services
```

## Dependencies

None. Keep it that way unless there is a very strong reason.

- CLI parsing is the `match` in `lib.rs::parse`; `--help` is the `HELP` const. Keep both in sync.
- Logging is the `info!` / `warn!` / `error!` / `debug!` macros in `lib.rs` (stderr);
  `-v` enables `debug!`. `println!` is for data output (`list`).
- Runtime tools: `nix`, `git`, `curl`, optionally `gh`.
- Every `nix` call references the checkout as `path:<root>`, which copies it. Keep the
  number of calls per package low: `nix::package_info` is the one evaluation for
  everything about a package (file, versions, output path, src URLs, meta links), taken
  once before and once after an update.

## NixOS module

One systemd timer + oneshot service per maintainer. Each service iterates over
`packageSets`, clones or resets each checkout to its upstream branch (under a lock), then
runs `bumpkin update --root <path> --maintainer <name>`. Options are documented in the
README table; `nix flake check` evaluates the module with a minimal config.

## Useful commands

```sh
nix develop -c cargo test --lib
nix develop -c cargo clippy --all-targets
nix develop -c cargo build --release
nix flake check                                                              # package + NixOS module eval
./target/release/bumpkin list --maintainer 74k1 --root $HOME/dev/tixpkgs
./target/release/bumpkin update --maintainer 74k1 --root $HOME/dev/tixpkgs -v    # dry run
./target/release/bumpkin update --package arcbrush --root $HOME/dev/tixpkgs      # in place
./target/release/bumpkin update --maintainer 74k1 --root $HOME/dev/tixpkgs --commit --signed --push --pr
```

## Behaviour worth knowing

Updating:
- A failing updateScript is an error; the native updater only runs when there is no updateScript.
- updateScripts get `UPDATE_NIX_ATTR_PATH` / `_NAME` / `_PNAME` / `_OLD_VERSION` like nixpkgs'
  `update.py`. List-style `nix-update` scripts get `--flake` injected when the repo has no
  root `default.nix`; legacyPackages-only packages get the full attr path.
- Bare-path updateScripts flattened into the read-only store are run from a byte-identical
  copy in the package directory, so scripts that write next to themselves work.
- Native updater limits: git-hosted, version-linked sources with numeric versions only.
  Anything else returns an `unsupported: ` error and counts as skipped in batch mode.
- Dependency hashes refreshed: `cargoHash`, `vendorHash`, `npmDepsHash`, `yarnHash`,
  `pomHash`, `mvnHash`, `mixHash`, `nugetHash`, `dotnetHash`.
- A numeric version downgrade after an update is an error.
- `--no-build` takes a list; without a value it skips all builds. Skipped packages still
  get updated, committed and PR'd. `--skip` / `BUMPKIN_SKIP` skip packages entirely.
- `--gc` runs `nix store gc` after each package, also after a failed one. It is store-wide
  and best-effort (a failure is a warning). It removes the old output, so the PR closure
  diff is mostly absent on later runs.

Committing and PRs:
- Commit mode needs a clean tree and a branch (no detached HEAD). It discards leftovers of
  a failed update (`checkout -f` + `git clean -fd`) and never resets the main branch.
- Leftover `<branchPrefix><package>` branches are force-deleted, hence the non-empty prefix.
- Branches are force-pushed unless the remote already has identical content; a create-PR
  response containing "already exists" counts as success.
- `auto` forge: `gh` for github.com, otherwise the REST API of the `origin` host.
- Signing options are passed as one-off `git -c`, never written to the repo config.
- PR body extras are best-effort and never block: compare link, `meta.changelog` /
  `meta.homepage`, dependents (via `nix-store --query --referrers-closure`; nothing for
  legacyPackages-only sets), closure diff (only if the old output is still in the store),
  and a version check that runs `bin/*` inside a Nix derivation (skipped unless the Nix
  sandbox is on, so upstream binaries never run unsandboxed).

Not supported: multi-platform builds, CVE checks, registry version lookups, parallel batch runs.
