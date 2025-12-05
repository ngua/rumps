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
    pre-commit-nix = {
      url = "github:cachix/pre-commit-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, flake-parts, rust-overlay, pre-commit-nix, ... }@inputs:
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
              extensions = [ "rust-src" "rustfmt" ];
            }
          );

          pre-commit = pre-commit-nix.lib.${system}.run {
            src = ./.;
            hooks = {
              nixpkgs-fmt.enable = true;
              rustfmt = {
                enable = true;
                packageOverrides.rustfmt = toolchain;
              };
            };
          };
        in
        {

          _module.args.pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };

          checks.pre-commit = pre-commit;

          devShells.default = pkgs.mkShell {
            inherit (pre-commit) shellHook;
            nativeBuildInputs = with pkgs; [
              rust-analyzer-unwrapped
              toolchain
              pkg-config
              openssl
              evcxr
              cargo-insta
            ];
          };
        };
    };
}
