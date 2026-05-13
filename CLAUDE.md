# CLAUDE.md

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

- Maintainer scans default to evaluating `packages.$system.*.meta.maintainers`; source scanning is a fallback.
- `run-update-script` currently supports flake package outputs and checks `passthru.updateScript` plus top-level `updateScript`.
- The old PR creation path is still nixpkgs/GitHub-specific and should be replaced/generalized before real forge automation.
