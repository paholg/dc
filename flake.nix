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

        # Member crates set `version.workspace = true`, which crane can't
        # resolve from a member Cargo.toml alone.
        workspaceVersion = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
        crateName =
          craneLib.crateNameFromCargoToml {
            cargoToml = ./crates/cli/Cargo.toml;
          }
          // {
            version = workspaceVersion;
          };
        serviceCrateName =
          craneLib.crateNameFromCargoToml {
            cargoToml = ./crates/proxy/Cargo.toml;
          }
          // {
            version = workspaceVersion;
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
          nativeBuildInputs = [
            pkgs.mdbook
            # Provides mdbook-tabs.
            pkgs.mdbook-plugins
          ];
          buildPhase = "mdbook build --dest-dir $out";
          dontInstall = true;
        };

        # OCI image for the service.
        dockerImage = nix2container.packages.${system}.nix2container.buildImage {
          name = "devconcurrent-proxy";
          tag = serviceCrateName.version;
          copyToRoot = [
            pkgs.cacert
            # We need to PUT files into containers before starting them; docker
            # will 404 unless the directories already exist.
            (pkgs.runCommand "mkdirs" { } # bash
              ''
                # Stores the sidecar's plan.json and certificate/key
                mkdir -p $out/etc/sidecar
                # Stores the intermediate CA
                mkdir -p $out/etc/proxy-ca
              ''
            )
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
        // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
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
              gh-markdown-preview
              jq
              just
              mdbook
              # Provides mdbook-tabs.
              mdbook-plugins
              nodejs
              pandoc
              rumdl
              vhs
              watchexec
              # For recording demos:
              bashInteractive
              starship
              xh
            ]
            ++ [ rust ];
        };
      }
    );
}
