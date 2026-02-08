{
  inputs = {
    nixpkgs.url = "nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      nixpkgs,
      flake-utils,
      rust-overlay,
      crane,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        rust = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-analyzer"
            "rust-src"
          ];
        };

        craneLib = (crane.mkLib pkgs).overrideToolchain (p: rust);

        commonArgs = {
          src = ./.;
          strictDeps = true;
          nativeBuildInputs = [ ];
        };

        artifacts = commonArgs // {
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        };

        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);

        package = craneLib.buildPackage (
          artifacts
          // {
            meta.mainProgram = cargoToml.package.name;
            doCheck = false;
          }
        );

      in
      {
        checks = {
          clippy = craneLib.cargoClippy (
            artifacts
            // {
              cargoClippyExtraArgs = "-- --deny warnings";
            }
          );
          fmt = craneLib.cargoFmt artifacts;
          test = craneLib.cargoNextest artifacts;
        };
        packages.default = package;
        devShells.default = pkgs.mkShell {
          packages =
            with pkgs;
            [
              cargo-dist
              cargo-edit
              cargo-nextest
              fd
              just
              nodejs
              pandoc
            ]
            ++ [ rust ];
        };
      }
    );
}
