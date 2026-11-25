{
  description = "deslop-please";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      flake-utils,
      ...
    }:
    flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-darwin" ] (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        # The lints read rust-analyzer's index, so its version is part of what
        # they report: pinned with the Rust it comes with.
        rustToolchain = pkgs.rust-bin.stable."1.99.0".minimal.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
            "clippy"
            "rustfmt"
          ];
        };
      in
      {
        # Built by .github/workflows/nix.yml into rapid-layers.cachix.org; it runs `rust-analyzer scip`, so that comes with it.
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "deslop-please";
          version = "0.1.4";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [ pkgs.makeWrapper ];
          doCheck = false;
          postFixup = ''
            wrapProgram $out/bin/deslop-please --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.rust-analyzer ]}
          '';
        };

        devShells.default = pkgs.mkShell {
          packages = [ rustToolchain ];
        };
      }
    );
}
