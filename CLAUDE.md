# CLAUDE.md

Guidance for coding agents working on Bumpkin.

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

## Useful commands

```sh
nix develop -c cargo check
nix build
bumpkin dry-run --maintainer 74k1 --root ../tixpkgs2
bumpkin run-update-script --package arcbrush --root ../tixpkgs2
bumpkin update --package arcbrush --root ../tixpkgs2
bumpkin update --package arcbrush --root ../tixpkgs2 --commit --signed
```

## Current caveats

- `dry-run` is intentionally source-first: it searches package files by maintainer before evaluation.
- `run-update-script` currently supports flake package outputs.
- The old PR creation path is still nixpkgs/GitHub-specific and should be replaced/generalized before real forge automation.
