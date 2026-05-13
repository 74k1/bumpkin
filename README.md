# bumpkin

A small Rust upkeep bot for Nix flake package sets. It finds packages by maintainer, runs package-owned `passthru.updateScript`s, shows the resulting diff, builds the package, and can optionally create a signed commit.

The guiding idea: package-specific upstream knowledge belongs in the package expression, not in the bot.

## Requirements

Runtime tools expected on `PATH`:

- `nix` with flakes enabled
- `git`
- whatever each package's `passthru.updateScript` declares in its own `runtimeInputs`

Repository shape expected by the current implementation:

- a flake root with `flake.nix`
- package outputs at `packages.$system.<attr>`
- update scripts at `packages.$system.<attr>.passthru.updateScript`
- for maintainer scans, nix package files under `pkgs/**/*.nix`

Bumpkin itself does not scrape upstreams or brute-force versions. If a package can be updated automatically, put that logic in `passthru.updateScript`.

## Build

```sh
nix build
# or
nix develop -c cargo build
```

## Usage

### List packages for a maintainer

```sh
bumpkin list --maintainer 74k1 --root ../tixpkgs2
```

This is source-first and intentionally simple: it scans `pkgs/**/*.nix` for the maintainer string and reports whether the file appears to define an update script.

### Dry-run one package

```sh
bumpkin dry-run --package arcbrush --root ../tixpkgs2
```

Dry-run creates a temporary git worktree, runs the package update script there, prints the diffstat and full diff, then removes the worktree. It does not modify your checkout.

### Dry-run packages for a maintainer

```sh
bumpkin dry-run --maintainer 74k1 --root ../tixpkgs2
```

Packages with update scripts are tried in temporary worktrees. Packages without update scripts are skipped with a short message.

### Update one package, no commit/PR

```sh
bumpkin update --package arcbrush --root ../tixpkgs2
```

This does the local CLI flow:

1. read current package version from the flake
2. build and run `.#packages.$system.$package.passthru.updateScript`
3. check whether the working tree changed
4. print suggested PR title/body for copy-paste
5. show a diffstat
6. run `nix build --no-link path:$root#$package`
7. leave all changes uncommitted

### Update and commit

```sh
bumpkin update --package arcbrush --root ../tixpkgs2 --commit
```

### Signed commits

```sh
bumpkin update --package arcbrush --root ../tixpkgs2 --commit --signed
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
bumpkin update --package arcbrush --root ../tixpkgs2 --commit --signed \
  --gpg-format ssh \
  --signing-key ~/.ssh/signing_key.pub
```

SSH authentication for pushing can still come from `gpg-agent`; commit signing is controlled by Git's signing config.

### Run just the update script

```sh
bumpkin run-update-script --package arcbrush --root ../tixpkgs2
```

This builds and executes:

```text
.#packages.$system.$package.passthru.updateScript
```

The script runs from the repository root, so package-local scripts can edit files and recalculate hashes.

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
