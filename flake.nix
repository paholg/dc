{
  inputs = {
    nixpkgs.url = "nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
    nix2container = {
      url = "github:nlewo/nix2container";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      flake-utils,
      rust-overlay,
      crane,
      nix2container,
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

        crateName = craneLib.crateNameFromCargoToml {
          cargoToml = ./crates/cli/Cargo.toml;
        };
        serviceCrateName = craneLib.crateNameFromCargoToml {
          cargoToml = ./crates/proxy/Cargo.toml;
        };

        commonArgs = {
          inherit (crateName) pname version;
          src = ./.;
          strictDeps = true;
          nativeBuildInputs = [ ];
        };

        artifacts = commonArgs // {
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        };

        package = craneLib.buildPackage (
          artifacts
          // {
            meta.mainProgram = crateName.pname;
            doCheck = false;
          }
        );

        servicePackage = craneLib.buildPackage (
          artifacts
          // {
            pname = serviceCrateName.pname;
            version = serviceCrateName.version;
            cargoToml = ./crates/proxy/Cargo.toml;
            cargoExtraArgs = "--package ${serviceCrateName.pname}";
            meta.mainProgram = serviceCrateName.pname;
            doCheck = true;
          }
        );

        # The mdbook site, deployed to https://devconcurrent.paholg.com by
        # `.github/workflows/docs.yml`.
        docs = pkgs.stdenvNoCC.mkDerivation {
          pname = "devconcurrent-docs";
          inherit (crateName) version;
          src = pkgs.lib.cleanSourceWith {
            src = ./docs;
            # `book` is mdbook's local build output; keep it out of the hash.
            filter = path: type: !(type == "directory" && baseNameOf path == "book");
          };
          nativeBuildInputs = [ pkgs.mdbook ];
          buildPhase = "mdbook build --dest-dir $out";
          dontInstall = true;
        };

        # OCI image for the service.
        #
        # `sidecarDir` is an empty `/etc/sidecar/` that exists at create time so
        # the proxy can `PUT archive` plan.json (+ cert/key) into it before
        # starting the sidecar — Docker's archive endpoint 404s if the target
        # directory doesn't exist in the image.
        sidecarDir = pkgs.runCommand "sidecar-dir" { } "mkdir -p $out/etc/sidecar";
        dockerImage = nix2container.packages.${system}.nix2container.buildImage {
          name = "devconcurrent-proxy";
          tag = serviceCrateName.version;
          copyToRoot = [
            pkgs.cacert
            sidecarDir
          ];
          maxLayers = 100;
          config = {
            Entrypoint = [ "${servicePackage}/bin/${serviceCrateName.pname}" ];
            ExposedPorts = {
              "53/udp" = { };
              "53/tcp" = { };
              "80/tcp" = { };
              "443/tcp" = { };
            };
          };
        };

      in
      {
        checks = {
          clippy = craneLib.cargoClippy (
            artifacts
            // {
              cargoClippyExtraArgs = "--all-targets -- --deny warnings";
            }
          );
          fmt = craneLib.cargoFmt artifacts;
          test = craneLib.cargoNextest artifacts;
        };
        packages = {
          default = package;
          service = servicePackage;
          inherit docs;
        }
        // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          docker-service-image = dockerImage;
        };
        devShells.default = pkgs.mkShell {
          packages =
            with pkgs;
            [
              cargo-dist
              cargo-edit
              cargo-machete
              cargo-nextest
              fd
              just
              mdbook
              nodejs
              pandoc
              rumdl
            ]
            ++ [ rust ];
          env = {
            RUST_BACKTRACE = 1;
          };
        };
      }
    );
}
