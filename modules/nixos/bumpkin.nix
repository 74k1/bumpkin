{ config, lib, pkgs, ... }:

let
  cfg = config.services.bumpkin;
  inherit (lib) types mkOption mkEnableOption mkIf;

  # Git clone URL for a flake ref (`github:owner/repo[/branch]`, ...) or a raw git URL.
  cloneUrl = ref:
    let
      m = builtins.match "(github|gitlab|sourcehut):([^/]+)/([^/]+)(/.*)?" ref;
      hosts = {
        github = "https://github.com/";
        gitlab = "https://gitlab.com/";
        sourcehut = "https://git.sr.ht/~";
      };
    in
      if m == null then lib.removePrefix "git+" ref
      else "${hosts.${builtins.elemAt m 0}}${builtins.elemAt m 1}/${builtins.elemAt m 2}";

  defaultPath = ps:
    let m = builtins.match ".*[:/]~?([^/:]+)/([^/]+)" (lib.removeSuffix ".git" (cloneUrl ps.repo));
    in if m == null then throw "cannot extract owner/repo from ref: ${ps.repo}"
       else "/var/lib/bumpkin/${builtins.elemAt m 0}/${builtins.elemAt m 1}";

  gitConfig = pkgs.writeText "bumpkin-gitconfig" (lib.generators.toGitINI {
    user = lib.filterAttrs (_: v: v != null) {
      name = cfg.git.userName;
      email = cfg.git.userEmail;
      signingkey = cfg.git.signingKey;
    };
    gpg = lib.optionalAttrs (cfg.git.gpgFormat != null) { format = cfg.git.gpgFormat; };
  });

  bumpkinCmd = "${cfg.package}/bin/bumpkin";

  # Git credential helper that reads the token from the environment, so the
  # token never lands in remote URLs (.git/config) or on a command line.
  credentialHelper = pkgs.writeShellScript "bumpkin-git-credential" ''
    [ "$1" = "get" ] || exit 0
    [ -n "''${GITHUB_TOKEN:-}" ] || exit 0
    echo "username=''${BUMPKIN_GIT_USER:-bumpkin}"
    echo "password=$GITHUB_TOKEN"
  '';

  serviceUnit = maintainer: {
    description = "bumpkin update run for maintainer ${maintainer}";
    after = [ "network-online.target" ];
    wants = [ "network-online.target" ];

    path = [ cfg.package pkgs.git pkgs.nix pkgs.curl pkgs.gh pkgs.util-linux ]
      ++ lib.optional (cfg.git.gpgFormat == "openpgp" || cfg.gpgKeyFile != null) pkgs.gnupg
      ++ lib.optional (cfg.git.sshKeyFile != null) pkgs.openssh;

    serviceConfig = {
      Type = "oneshot";
      User = cfg.user;
      Group = cfg.group;
      StateDirectory = "bumpkin";
      RemainAfterExit = false;
      PrivateTmp = true;
    };

    environment = {
      HOME = "/var/lib/bumpkin";
      NIX_PATH = "nixpkgs=${pkgs.path}";
      # For updateScripts that call nix themselves; scoped to this service.
      NIX_CONFIG = "extra-experimental-features = nix-command flakes";
      GIT_CONFIG_GLOBAL = gitConfig;
    } // lib.optionalAttrs (cfg.git.userName != null) {
      GIT_AUTHOR_NAME = cfg.git.userName;
      GIT_AUTHOR_EMAIL = cfg.git.userEmail;
      GIT_COMMITTER_NAME = cfg.git.userName;
      GIT_COMMITTER_EMAIL = cfg.git.userEmail;
      BUMPKIN_GIT_USER = cfg.git.userName;
    } // lib.optionalAttrs (cfg.git.sshKeyFile != null) {
      GIT_SSH_COMMAND = "ssh -i ${cfg.git.sshKeyFile} -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new";
    };

    script =
      let
        pkgSetBlock = ps:
          let
            checkout = if ps.path != null then ps.path else defaultPath ps;
            args = [
              "--maintainer" maintainer
              "--branch-prefix" (if ps.branchPrefix != null then ps.branchPrefix else cfg.branchPrefix)
            ]
            ++ lib.optionals (ps.forge != null) [ "--forge" ps.forge ]
            ++ lib.optionals (ps.forgeApiUrl != null) [ "--forge-api-url" ps.forgeApiUrl ]
            ++ lib.optional cfg.actions.commit "--commit"
            ++ lib.optional cfg.actions.signed "--signed"
            ++ lib.optional cfg.actions.push "--push"
            ++ lib.optional (if ps.pr != null then ps.pr else cfg.actions.pr) "--pr"
            ++ lib.optionals (ps.noBuild != []) [ "--no-build" (lib.concatStringsSep "," ps.noBuild) ]
            ++ lib.optionals (cfg.skip != []) [ "--skip" (lib.concatStringsSep "," cfg.skip) ];
          in ''
            CHECKOUT_PATH=${lib.escapeShellArg checkout}
            CLONE_URL=${lib.escapeShellArg (cloneUrl ps.repo)}
            BRANCH=${lib.escapeShellArg (toString ps.branch)}

            mkdir -p "$(dirname "$CHECKOUT_PATH")"
            # Serialize access to the checkout: services for different
            # maintainers share packageSets and may fire concurrently.
            (
            flock -x 9

            ${lib.optionalString (ps.forgeTokenFile != null) ''
            # Per-packageSet token overrides the global one for this set.
            export GITHUB_TOKEN="$(cat ${ps.forgeTokenFile})"
            ''}

            # HTTPS auth comes from the credential helper (reads GITHUB_TOKEN
            # from the environment), so the token is never part of the remote
            # URL. SSH auth is handled by GIT_SSH_COMMAND.
            if [ ! -d "$CHECKOUT_PATH/.git" ]; then
              git clone -c credential.helper="${credentialHelper}" "$CLONE_URL" "$CHECKOUT_PATH"
            fi
            git -C "$CHECKOUT_PATH" config credential.helper "${credentialHelper}"
            git -C "$CHECKOUT_PATH" remote set-url origin "$CLONE_URL"
            git -C "$CHECKOUT_PATH" fetch origin --prune

            # Branch selection: explicit > auto-discovered from upstream HEAD.
            if [ -z "$BRANCH" ]; then
              BRANCH=$(git -C "$CHECKOUT_PATH" symbolic-ref refs/remotes/origin/HEAD 2>/dev/null | sed 's|.*/||')
              [ -z "$BRANCH" ] && BRANCH=$(git -C "$CHECKOUT_PATH" rev-parse --abbrev-ref HEAD)
            fi

            # Clean up any mess from a previous run and fast-forward.
            git -C "$CHECKOUT_PATH" checkout -f "$BRANCH"
            git -C "$CHECKOUT_PATH" reset --hard "origin/$BRANCH"
            git -C "$CHECKOUT_PATH" clean -fdx

            cd "$CHECKOUT_PATH"
            # Per-packageSet commit identity overrides the service-level one.
            # Set inside the flock subshell, so it cannot leak to other sets.
            ${lib.optionalString (ps.gitUserName != null) ''
            export GIT_AUTHOR_NAME=${lib.escapeShellArg ps.gitUserName} GIT_COMMITTER_NAME=${lib.escapeShellArg ps.gitUserName}
            ''}
            ${lib.optionalString (ps.gitUserEmail != null) ''
            export GIT_AUTHOR_EMAIL=${lib.escapeShellArg ps.gitUserEmail} GIT_COMMITTER_EMAIL=${lib.escapeShellArg ps.gitUserEmail}
            ''}
            ${bumpkinCmd} update ${lib.escapeShellArgs args} --root "$CHECKOUT_PATH"

            ) 9>"$CHECKOUT_PATH.lock"

            # Restore the global token after a per-packageSet override.
            ${lib.optionalString (ps.forgeTokenFile != null && cfg.forgeTokenFile != null) ''
            export GITHUB_TOKEN="$(cat ${cfg.forgeTokenFile})"
            ''}
          '';
      in ''
        set -euo pipefail
        export GIT_ASKPASS=
        export GIT_TERMINAL_PROMPT=0

        ${lib.optionalString (cfg.forgeTokenFile != null) ''
        if [ -f ${cfg.forgeTokenFile} ]; then
          export GITHUB_TOKEN="$(cat ${cfg.forgeTokenFile})"
        fi''}

        ${lib.optionalString (cfg.gpgKeyFile != null) ''
        gpg --import ${cfg.gpgKeyFile}''}

        ${lib.concatMapStringsSep "\n" pkgSetBlock cfg.packageSets}
      '';
  };

  timerUnit = maintainer: {
    description = "bumpkin periodic update timer for maintainer ${maintainer}";
    wantedBy = [ "timers.target" ];
    timerConfig = {
      OnCalendar = cfg.schedule;
      RandomizedDelaySec = cfg.randomizedDelaySec;
      Persistent = true;
    };
  };

