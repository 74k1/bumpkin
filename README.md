# bumpkin

A small Rust upkeep bot for Nix flake package sets. It finds packages by maintainer, runs package-owned `passthru.updateScript`s, shows the resulting diff, builds the package, and can optionally create a signed commit.

The guiding idea: package-specific upstream knowledge belongs in the package expression, not in the bot.

## Requirements

Runtime tools expected on `PATH`:

- `nix` with flakes enabled
- `git`

Bumpkin builds package-owned update scripts through Nix and runs the resulting store executable. For scripts produced by helpers such as `writeShellApplication`, runtime inputs are provided by the script wrapper; they do not need to be installed globally. Ad-hoc scripts should still arrange their own dependencies through Nix rather than relying on the host environment.

Repository shape expected by the current implementation:

- a flake root with `flake.nix`
- package outputs at `packages.$system.<attr>`
- update scripts at `packages.$system.<attr>.passthru.updateScript` or `packages.$system.<attr>.updateScript`
- maintainer scans default to evaluating package `meta.maintainers` from the flake package set; source scanning under `pkgs/**/*.nix` is only a fallback

Bumpkin itself does not brute-force versions. The preferred path is still package-owned `passthru.updateScript`, but Bumpkin also recognizes the nixpkgs fetcher family documented in the nixpkgs manual.

Recognition and runnable update support are different:

- Bumpkin can classify many fetchers in `list` / maintainer scans.
- Runnable native updates currently exist for simple GitHub-backed sources only: `fetchFromGitHub`, plus `fetchurl`/`fetchzip` URLs that point at GitHub release assets.
- For those supported cases, Bumpkin checks GitHub tags, updates `version`/source hash, and can refresh `cargoHash`, `vendorHash`, or `npmDepsHash` by asking Nix for the expected hash.
- Other fetchers should either use `passthru.updateScript` for now or gain explicit native updater support later.

## Build

```sh
nix build
# or
nix develop -c cargo build
```

## Usage

Get help with:

```sh
bumpkin --help
bumpkin -h
bumpkin help
```

Most commands accept the flake root as the first positional argument after the command:

```sh
bumpkin update ./ --package arcbrush
bumpkin update ../meow --package arcbrush
```

You can still use `--root <flake>` or `-C <flake>` instead.

Bumpkin currently uses a tiny built-in argument parser instead of `clap` to keep Rust dependencies low.

### List packages for a maintainer

```sh
bumpkin list ../tixpkgs2 --maintainer 74k1
# --root still works too
bumpkin list --maintainer 74k1 --root ../tixpkgs2
# machine-readable output
bumpkin list ../tixpkgs2 --maintainer 74k1 --json
```

This evaluates `packages.$system` and filters packages by `meta.maintainers`, matching fields such as `github`, `githubId`, `name`, `email`, `handle`, and `matrix`. If evaluation is not available, Bumpkin falls back to a simple `pkgs/**/*.nix` source scan. It reports the detected backend, such as `update-script`, `fetchFromGitHub`, `github-release-asset`, `fetchurl`, `fetchFromGitLab`, `fetchPypi`, `fetchCrate`, `buildGoModule`, `rust/cargo`, or `npm`.

### Dry-run one package

```sh
bumpkin dry-run ../tixpkgs2 --package arcbrush
```

Dry-run creates a temporary git worktree, runs the package update script there, prints the diffstat and full diff, then removes the worktree. It does not modify your checkout.

### Dry-run packages for a maintainer

```sh
bumpkin dry-run ../tixpkgs2 --maintainer 74k1
```

Packages with update scripts are tried in temporary worktrees. Simple GitHub-backed native fetcher packages without update scripts are also tried via the native updater. Bumpkin then runs a package build and reports whether it succeeds.

### Batch update packages for a maintainer

```sh
bumpkin update ../tixpkgs2 --maintainer 74k1
```

This tries each runnable package in its own temporary git worktree and prints a final summary. It does not mutate your checkout and does not currently support `--commit`.

### Update one package, no commit/PR

```sh
bumpkin update ../tixpkgs2 --package arcbrush
```

This requires a clean working tree, then does the local CLI flow:

