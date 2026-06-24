{
  description = "Minimalistic coding agent written in Rust, optimized for memory footprint and performance";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system}.appendOverlays [
          (import ./nix/overlay)
          (import ./nix/overlay/development.nix)
        ];
      in
      {
        overlays = {
          default = import ./nix/overlay;
          development = import ./nix/overlay/development;
        };

        packages = {
          inherit (pkgs) zerostack;
          default = self.packages.${system}.zerostack;
        };

        apps = {
          zerostack = {
            type = "app";
            program = pkgs.lib.getExe self.packages.${system}.zerostack;
          };

          default = self.apps.${system}.zerostack;
        };

        devShells.default = pkgs.zerostack-dev-shell;
      }
    );
}
