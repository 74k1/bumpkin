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
     referencing `${version}`; pinned-rev packages are skipped.
3. Repology API (last resort — version hint only, never drives updates)

## Project layout

```
src/
  main.rs        # Tracing init, CLI parse entrypoint, error exit
  lib.rs         # Clap CLI structs (Commands, flags), config loading, command dispatch
  update.rs      # Update logic: dry-run, per-package update, batch maintainer update,
                 #   native updater (git ls-remote tags + Nix-computed hashes),
                 #   fake-hash refresh loop (src + cargoHash, vendorHash, npmDepsHash, etc.)
  packages.rs    # Package discovery, maintainer filtering, fetcher classification
  nix.rs         # Nix evaluation: package version, build, updateScript, maintainer query,
                 #   src info (gitRepoUrl/urls), flake input info (nixpkgs rev for PR bodies)
  git.rs         # Git helpers: branch, commit, push, diff, remote_url
  forge.rs       # Forge abstraction: PR create/find via gh CLI, GitHub REST API, Gitea/Forgejo REST API
  repology.rs    # Repology.org version oracle (free, no API key)
modules/nixos/
  bumpkin.nix    # NixOS module: systemd timers + oneshot services
```

## Dependencies

- **clap** (derive): CLI argument parsing, subcommand dispatch, auto-generated `--help`.
- **tracing** / **tracing-subscriber** (env-filter): structured logging.
  - Default level: `info` (per-package results, summary).
  - `--verbose` / `-v` enables `debug` (internal steps, diffs, "trying..." messages).
  - `RUST_LOG` env var overrides both (e.g., `RUST_LOG=warn` for quiet runs).
  - `println!` is retained for data output (JSON, tab-separated list); logs go to stderr.

## NixOS module

The module at `modules/nixos/bumpkin.nix` creates one systemd timer + oneshot service per
maintainer. Each service iterates over `packageSets`, clones or fast-forwards each repo,
then runs `bumpkin update --root <path> --maintainer <name>`.

Options exposed to NixOS:
- `services.bumpkin.enable`
- `services.bumpkin.maintainers` — list of maintainer handles
- `services.bumpkin.packageSets` — list of flake refs (string sugar) or attrsets with `repo`, `branch`, `path`, `forge`, `forgeApiUrl`, `noBuild`
- `services.bumpkin.actions.commit` / `.signed` / `.push` / `.pr` — batch commit/push/PR behaviour
- `services.bumpkin.forgeTokenFile` — path to forge PAT file (GitHub, Gitea, Forgejo)
- `services.bumpkin.gpgKeyFile` — path to ASCII-armored GPG key for sign+import
- `services.bumpkin.git.userName` / `.userEmail` / `.gpgFormat` / `.signingKey` / `.sshKeyFile` / `.extraConfig`
- `services.bumpkin.schedule` / `.randomizedDelaySec` — timer tuning
- `services.bumpkin.gc.enable` — periodic `nix store gc`
- `services.bumpkin.package` — the bumpkin derivation (auto-set by `nixosModules.default`)

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

- Maintainer scans default to evaluating `packages.$system.*.meta.maintainers`; source scanning is a fallback.
- `run-update-script` supports flake package outputs, checks `passthru.updateScript` plus top-level `updateScript`.
- `update --maintainer --commit` runs per-package branches sequentially; it does not parallelize across packages (Nix builds are single-instance anyway).
- `update --package` supports `--commit --push --pr` too (per-package branch flow, same as batch mode); `--push` requires `--commit`, `--pr` requires `--push`.
- Commit mode refuses to run from a detached HEAD (no branch to return to).
- Repology is a fallback version hint only; it does not drive the primary update flow.
- Registry-based sources (`fetchPypi`, `fetchCrate`, ...) and non-git VCSes still need updateScripts; the native updater handles git-hosted sources only.
- Non-numeric versions (rc/beta tags, `unstable-…` dates) are skipped by the native updater.
- Multi-platform builds are not supported; bumpkin builds only on the local system.
- No CVE checking or rebuild-impact estimation (unlike nixpkgs-update).
- Forge PR dedup covers `gh` CLI, GitHub REST API, and Gitea/Forgejo REST API. Gitea/Forgejo do not support the `head=` filter, so matching is done client-side on `head.ref`.
- Dependency hash refresh covers: `cargoHash`, `vendorHash`, `npmDepsHash`, `yarnHash`, `pomHash`, `mvnHash`, `mixHash`, `nugetHash`, `dotnetHash`; multiple hashes per package are resolved iteratively.
- `noBuild` (`--no-build`) is per-packageSet in the NixOS module, comma-separated at the CLI. Skipped packages still get version/hash updates, commits, and PRs; only the `nix build` step is omitted.
- `BUMPKIN_SKIP` (comma-separated env var) applies to both `dry-run` and `update`.
- `nix flake check` builds the package (incl. cargo tests) and evaluates the NixOS module with a minimal config (`checks.<system>.nixos-module`).