1. read current package version from the flake
2. build and run the package-owned `updateScript`
3. check whether the working tree changed
4. print suggested PR title/body for copy-paste
5. show a diffstat
6. run `nix build --no-link path:$root#$package`
7. leave all changes uncommitted

### Update and commit

```sh
bumpkin update ../tixpkgs2 --package arcbrush --commit
```

### Signed commits

```sh
bumpkin update ../tixpkgs2 --package arcbrush --commit --signed
```

This uses:

```sh
git commit -S
```

So signing is handled by your normal Git/GPG setup. If your GPG key is available through `gpg-agent`, configure Git normally, for example:

```sh
git config --global user.signingkey <key-id>
git config --global commit.gpgsign true
```

For SSH signing:

```sh
git config --global gpg.format ssh
git config --global user.signingkey ~/.ssh/signing_key.pub
```

You can also set signing options for one run:

```sh
bumpkin update ../tixpkgs2 --package arcbrush --commit --signed \
  --gpg-format ssh \
  --signing-key ~/.ssh/signing_key.pub
```

SSH authentication for pushing can still come from `gpg-agent`; commit signing is controlled by Git's signing config.

### Run just the update script

```sh
bumpkin run-update-script ../tixpkgs2 --package arcbrush
```

This builds and executes the package-owned update script. Bumpkin recognizes both:

```text
packages.$system.$package.passthru.updateScript
packages.$system.$package.updateScript
```

It also checks `legacyPackages.$system` and wraps simple list-valued update scripts as shell commands where possible. The script runs from the repository root, so package-local scripts can edit files and recalculate hashes.

## Configuration

Bumpkin uses the first config file found in this order:

1. `./.bumpkin.nix`
2. `$XDG_CONFIG_HOME/bumpkin/config.nix`, or `$HOME/.config/bumpkin/config.nix` when `XDG_CONFIG_HOME` is unset
3. `/var/bumpkin/config.nix`

CLI flags override config values.

```nix
{
  root = "../tixpkgs";
  maintainer = "74k1";
  package = "arcbrush";
  commit = false;
  signed = false;
  signingKey = null;
  gpgFormat = null;
}
```

All fields are optional.

## Fetcher support status

### Recognized/classified

Bumpkin can update packages using any fetcher when the package provides a runnable `passthru.updateScript` or `updateScript`. For packages without an update script, Bumpkin tries to recognize common nixpkgs fetchers and dependency helpers, including:

- `fetchurl`, `fetchzip`, `fetchpatch`
- `fetchgit`, `fetchGit`
- `fetchFromGitHub`, `fetchFromGitLab`, `fetchFromGitea`, `fetchFromForgejo`
- `fetchFromSourcehut`, `fetchFromBitbucket`, `fetchFromCodeberg`
- `fetchFromGitiles`, `fetchFromRepoOrCz`, `fetchFromSavannah`, `fetchFromRadicle`
- `fetchcvs`, `fetchsvn`, `fetchhg`, `fetchfossil`, `fetchbzr`
- `fetchPypi`, `fetchCrate`, `fetchFirefoxAddon`, `fetchNuGet`, `fetchHex`
- npm/yarn/pnpm/maven/mix/rebar helpers
- `dockerTools.pullImage`
- `builtins.fetchTree`, `builtins.fetchTarball`, `fetchClosure`

Recognition means Bumpkin can report the backend. A package-owned update script remains the generic path for safely updating every fetcher family; native Rust updates are intentionally limited to fetchers where Bumpkin can infer both the latest version source and the replacement hash without guessing URLs.

### Runnable native updates today

Currently implemented:

- simple `fetchFromGitHub`
- simple GitHub release asset `fetchurl`
- simple GitHub release asset `fetchzip`
- dependency hash refresh for `cargoHash`, `vendorHash`, and `npmDepsHash`

These paths still require the package to be straightforward: a parseable `version`, GitHub owner/repo or release URL, and a version tag scheme Bumpkin can infer.

### Not generally runnable yet

Not yet generally updateable without a package-owned `passthru.updateScript`:

