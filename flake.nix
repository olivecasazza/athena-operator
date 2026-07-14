{
  description = "Athena ML Operator";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      nixpkgs,
      flake-utils,
      crane,
      ...
    }:
    let
      mkDeployment = lib: import ./nix/athena/deployment.nix { inherit lib; };
      outputs = flake-utils.lib.eachDefaultSystem (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          craneLib = crane.mkLib pkgs;
          mardiGras = pkgs.buildGoModule rec {
            pname = "mardi-gras";
            version = "0.22.0";
            src = pkgs.fetchFromGitHub {
              owner = "quietpublish";
              repo = "mardi-gras";
              rev = "v${version}";
              hash = "sha256-JBiig+kI2X6SZhEb5mAiacLiMI5nIU0klWlBCBFshMo=";
            };
            vendorHash = "sha256-FuXR6Cq+BLJ7h5UqFEDJ/BVlWIUpye7GOpiqbhjv6aM=";
            subPackages = [ "cmd/mg" ];
            ldflags = [
              "-s"
              "-w"
            ];
            postInstall = ''
              ln -s "$out/bin/mg" "$out/bin/mardi-gras"
            '';
          };
          perles = pkgs.buildGoModule rec {
            pname = "perles";
            version = "0.8.6";
            src = pkgs.fetchFromGitHub {
              owner = "zjrosen";
              repo = "perles";
              rev = "v${version}";
              hash = "sha256-rSXRxdK9Z5crYyABlyrc3xASikvyaPRQOzU9UyiJJc4=";
            };
            vendorHash = "sha256-Z90bsgXyfrz0Wurj0cJG4J5ZoCBp5ED51tVWby5xaOs=";
            ldflags = [
              "-s"
              "-w"
            ];
            doCheck = false;
          };
          athena = mkDeployment pkgs.lib;
          consoleLibPath = pkgs.lib.makeLibraryPath [
            pkgs.atk
            pkgs.bzip2
            pkgs.cairo
            pkgs.fontconfig
            pkgs.gdk-pixbuf
            pkgs.gcc.cc.lib
            pkgs.glib
            pkgs.gtk3
            pkgs.harfbuzz
            pkgs.libxkbcommon
            pkgs.zlib
            pkgs.vulkan-loader
            pkgs.wayland
            pkgs.libx11
            pkgs.libxcursor
            pkgs.libxext
            pkgs.libxi
            pkgs.libxrandr
            pkgs.libxrender
            pkgs.pango
            pkgs.xorgproto
          ];
          consoleFontsConf = pkgs.makeFontsConf {
            fontDirectories = [ pkgs.nerd-fonts.monaspace ];
          };
          consoleNativeBuildInputs = [
            pkgs.pkg-config
            pkgs.gcc
          ];
          consoleBuildInputs = [
            pkgs.atk
            pkgs.bzip2
            pkgs.cairo
            pkgs.fontconfig
            pkgs.gdk-pixbuf
            pkgs.glib
            pkgs.gtk3
            pkgs.harfbuzz
            pkgs.libxkbcommon
            pkgs.zlib
            pkgs.vulkan-loader
            pkgs.wayland
            pkgs.libx11
            pkgs.libxcursor
            pkgs.libxext
            pkgs.libxi
            pkgs.libxrandr
            pkgs.libxrender
            pkgs.pango
            pkgs.xorgproto
          ];
          operatorSrc = pkgs.lib.cleanSourceWith {
            src = ./operator;
            filter =
              path: type:
              let
                base = baseNameOf path;
              in
              !(type == "directory" && base == "target");
          };
          consoleCargoArtifacts = craneLib.buildDepsOnly {
            pname = "athena-console-deps";
            src = operatorSrc;
            cargoExtraArgs = "-p athena-console";
            nativeBuildInputs = consoleNativeBuildInputs;
            buildInputs = consoleBuildInputs;
          };
          operatorCargoArtifacts = craneLib.buildDepsOnly {
            pname = "athena-operator-deps";
            src = operatorSrc;
            cargoExtraArgs = "-p athena";
          };
          athenaConsole = craneLib.buildPackage {
            pname = "athena-console";
            src = operatorSrc;
            cargoArtifacts = consoleCargoArtifacts;
            cargoExtraArgs = "-p athena-console";
            nativeBuildInputs = consoleNativeBuildInputs;
            buildInputs = consoleBuildInputs;
            postInstall = ''
              install -Dm644 crates/athena-console/assets/athena-console.svg \
                $out/share/icons/hicolor/scalable/apps/athena-console.svg
              install -Dm644 crates/athena-console/assets/athena-console.svg \
                $out/share/athena-console/favicon.svg
              install -Dm644 /dev/stdin $out/share/applications/athena-console.desktop <<EOF
              [Desktop Entry]
              Type=Application
              Name=Athena Console
              Comment=Kubernetes-native Athena research operator console
              Exec=$out/bin/athena-console
              Icon=athena-console
              Terminal=false
              Categories=Development;Science;
              StartupWMClass=Athena Console
              EOF
            '';
          };
          athenaOperator = craneLib.buildPackage {
            pname = "athena-operator";
            src = operatorSrc;
            cargoArtifacts = operatorCargoArtifacts;
            cargoExtraArgs = "-p athena";
          };
          athenaOperatorImage = pkgs.dockerTools.buildLayeredImage {
            name = "ghcr.io/olivecasazza/athena-operator";
            tag = "dev";
            contents = [ pkgs.cacert ];
            config = {
              Entrypoint = [ "${athenaOperator}/bin/athena" ];
              Env = [
                "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
                "RUST_LOG=info"
              ];
              ExposedPorts = {
                "8080/tcp" = { };
              };
            };
          };
        in
        {
          apps = {
            athena-console = {
              type = "app";
              program = "${
                pkgs.writeShellApplication {
                  name = "athena-console";
                  runtimeInputs = [
                    pkgs.lapce
                    pkgs.xdg-utils
                  ];
                  text = ''
                    export KUBECONFIG="''${KUBECONFIG:-$HOME/.kube/config}"
                    export ATHENA_EDITOR="''${ATHENA_EDITOR:-lapce}"
                    export FONTCONFIG_FILE="${consoleFontsConf}"
                    export ICED_BACKEND=tiny-skia
                    export LD_LIBRARY_PATH="${consoleLibPath}:''${LD_LIBRARY_PATH:-}"
                    exec ${athenaConsole}/bin/athena-console "$@"
                  '';
                }
              }/bin/athena-console";
            };
            athena-console-dev = {
              type = "app";
              program = "${
                pkgs.writeShellApplication {
                  name = "athena-console-dev";
                  runtimeInputs = [
                    pkgs.cargo
                    pkgs.fontconfig
                    pkgs.gcc
                    pkgs.lapce
                    pkgs.nerd-fonts.monaspace
                    pkgs.pkg-config
                    pkgs.rustc
                    pkgs.watchexec
                    pkgs.xdg-utils
                  ];
                  text = ''
                      repo="''${ATHENA_REPO_ROOT:-$PWD}"

                      if [ ! -d "$repo/operator/crates/athena-console" ]; then
                        echo "athena-console-dev must run from the athena-operator repo root, or set ATHENA_REPO_ROOT" >&2
                        exit 1
                      fi

                    export KUBECONFIG="''${KUBECONFIG:-$HOME/.kube/config}"
                    export ATHENA_EDITOR="''${ATHENA_EDITOR:-lapce}"
                    export FONTCONFIG_FILE="${consoleFontsConf}"
                    export ICED_BACKEND=tiny-skia
                    export LD_LIBRARY_PATH="${consoleLibPath}:''${LD_LIBRARY_PATH:-}"

                      echo "Starting Athena Console workbench using KUBECONFIG=$KUBECONFIG"
                      echo "Using release profile for smoother native UI rendering"
                      cd "$repo/operator"
                      exec watchexec \
                        --restart \
                        --watch Cargo.toml \
                        --watch Cargo.lock \
                        --watch crates/athena-api \
                        --watch crates/athena-console \
                        --exts rs,toml \
                        -- cargo run --release -p athena-console
                  '';
                }
              }/bin/athena-console-dev";
            };
          };

          packages = {
            default = athenaConsole;
            athena-operator = athenaOperator;
            athena-operator-image = athenaOperatorImage;
            athena-console = athenaConsole;
            helm-chart = athena.helmChart pkgs;
            k8s-manifests = athena.k8sManifests pkgs;
          };

          checks = {
            helm-chart = athena.helmChart pkgs;
            k8s-manifests = athena.k8sManifests pkgs;
          };

          devShells = {
            default = pkgs.mkShell {
              packages = [
                pkgs.cargo
                pkgs.curl
                pkgs.fontconfig
                pkgs.gcc
                pkgs.go
                pkgs.helix
                pkgs.jujutsu
                pkgs.lapce
                pkgs.neovim
                pkgs.nerd-fonts.monaspace
                pkgs.nodejs
                mardiGras
                perles
                pkgs.pkg-config
                pkgs.rustc
                pkgs.rustfmt
                pkgs.clippy
                pkgs.uv
                pkgs.watchexec
                pkgs.xdg-utils
              ]
              ++ consoleBuildInputs;

              shellHook = ''
                export KUBECONFIG="''${KUBECONFIG:-$HOME/.kube/config}"
                export ATHENA_EDITOR="''${ATHENA_EDITOR:-lapce}"
                export FONTCONFIG_FILE="${consoleFontsConf}"
                export ICED_BACKEND=tiny-skia
                export LD_LIBRARY_PATH="${consoleLibPath}:''${LD_LIBRARY_PATH:-}"
                export ATHENA_REPO_ROOT="$PWD"

                echo "Athena dev shell active (repo-local via direnv)."
                echo "Run: cd operator && cargo run --release -p athena-console"
                echo "Live-reload: nix run .#athena-console-dev"
              '';
            };
          };
        }
      );
    in
    outputs
    // {
      lib.athena = mkDeployment nixpkgs.lib;

      hydraJobs = {
        x86_64-linux = outputs.packages.x86_64-linux;
        aarch64-linux = outputs.packages.aarch64-linux;
      };

      formatter.x86_64-linux = nixpkgs.legacyPackages.x86_64-linux.nixfmt-rfc-style;
      formatter.aarch64-linux = nixpkgs.legacyPackages.aarch64-linux.nixfmt-rfc-style;
    };
}
