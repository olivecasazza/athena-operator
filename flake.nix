{
  description = "Athena ML Operator";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    { nixpkgs, flake-utils, ... }:
    let
      mkDeployment = lib: import ./nix/athena/deployment.nix { inherit lib; };
      outputs = flake-utils.lib.eachDefaultSystem (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          athena = mkDeployment pkgs.lib;
        in
        {
          packages = {
            default = pkgs.hello;
            helm-chart = athena.helmChart pkgs;
            k8s-manifests = athena.k8sManifests pkgs;
          };

          checks = {
            helm-chart = athena.helmChart pkgs;
            k8s-manifests = athena.k8sManifests pkgs;
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