- arbitrary `fetchurl` / `fetchzip`
- `fetchFromGitLab`, `fetchFromGitea`, `fetchFromForgejo`, etc.
- `fetchgit`, `fetchhg`, `fetchsvn`, `fetchfossil`
- `fetchPypi`, `fetchCrate`, `fetchFirefoxAddon`
- Docker image fetchers
- patch fetchers
- Maven/Mix/Rebar/Yarn/Pnpm dependency helpers
- multi-source packages
- packages where the new version requires coordinated dependency/package changes

For these, add `passthru.updateScript` or implement a dedicated native updater.

## Package-side updater shape

```nix
passthru.updateScript = writeShellApplication {
  name = "update-example";
  runtimeInputs = [ curl jq nix ];
  text = ''
    set -euo pipefail
    latest="..."
    hash="$(nix store prefetch-file --json "https://example.org/foo-$latest.tar.gz" | jq -r .hash)"
    # edit package file here
  '';
};
```

## Running modes

Bumpkin is intended to support three modes:

- local CLI: make changes, build, print copy-paste PR text, no commit by default
- scheduled job/cron/systemd timer: same updater flow, optionally committing/pushing
- GitHub Actions or other CI: run the same CLI commands in a checkout

Forge/PR creation should be built as a configurable layer on top of this flow, not hardcoded to nixpkgs.

## CI setup

### 1. Bot identity

Configure a bot identity in CI:

```sh
git config --global user.name "$BOT_USER"
git config --global user.email "$BOT_EMAIL"
```

Example values:

```text
BOT_USER=bumpkin-bot
BOT_EMAIL=bumpkin-bot@example.org
```

### 2. Checkout with push access

For GitHub Actions, store a token with write access as `GH_TOKEN`, then checkout the package set with it:

```yaml
- uses: actions/checkout@v4
  with:
    repository: 74k1/tixpkgs
    path: package-set
    token: ${{ secrets.GH_TOKEN }}
```

### 3. Install Nix

```yaml
- uses: cachix/install-nix-action@v31
```

### 4. Build Bumpkin

```yaml
- uses: actions/checkout@v4
  with:
    repository: 74k1/bumpkin
    path: bumpkin

- name: Build bumpkin
  working-directory: bumpkin
  run: nix build
```

### 5. Optional signed commits

For SSH commit signing, generate a dedicated key:

```sh
ssh-keygen -t ed25519 -f bumpkin_signing_key -C "bumpkin signing"
```

Add the public key to the bot account as a signing key if your forge supports it.

Store these CI secrets:

```text
BUMPKIN_SIGNING_KEY      # private key
BUMPKIN_SIGNING_KEY_PUB  # public key
```

Configure Git in CI:

```sh
mkdir -p ~/.ssh
printf '%s\n' "$BUMPKIN_SIGNING_KEY" > ~/.ssh/bumpkin_signing_key
printf '%s\n' "$BUMPKIN_SIGNING_KEY_PUB" > ~/.ssh/bumpkin_signing_key.pub
chmod 600 ~/.ssh/bumpkin_signing_key

git config --global gpg.format ssh
git config --global user.signingkey ~/.ssh/bumpkin_signing_key.pub
git config --global commit.gpgsign true
```

Then run Bumpkin with:

```sh
./bumpkin/result/bin/bumpkin update \
  --root package-set \
  --package arcbrush \
  --commit \
  --signed
```

If you use GPG commit signing instead, configure Git/GPG normally and still use `--commit --signed`; Bumpkin calls `git commit -S` and lets Git handle signing.

### 6. Dry-run or update

Dry-run packages for a maintainer:

```sh
./bumpkin/result/bin/bumpkin dry-run \
  --root package-set \
  --maintainer 74k1
```

Update one package without committing:

```sh
./bumpkin/result/bin/bumpkin update \
  --root package-set \
  --package arcbrush
```

Update one package and create a signed commit:

```sh
./bumpkin/result/bin/bumpkin update \
  --root package-set \
  --package arcbrush \
  --commit \
  --signed
```

### 7. Push/open PR

Generalized forge PR creation is not implemented yet. For now, CI can push a branch and use your forge CLI/API to open a PR with Bumpkin's printed title/body:

```sh
cd package-set
git switch -c bumpkin/arcbrush
git push origin bumpkin/arcbrush
```
