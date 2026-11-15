{
  description = "haunt — a small terminal multiplexer for Ghostty";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    {
      self,
      nixpkgs,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      version = (fromTOML (builtins.readFile ./Cargo.toml)).package.version;
    in
    {
      packages = forAll (
        pkgs:
        let
          libghostty-vt = pkgs.callPackage ./nix/libghostty-vt.nix { };
        in
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "haunt";
            inherit version;
            src = ./.;
            cargoLock = {
              lockFile = ./Cargo.lock;
              outputHashes."libghostty-vt-0.2.1" = "sha256-e5YkGJxvKF6kNaFlRC9M1VCftcKrk3YgnirkW/EXlCQ=";
            };

            nativeBuildInputs = [ pkgs.pkg-config ];
            buildInputs = [ libghostty-vt ];

            env.PKG_CONFIG_PATH = "${libghostty-vt.dev}/lib/pkgconfig";

            meta = {
              description = "A small terminal multiplexer for Ghostty";
              mainProgram = "haunt";
              platforms = pkgs.lib.platforms.unix;
            };
          };
        }
      );

      devShells = forAll (
        pkgs:
        let
          libghostty-vt = pkgs.callPackage ./nix/libghostty-vt.nix { };
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              rust-analyzer
              clippy
              rustfmt
              zig
              pkg-config
              git
              libghostty-vt
            ];

            PKG_CONFIG_PATH = "${libghostty-vt.dev}/lib/pkgconfig";
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          };
        }
      );
    };
}
