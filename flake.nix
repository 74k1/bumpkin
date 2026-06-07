{
  description = "bumpkin: Rust upkeep bot for Nix flake package updates";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
    in
    {
      nixosModules = {
        bumpkin = import ./modules/nixos/bumpkin.nix;
        default = { config, lib, pkgs, ... }: {
          imports = [ self.nixosModules.bumpkin ];
          config.services.bumpkin.package = lib.mkDefault self.packages.${pkgs.system}.default;
        };
      };

      packages = forAllSystems (system:
        let pkgs = import nixpkgs { inherit system; };
        in {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "bumpkin";
            version = cargoToml.package.version;
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
          };
        });

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/bumpkin";
        };
      });

      checks = forAllSystems (system:
        let pkgs = import nixpkgs { inherit system; };
        in {
          package = self.packages.${system}.default;
        } // nixpkgs.lib.optionalAttrs (nixpkgs.lib.hasSuffix "-linux" system) {
          # Evaluate the NixOS module with a minimal config and force the
          # generated service script. git.userName is deliberately unset:
          # the module must eval without it (regression test for a null
          # string-interpolation bug).
          nixos-module = let
            nixos = nixpkgs.lib.nixosSystem {
              inherit system;
              modules = [
                self.nixosModules.default
                {
                  services.bumpkin = {
                    enable = true;
                    maintainers = [ "74k1" ];
                    packageSets = [ "github:74k1/tixpkgs" ];
                  };
                }
              ];
            };
          in pkgs.runCommand "bumpkin-nixos-module-eval" {
            script = nixos.config.systemd.services."bumpkin-74k1".script;
          } "touch $out";
        });

      devShells = forAllSystems (system:
        let pkgs = import nixpkgs { inherit system; };
        in {
          default = pkgs.mkShell {
            packages = [
              pkgs.cargo
              pkgs.rustc
              pkgs.rustfmt
              pkgs.clippy
              pkgs.nix
              pkgs.git
              pkgs.curl
            ];
          };
        });
    };
}
