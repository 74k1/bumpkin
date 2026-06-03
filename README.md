# bumpkin

Rust upkeep bot for Nix flake package sets. Finds packages by maintainer, runs update scripts (or native fetcher updaters), builds, commits, pushes, opens PRs.

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

# Batch maintainer: per-package branches, commit, push, PR
bumpkin update --maintainer 74k1 --root $HOME/dev/tixpkgs --commit --signed --push --pr

# Per-machine blocklist
BUMPKIN_SKIP=waterfox,waterfox-unwrapped bumpkin dry-run --maintainer 74k1 --root $HOME/dev/tixpkgs
```

**Update priority:** package-owned `updateScript` → native fetcher (GitHub/GitLab/sourcehut/fetchgit) → Repology (version hint only).

**Forge backends:** `auto` (gh CLI if available, else GitHub REST API), `github-cli`, `github-api`, `api` (Gitea/Forgejo REST API).

**Dependency hash refresh:** `cargoHash`, `vendorHash`, `npmDepsHash`, `yarnHash`, `pomHash`, `mvnHash`, `mixHash`, `nugetHash`, `dotnetHash`.

## NixOS module

Two module entry points:

- `nixosModules.bumpkin` — raw module, requires explicit `services.bumpkin.package`
- `nixosModules.default` — wraps the raw module and auto-sets `package` to `self.packages.${system}.default`

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
| `actions.commit` | bool | `false` | Create per-package commits |
| `actions.signed` | bool | `false` | GPG/SSH sign commits |
| `actions.push` | bool | `false` | Push branches to origin |
| `actions.pr` | bool | `false` | Open pull requests |
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

- `forgeTokenFile` — forge personal access token. Used for forge API calls (PR
  creation) and optionally for HTTPS git transport when `git.sshKeyFile` is not
  set. Works with GitHub, Gitea, and Forgejo.
- `git.sshKeyFile` — SSH private key for git transport (clone, fetch, push).
  Takes priority over `forgeTokenFile` for git auth. The `forgeTokenFile` is
  still used for forge API calls.
- `gpgKeyFile` — ASCII-armored GPG private key imported before each run.

### Inspecting

```sh
systemctl status bumpkin-74k1.service
journalctl -u bumpkin-74k1 -f
systemctl list-timers 'bumpkin-*'
```

## Requirements

Runtime: `nix` (with flakes), `git`, `jq`. Optional: `gh` (GitHub CLI), `gnupg` (GPG signing), `openssh` (SSH push).

Repository shape: flake root with `flake.nix`, packages at `packages.$system.<attr>` or `legacyPackages.$system.<attr>`.
