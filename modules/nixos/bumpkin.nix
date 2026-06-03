{ config, lib, pkgs, ... }:

let
  cfg = config.services.bumpkin;
  inherit (lib) types mkOption mkEnableOption mkIf;

  stripDotGit = s:
    let m = builtins.match "(.+)\\.git$" s;
    in if m != null then builtins.head m else s;

  extractOwnerRepo = ref:
    let
      flakeMatch = builtins.match "^(github|gitlab|sourcehut):([^/]+)/([^/]+)(/.*)?$" ref;
      sshMatch   = builtins.match "^git@[^:]+:([^/]+)/([^/]+)$" ref;
      httpsMatch = builtins.match "^https?://[^/]+/([^/]+)/([^/]+)$" ref;
    in
      if flakeMatch != null then {
        owner = builtins.elemAt flakeMatch 1;
        repo  = builtins.elemAt flakeMatch 2;
      }
      else if sshMatch != null then {
        owner = builtins.elemAt sshMatch 0;
        repo  = stripDotGit (builtins.elemAt sshMatch 1);
      }
      else if httpsMatch != null then {
        owner = builtins.elemAt httpsMatch 0;
        repo  = stripDotGit (builtins.elemAt httpsMatch 1);
      }
      else throw "cannot extract owner/repo from ref: ${ref}";

  defaultPath = ps:
    let info = extractOwnerRepo ps.repo;
    in "/var/lib/bumpkin/${info.owner}/${info.repo}";

  bumpkinCmd = "${cfg.package}/bin/bumpkin";

  serviceUnit = maintainer: {
    description = "bumpkin update run for maintainer ${maintainer}";
    after = [ "network-online.target" ];
    wants = [ "network-online.target" ];

    path = [ cfg.package pkgs.git pkgs.nix pkgs.curl pkgs.jq pkgs.gh ]
      ++ lib.optional (cfg.git.gpgFormat == "openpgp" || cfg.gpgKeyFile != null) pkgs.gnupg
      ++ lib.optional (cfg.git.sshKeyFile != null) pkgs.openssh;

    serviceConfig = {
      Type = "oneshot";
      User = cfg.user;
      Group = cfg.group;
      StateDirectory = "bumpkin";
      RemainAfterExit = false;
    };

    environment = {
      HOME = "/var/lib/bumpkin";
      NIX_PATH = "nixpkgs=${pkgs.path}";
    } // lib.optionalAttrs (cfg.git.userName != null) {
      GIT_AUTHOR_NAME = cfg.git.userName;
      GIT_AUTHOR_EMAIL = cfg.git.userEmail;
      GIT_COMMITTER_NAME = cfg.git.userName;
      GIT_COMMITTER_EMAIL = cfg.git.userEmail;
    } // lib.optionalAttrs (cfg.git.sshKeyFile != null) {
      GIT_SSH_COMMAND = "ssh -i ${cfg.git.sshKeyFile} -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new";
    };

    script =
      let
        pkgSetBlock = idx: ps:
          let
            checkout = if ps.path != null then ps.path else defaultPath ps;
          in ''
            CHECKOUT_PATH="${checkout}"
            REPO_REF="${ps.repo}"
            ${lib.optionalString (ps.branch != null) ''
            CFG_BRANCH="${ps.branch}"''}
            ${lib.optionalString (ps.forge != null) ''
            CFG_FORGE="${ps.forge}"''}
            ${lib.optionalString (ps.forgeApiUrl != null) ''
            CFG_FORGE_API="${ps.forgeApiUrl}"''}

            # Resolve clone URL via nix flake metadata.
            META_FILE="/tmp/bumpkin-meta-${toString idx}.json"
            if nix --extra-experimental-features 'flakes nix-command' \
                   flake metadata --json "$REPO_REF" >"$META_FILE" 2>/dev/null; then
              META_TYPE=$(jq -r '.resolved.type // "git"' "$META_FILE")
              case "$META_TYPE" in
                github)
                  OWNER=$(jq -r '.resolved.owner' "$META_FILE")
                  REPO=$(jq -r '.resolved.repo' "$META_FILE")
                  CLONE_URL="https://github.com/$OWNER/$REPO.git"
                  ;;
                gitlab)
                  OWNER=$(jq -r '.resolved.owner' "$META_FILE")
                  REPO=$(jq -r '.resolved.repo' "$META_FILE")
                  CLONE_URL="https://gitlab.com/$OWNER/$REPO.git"
                  ;;
                sourcehut)
                  OWNER=$(jq -r '.resolved.owner' "$META_FILE")
                  REPO=$(jq -r '.resolved.repo' "$META_FILE")
                  CLONE_URL="https://git.sr.ht/~$OWNER/$REPO"
                  ;;
                *)
                  CLONE_URL=$(jq -r '.resolved.url // .original.url // empty' "$META_FILE")
                  ;;
              esac
              [ -z "$CLONE_URL" ] && CLONE_URL="$REPO_REF"
            else
              CLONE_URL="$REPO_REF"
              META_TYPE="git"
              META_FILE=""
            fi

            # Auth injection: SSH key takes priority over HTTPS token.
            if [ -n "''${GIT_SSH_COMMAND:-}" ]; then
              : # leave CLONE_URL as-is (SSH handles auth)
            elif [ -n "''${GITHUB_TOKEN:-}" ]; then
              case "$CLONE_URL" in
                https://*)
                  CLONE_URL="https://${cfg.git.userName or "bumpkin"}:''${GITHUB_TOKEN}@$(echo "$CLONE_URL" | sed 's|^https://||')"
                  ;;
              esac
            fi

            # Clone or fast-forward.
            mkdir -p "$(dirname "$CHECKOUT_PATH")"
            if [ ! -d "$CHECKOUT_PATH/.git" ]; then
              git clone "$CLONE_URL" "$CHECKOUT_PATH"
            else
              git -C "$CHECKOUT_PATH" fetch origin --prune
            fi

            # Branch selection: explicit > auto-discovered from upstream HEAD.
            if [ -n "''${CFG_BRANCH:-}" ]; then
              BRANCH="$CFG_BRANCH"
            else
              BRANCH=$(git -C "$CHECKOUT_PATH" symbolic-ref refs/remotes/origin/HEAD 2>/dev/null | sed 's|.*/||')
              [ -z "$BRANCH" ] && BRANCH=$(git -C "$CHECKOUT_PATH" rev-parse --abbrev-ref HEAD)
            fi

            git -C "$CHECKOUT_PATH" checkout "$BRANCH"
            git -C "$CHECKOUT_PATH" merge --ff-only "origin/$BRANCH"

            # Set remote with token for push if using HTTPS auth.
            if [ -z "''${GIT_SSH_COMMAND:-}" ] && [ -n "''${GITHUB_TOKEN:-}" ]; then
              git -C "$CHECKOUT_PATH" remote set-url origin "$CLONE_URL"
            fi

            # Clean up any mess from a previous run.
            git -C "$CHECKOUT_PATH" checkout -f "$BRANCH"
            git -C "$CHECKOUT_PATH" reset --hard "origin/$BRANCH"
            git -C "$CHECKOUT_PATH" clean -fdx

            # Resolve forge backend.
            if [ -n "''${CFG_FORGE:-}" ]; then
              FORGE="$CFG_FORGE"
            elif [ -n "$META_FILE" ] && [ -f "$META_FILE" ]; then
              case "$META_TYPE" in
                github) FORGE="auto" ;;
                gitlab) FORGE="api" ;;
                *) FORGE="api" ;;
              esac
            else
              case "$CLONE_URL" in
                *github.com*) FORGE="auto" ;;
                *gitlab.com*) FORGE="api" ;;
                *) FORGE="api" ;;
              esac
            fi

            cd "$CHECKOUT_PATH"
            ${bumpkinCmd} update \
              --root "$CHECKOUT_PATH" \
              --maintainer ${maintainer} \
              --forge "$FORGE" \
              ''${CFG_FORGE_API:+--forge-api-url "$CFG_FORGE_API"} \
              ${if cfg.actions.commit then "--commit" else ""} \
              ${if cfg.actions.signed then "--signed" else ""} \
              ${if cfg.actions.push   then "--push"   else ""} \
              ${if cfg.actions.pr     then "--pr"     else ""} \
              ${if cfg.skip != [] then "--skip ${lib.concatStringsSep "," cfg.skip}" else ""}
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

        ${lib.concatStringsSep "\n" (lib.imap0 (i: ps: pkgSetBlock i ps) cfg.packageSets)}
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
                - SSH: `git@github.com:owner/repo.git`

                Flake refs are resolved at runtime via `nix flake metadata`.
                Raw git URLs are used directly.
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

                - `auto`: prefer `gh` CLI if available, fall back to GitHub API
                - `github-cli`: always use `gh` CLI
                - `github-api`: use GitHub REST API via curl
                - `api`: use a GitHub-compatible REST API via curl (Gitea, Forgejo)

                If null (default), the forge is auto-detected from flake
                metadata or the repo URL.
              '';
            };

            forgeApiUrl = mkOption {
              type = types.nullOr types.str;
              default = null;
              example = "https://gitea.example.com/api/v1";
              description = ''
                API base URL when `forge` is `api`.

                - Gitea: `https://gitea.example.com/api/v1`
                - Forgejo: `https://forgejo.example.com/api/v1`

                Only used when `forge` is explicitly `api`. Ignored otherwise.
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

      extraConfig = mkOption {
        type = types.attrsOf types.str;
        default = {};
        description = "Additional git config entries.";
        example = { "core.sshCommand" = "ssh -i /path/to/deploy-key"; };
      };
    };

    gc = {
      enable = mkEnableOption "periodic Nix store garbage collection";
      schedule = mkOption {
        type = types.str;
        default = "weekly";
        description = "Systemd calendar event for GC.";
      };
      randomizedDelaySec = mkOption {
        type = types.ints.unsigned;
        default = 7200;
        description = "Random delay for GC in seconds.";
      };
    };
  };

  config = mkIf cfg.enable {
    nix.settings.experimental-features = [ "nix-command" "flakes" ];
    nix.nixPath = [ "nixpkgs=${pkgs.path}" ];

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

    environment.etc."bumpkin-home-gitconfig".source = let
      extraLines = lib.mapAttrsToList (k: v: "\t${k} = ${v}") cfg.git.extraConfig;
    in pkgs.writeText "gitconfig" ''
      [user]
      ${lib.optionalString (cfg.git.userName != null) "\tname = ${cfg.git.userName}"}
      ${lib.optionalString (cfg.git.userEmail != null) "\temail = ${cfg.git.userEmail}"}
      ${lib.optionalString (cfg.git.signingKey != null) "\tsigningkey = ${cfg.git.signingKey}"}
      ${lib.optionalString (cfg.git.gpgFormat != null) ''
      [gpg]
      	format = ${cfg.git.gpgFormat}''}
      ${lib.concatStringsSep "\n" extraLines}
    '';
    systemd.tmpfiles.rules = [
      "L+ /var/lib/bumpkin/.gitconfig - - - - /etc/bumpkin-home-gitconfig"
    ];

    systemd.services =
      (lib.listToAttrs (map (maintainer:
        lib.nameValuePair "bumpkin-${maintainer}" (serviceUnit maintainer)
      ) cfg.maintainers))
      // lib.optionalAttrs cfg.gc.enable {
        "bumpkin-gc" = {
          description = "Bumpkin Nix store garbage collection";
          serviceConfig = {
            Type = "oneshot";
            User = cfg.user;
            ExecStart = "${pkgs.nix}/bin/nix store gc";
          };
        };
      };

    systemd.timers =
      (lib.listToAttrs (map (maintainer:
        lib.nameValuePair "bumpkin-${maintainer}" (timerUnit maintainer)
      ) cfg.maintainers))
      // lib.optionalAttrs cfg.gc.enable {
        "bumpkin-gc" = {
          description = "Bumpkin periodic Nix store GC";
          wantedBy = [ "timers.target" ];
          timerConfig = {
            OnCalendar = cfg.gc.schedule;
            RandomizedDelaySec = cfg.gc.randomizedDelaySec;
            Persistent = true;
          };
        };
      };

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
        assertion = cfg.actions.pr -> cfg.forgeTokenFile != null;
        message = "services.bumpkin.forgeTokenFile must be set when actions.pr is enabled (needed for forge API).";
      }
      {
        assertion = lib.all (ps: ps.forge != "api" || ps.forgeApiUrl != null) cfg.packageSets;
        message = "Each packageSet with forge = \"api\" must set forgeApiUrl.";
      }
      (let
        paths = map (ps: ps.path or (defaultPath ps)) cfg.packageSets;
      in {
        assertion = builtins.length paths == builtins.length (lib.unique paths);
        message = "Two packageSets resolve to the same checkout path. Set `path` explicitly on one of them.";
      })
    ];
  };
}
