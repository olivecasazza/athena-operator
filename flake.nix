{
  description = "Athena ML Operator";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };
  outputs = { self, nixpkgs, flake-utils }:
    let
      # Use flake-utils for per-system outputs
      outputs = flake-utils.lib.eachDefaultSystem (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in {
          packages = {
            # Define image build targets here for Hydra
            default = pkgs.hello;
          };
        });
    in outputs // {
      # Hydra specifically looks for `hydraJobs` at the top level
      hydraJobs = {
        x86_64-linux = outputs.packages.x86_64-linux;
        aarch64-linux = outputs.packages.aarch64-linux;
      };
    };
}
