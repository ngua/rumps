{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    flake-parts = {
      url = "github:hercules-ci/flake-parts/47478a4a003e745402acf63be7f9a092d51b83d7";
    };
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, flake-parts, rust-overlay, ... }@inputs:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [ "x86_64-linux" ];
      perSystem =
        { config
        , lib
        , system
        , pkgs
        , ...
        }:
        let
          toolchain = pkgs.rust-bin.selectLatestNightlyWith (
            t: t.default.override {
              extensions = [ "rust-src" ];
            }
          );
        in
        {

          _module.args.pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };

          devShells.default = pkgs.mkShell {
            nativeBuildInputs = with pkgs; [
              rust-analyzer-unwrapped
              toolchain
              pkg-config
              openssl
              evcxr
            ];
          };
        };
    };
}