in
{
  options.services.bumpkin = {
    enable = mkEnableOption "bumpkin Nix flake upkeep bot";

    package = mkOption {
      type = types.package;
      defaultText = lib.literalExpression ''self.packages.''${system}.default'';
      description = "The bumpkin package to use.";
    };

    user = mkOption {
      type = types.str;
      default = "bumpkin";
      description = "System user for bumpkin services.";
    };

    group = mkOption {
      type = types.str;
      default = "bumpkin";
      description = "Group for the bumpkin user.";
    };

    schedule = mkOption {
      type = types.str;
      default = "daily";
      description = ''
        Systemd calendar event for periodic runs.
        Examples: "daily", "hourly", "*-*-* 03:00:00".
        See {manpage}`systemd.time(7)`.
      '';
    };

    randomizedDelaySec = mkOption {
      type = types.ints.unsigned;
      default = 3600;
      description = "Maximum random delay before each run to spread server load.";
    };

    maintainers = mkOption {
      type = types.listOf types.str;
      default = [];
      example = [ "74k1" "jtojnar" ];
      description = ''
        Maintainer handles to update packages for.
        Each gets a dedicated systemd service and timer unit.

        Each maintainer service iterates over all configured
        `packageSets` and runs `bumpkin update` in each checkout.
      '';
    };

    skip = mkOption {
      type = types.listOf types.str;
      default = [];
      example = [ "waterfox" "waterfox-unwrapped" ];
      description = ''
        Package attribute paths to skip. Useful for excluding packages
        with excessively long builds (browser forks, large compilations).
      '';
    };

    branchPrefix = mkOption {
      type = types.strMatching ".+";
      default = "bumpkin/";
      example = "upkeep/";
      description = ''
        Prefix for per-package branch names (`<prefix><package>`).
        No separator is added, so include a trailing `/` or `-` yourself.
        Can be overridden per packageSet.
      '';
    };

    actions = {
      commit = mkEnableOption "create per-package commits on dedicated branches";

      signed = mkEnableOption "sign commits with GPG or SSH";

      push = mkEnableOption "push per-package branches to origin after committing";

      pr = mkEnableOption "create pull requests for each pushed branch";
    };

    forgeTokenFile = mkOption {
      type = types.nullOr types.path;
      default = null;
      example = "/run/secrets/bumpkin-forge-token";
      description = ''
        Path to a file containing a forge personal access token.

        For GitHub, use a **classic** PAT (not fine-grained) with the
        `repo` scope. For Gitea/Forgejo, generate a token from
        Settings → Applications.

        The token is read into `GITHUB_TOKEN` and used for:
        - HTTPS git auth (clone/fetch/push) when `git.sshKeyFile` is not set
        - PR creation via the selected forge backend

        Required permissions: Contents (R/W), Pull requests (R/W).
      '';
    };

    gpgKeyFile = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "/run/secrets/bumpkin-gpg-key";
      description = ''
        Path to an ASCII-armored GPG private key file.

        The key is imported into the bumpkin user's GPG keyring before
        each run (`gpg --import`). Use together with `git.gpgFormat =
        "openpgp"` and `git.signingKey` set to the key fingerprint.
      '';
    };

    packageSets = mkOption {
      type = types.listOf (types.coercedTo types.str
        (ref: { repo = ref; })
        (types.submodule {
          options = {
            repo = mkOption {
              type = types.str;
              example = "github:74k1/tixpkgs";
              description = ''
                Flake ref or git URL for the package set repo.

                Supported formats:
                - Flake refs: `github:owner/repo`, `github:owner/repo/branch`,
                  `gitlab:owner/repo`, `sourcehut:owner/repo`
                - HTTPS: `https://github.com/owner/repo.git`
                - SSH: `git@github.com:owner/repo.git`,
                  `user@host:owner/repo.git`,
                  `git+ssh://user@host/owner/repo.git`

                Flake refs are mapped to their clone URL; raw git URLs are
                used directly.
              '';
            };

            branch = mkOption {
              type = types.nullOr types.str;
              default = null;
              description = ''
                Branch to track. If null (default), the upstream default
                branch is auto-discovered at runtime.
              '';
            };

            path = mkOption {
              type = types.nullOr types.str;
              default = null;
              description = ''
                Local checkout path. Defaults to
                `/var/lib/bumpkin/<owner>/<repo>` derived from the ref.
                Set this explicitly when two packageSets would collide.
              '';
            };

            forge = mkOption {
              type = types.nullOr (types.enum [ "auto" "github-cli" "github-api" "api" ]);
              default = null;
              description = ''
                Forge backend for PR creation.

                - `auto`: `gh` CLI for github.com if available, else the REST API
                - `github-cli`: always use `gh` CLI
                - `github-api`: use GitHub REST API via curl
                - `api`: use a GitHub-compatible REST API via curl (Gitea, Forgejo)

                If null (default), `auto` is used: `gh` for github.com,
                otherwise the REST API of the repo's host.
              '';
            };

            forgeApiUrl = mkOption {
              type = types.nullOr types.str;
              default = null;
              example = "https://gitea.example.com/api/v1";
              description = ''
                API base URL for the REST backends. Defaults to
                `https://api.github.com` for github.com and
                `https://<host>/api/v1` (Gitea, Forgejo) for other hosts.
              '';
            };

            branchPrefix = mkOption {
              type = types.nullOr (types.strMatching ".+");
              default = null;
              example = "upkeep/";
              description = ''
                Branch name prefix for this package set. If null (default),
                `services.bumpkin.branchPrefix` is used.
              '';
            };

            pr = mkOption {
              type = types.nullOr types.bool;
              default = null;
              description = ''
                Per-packageSet override of `actions.pr`. If null (default),
                the global `actions.pr` value is used.
              '';
            };

            forgeTokenFile = mkOption {
              type = types.nullOr types.str;
              default = null;
              description = ''
                Token file for this package set only. Overrides the global
                `forgeTokenFile` while this set is processed. Use this when
                package sets live on different forges (e.g. GitHub and a
                self-hosted Forgejo) and need different tokens.
              '';
            };

            gitUserName = mkOption {
              type = types.nullOr types.str;
              default = null;
              example = "ed";
              description = ''
                Commit author/committer name for this package set. If null
                (default), `services.bumpkin.git.userName` is used.
              '';
            };

            gitUserEmail = mkOption {
              type = types.nullOr types.str;
              default = null;
              example = "ed@example.com";
              description = ''
                Commit author/committer email for this package set. If null
                (default), `services.bumpkin.git.userEmail` is used.
              '';
            };

            noBuild = mkOption {
              type = types.listOf types.str;
              default = [];
              example = [ "waterfox" "waterfox-unwrapped" ];
              description = ''
                Package attribute names to skip building.
                Versions and hashes are still updated, committed, and
                included in PRs, but nix build is not run for these
                packages. Useful for packages that take too long to
                build locally.
              '';
            };
          };
        })
      );
      default = [];
      example = [
        "github:74k1/tixpkgs"
        { repo = "https://git.example.com/org/pkgs.git"; forge = "api"; forgeApiUrl = "https://git.example.com/api/v1"; }
      ];
      description = ''
        Package set repositories to track and update.

        Each element can be a shorthand string (flake ref or git URL)
        or an attrset with explicit options.
      '';
    };

    git = {
      userName = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "bumpkin-bot";
        description = "Git user.name for commits.";
      };

      userEmail = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "bumpkin-bot@example.org";
        description = "Git user.email for commits.";
      };

      gpgFormat = mkOption {
        type = types.nullOr (types.enum [ "openpgp" "ssh" ]);
        default = null;
        description = "Git gpg.format for signed commits.";
      };

      signingKey = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "~/.ssh/signing_key.pub";
        description = "Git user.signingkey (SSH pubkey path or GPG key ID).";
      };

      sshKeyFile = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "/run/secrets/bumpkin-deploy-key";
        description = ''
          Path to an SSH private key for git transport.

          When set, `GIT_SSH_COMMAND` is configured to use this key for all
          git operations. Takes priority over `forgeTokenFile` for git
          transport. The `forgeTokenFile` is still used for forge API calls
          (PR creation).
        '';
      };
    };
  };

  config = mkIf cfg.enable {
    users.users = mkIf (cfg.user == "bumpkin") {
      bumpkin = {
        isSystemUser = true;
        group = cfg.group;
        home = "/var/lib/bumpkin";
        createHome = true;
        description = "Bumpkin Nix upkeep bot user";
      };
    };

    users.groups = mkIf (cfg.group == "bumpkin") {
      bumpkin = {};
    };

    systemd.services = lib.listToAttrs (map (maintainer:
      lib.nameValuePair "bumpkin-${maintainer}" (serviceUnit maintainer)
    ) cfg.maintainers);

    systemd.timers = lib.listToAttrs (map (maintainer:
      lib.nameValuePair "bumpkin-${maintainer}" (timerUnit maintainer)
    ) cfg.maintainers);

    assertions = [
      {
        assertion = cfg.maintainers != [] -> cfg.packageSets != [];
        message = "services.bumpkin.packageSets must be non-empty when maintainers are configured.";
      }
      {
        assertion = cfg.actions.commit -> cfg.git.userName != null && cfg.git.userEmail != null;
        message = "services.bumpkin.git.userName and userEmail must be set when actions.commit is enabled.";
      }
      {
        assertion = cfg.actions.signed -> cfg.actions.commit;
        message = "services.bumpkin.actions.signed requires actions.commit to be enabled.";
      }
      {
        assertion = cfg.actions.signed -> cfg.git.signingKey != null;
        message = "services.bumpkin.git.signingKey must be set when actions.signed is enabled.";
      }
      {
        assertion = cfg.actions.push -> cfg.actions.commit;
        message = "services.bumpkin.actions.push requires actions.commit to be enabled.";
      }
      {
        assertion = cfg.actions.pr -> cfg.actions.push;
        message = "services.bumpkin.actions.pr requires actions.push to be enabled.";
      }
      {
        assertion = lib.all (ps:
          (if ps.pr != null then ps.pr else cfg.actions.pr)
          -> (ps.forgeTokenFile != null || cfg.forgeTokenFile != null)
        ) cfg.packageSets;
        message = "services.bumpkin.forgeTokenFile must be set (globally or per packageSet) when actions.pr is enabled (needed for forge API).";
      }
      (let
        paths = map (ps: if ps.path != null then ps.path else defaultPath ps) cfg.packageSets;
      in {
        assertion = builtins.length paths == builtins.length (lib.unique paths);
        message = "Two packageSets resolve to the same checkout path. Set `path` explicitly on one of them.";
      })
    ];
  };
}
