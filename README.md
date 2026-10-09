<img align="left" src="/.github/assets/bumpkin.png" width="256px" />

<br/>
<br/>

<div align="right">
    <h3><a href="http://bumpkin.urbanup.com/586439">Bumpkin</a> 🌽</h3>
    Bump your nix flake package sets.
</div>

<br/>
<br/>
<br/>
<br/>

## About

> [!NOTE]
> I hope you can tell by the AGENTS.md file that this is a vibecoded project and is mainly intended to be used by myself for my own repositories. Should this not be a warning to you, feel free to try to run it as well.
> 
> I'm still learning rust and I needed something quick to do the updating of packages in my own repository (just [tixpkgs](https://github.com/74k1/tixpkgs)).
> I can promise, that once I've learned rust on a level where I can take a big look at this codebase, I'll rewrite it by hand. (The logo for example, is already handmade. Thanks to a friend for the Idea.)

Bumpkin finds packages by maintainer. For each package it runs the package update script, or the native updater. It then builds the package and can commit, push, and open a pull request.

[Here](https://github.com/74k1/tix/blob/main/modules/nixos/daemons/bumpkin/default.nix)'s an example of how I set it up with the NixOS Module.

## Build

```sh
nix build
# or
nix develop -c cargo build --release
```

## CLI

```sh
# Discover packages
bumpkin list --maintainer 74k1 --root $HOME/dev/tixpkgs

# Dry-run (temp worktree, no mutation)
bumpkin dry-run --package arcbrush --root $HOME/dev/tixpkgs
bumpkin dry-run --maintainer 74k1 --root $HOME/dev/tixpkgs

# Update one package (local, no commit)
bumpkin update --package arcbrush --root $HOME/dev/tixpkgs

# Update one package: dedicated branch, commit, push, PR
bumpkin update --package arcbrush --root $HOME/dev/tixpkgs --commit --signed --push --pr

# Batch maintainer: per-package branches, commit, push, PR
bumpkin update --maintainer 74k1 --root $HOME/dev/tixpkgs --commit --signed --push --pr

# Per-machine blocklist (works for dry-run and update)
BUMPKIN_SKIP=waterfox,waterfox-unwrapped bumpkin dry-run --maintainer 74k1 --root $HOME/dev/tixpkgs
bumpkin update --maintainer 74k1 --root $HOME/dev/tixpkgs --commit --no-build waterfox,waterfox-unwrapped
```

**Update priority:** package-owned `updateScript` → native updater → Repology (version hint only).

**updateScripts:** Nix builds a bare-path script (for example `./update.sh`) into a
read-only file in the Nix store. A script that writes next to itself then fails. If the
package directory in the checkout contains a file with identical content, bumpkin runs
that copy instead. The checkout directory is writable.

**Native updater:** evaluates the package's `src` with Nix to find the upstream
git URL, discovers new versions via `git ls-remote --tags` (works on any git
host: GitHub, GitLab, sourcehut, Codeberg, Gitea, ...), then writes fake
src/dependency hashes and lets `nix build` report the real ones. Since Nix runs
the fetcher itself, every fetcher is supported as long as the source is
git-hosted and version-linked (`rev`/`tag`/`url` referencing `${version}`
or bare `rev = version;`).  GitHub repository **transfers** are detected
automatically (via 301 redirect) and the `owner`/`repo` fields are
updated in-place before any update runs.

**Forge backends:** `auto` (the `gh` CLI if it is installed, otherwise the GitHub REST API), `github-cli`, `github-api`, `api` (Gitea/Forgejo REST API).

**Dependency hash refresh:** `cargoHash`, `vendorHash`, `npmDepsHash`, `yarnHash`, `pomHash`, `mvnHash`, `mixHash`, `nugetHash`, `dotnetHash`.

## CI

[`examples/github-actions.yaml`](examples/github-actions.yaml) is a workflow you can copy. It works with GitHub Actions and Forgejo Actions. Copy it to `.github/workflows/bumpkin.yaml` inside your package set repository (not here).

### Secrets

| Secret | Required | Description |
|---|---|---|
| `GH_TOKEN` | yes | PAT with `contents:write` and `pull-requests:write` scope |

### How it works

The workflow runs daily at 04:00 UTC. You can also start it manually with an optional `maintainer` or `package` input to override the default target.

The runner starts bumpkin with `nix run github:74k1/bumpkin`, so no local build is necessary. Packages are updated with `--no-build`: bumpkin refreshes the hashes and commits and pushes the changes as pull requests. It does not run `nix build` on the runner. This keeps the job fast and does not stall on packages that build for a long time.

Before you commit the workflow to your repository, replace `your-handle` in the run step with your nixpkgs maintainer handle.

## NixOS module

Two module entry points:

- `nixosModules.bumpkin` - raw module, requires explicit `services.bumpkin.package`
- `nixosModules.default` - wraps the raw module and auto-sets `package` to `self.packages.${system}.default`

Use `default` unless you need a custom bumpkin derivation:

```nix
{
  imports = [
    inputs.bumpkin.nixosModules.default
  ];

  services.bumpkin = {
    enable = true;
    maintainers = [ "74k1" ];

    packageSets = [
      "github:74k1/tixpkgs"
      { repo = "github:74k1/tixpkgs"; noBuild = [ "waterfox" "waterfox-unwrapped" ]; }
      { repo = "https://git.example.com/org/pkgs.git"; forge = "api"; forgeApiUrl = "https://git.example.com/api/v1"; }
    ];

    actions = {
      commit = true;
      signed = true;
      push = true;
      pr = true;
    };

    forgeTokenFile = "/run/secrets/bumpkin-forge-token";
    gpgKeyFile = "/run/secrets/bumpkin-gpg-key";

    git = {
      userName = "bumpkin-bot";
      userEmail = "bumpkin@example.com";
      gpgFormat = "openpgp";
      signingKey = "7B2C...";
    };

    schedule = "daily";
    gc.enable = true;
  };
}
```

### Options

| Option | Type | Default | Description |
|---|---|---|---|
| `enable` | bool | `false` | |
| `maintainers` | list of str | `[]` | Maintainer handles to update |
| `packageSets` | list of str or attrset | `[]` | Flake refs or git URLs |
| `packageSets.*.repo` | str | (required) | Flake ref (`github:owner/repo`) or git URL |
| `packageSets.*.branch` | null or str | `null` | Branch to track (null = auto-discover) |
| `packageSets.*.path` | null or str | `null` | Checkout path (default: `/var/lib/bumpkin/<owner>/<repo>`) |
| `packageSets.*.forge` | null or str | `null` | Forge backend override (null = auto-detect) |
| `packageSets.*.forgeApiUrl` | null or str | `null` | API URL for `api` forge |
| `packageSets.*.noBuild` | list of str | `[]` | Package attr names to skip building (still update/commit/PR) |
| `actions.commit` | bool | `false` | Create per-package commits |
| `actions.signed` | bool | `false` | GPG/SSH sign commits |
| `actions.push` | bool | `false` | Push branches to origin |
| `actions.pr` | bool | `false` | Open pull requests |
| `skip` | list of str | `[]` | Package attr names to skip entirely |
| `forgeTokenFile` | null or str | `null` | Path to forge PAT file (GitHub, Gitea, Forgejo) |
| `gpgKeyFile` | null or str | `null` | Path to ASCII-armored GPG key |
| `git.userName` | null or str | `null` | Git author name |
| `git.userEmail` | null or str | `null` | Git author email |
| `git.gpgFormat` | null or str | `null` | `"openpgp"` or `"ssh"` |
| `git.signingKey` | null or str | `null` | GPG key fingerprint or SSH pubkey path |
| `git.sshKeyFile` | null or str | `null` | SSH private key for git transport |
| `git.extraConfig` | attrs | `{}` | Additional git config |
| `schedule` | str | `"daily"` | Systemd calendar event |
| `randomizedDelaySec` | int | `3600` | Max random delay before run |
| `gc.enable` | bool | `false` | Periodic Nix store GC |
| `gc.schedule` | str | `"weekly"` | GC calendar event |

### Auth

- `forgeTokenFile` - forge personal access token. Bumpkin uses it for forge
  API calls (PR creation) and for HTTPS git transport when `git.sshKeyFile`
  is not set. It works with GitHub, Gitea, and Forgejo. The token is supplied
  through a git credential helper and a curl stdin config. It therefore never
  appears in remote URLs, `.git/config`, or process command lines.
- `git.sshKeyFile` - SSH private key for git transport (clone, fetch, push).
  It takes priority over `forgeTokenFile` for git auth. Bumpkin still uses
  `forgeTokenFile` for forge API calls.
- `gpgKeyFile` - ASCII-armored GPG private key. Bumpkin imports it before
  each run.

### Inspecting

```sh
systemctl status bumpkin-74k1.service
journalctl -u bumpkin-74k1 -f
systemctl list-timers 'bumpkin-*'
```

## Requirements

Runtime: `nix` (with flakes), `git`, `jq`, `curl`. Optional: `gh` (GitHub CLI), `gnupg` (GPG signing), `openssh` (SSH push).

Repository shape: flake root with `flake.nix`, packages at `packages.$system.<attr>` or `legacyPackages.$system.<attr>`.
