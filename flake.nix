{
  description = "haunt — a small terminal multiplexer for Ghostty";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    # Must match GHOSTTY_COMMIT in libghostty-vt-sys's build.rs.
    ghostty = {
      url = "github:ghostty-org/ghostty/0fa3d577a5614150ff8e2c152de5fb9d5c6d2dec";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      ghostty,
    }:
    let
      systems = [ "x86_64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (
        pkgs:
        let
          ghosttyLib = ghostty.packages.${pkgs.stdenv.hostPlatform.system}.libghostty-vt;
        in
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "haunt";
            version = "0.1.0";
            src = ./.;
            cargoLock = {
              lockFile = ./Cargo.lock;
              outputHashes."libghostty-vt-0.2.1" = "sha256-7KbO3Be+DWUvQnSLEksGVzNOKVieBhBqR4hbkoQtK6c=";
            };

            nativeBuildInputs = [ pkgs.pkg-config ];
            buildInputs = [ ghosttyLib ];

            env.PKG_CONFIG_PATH = "${ghosttyLib.dev}/share/pkgconfig";

            meta = {
              description = "A small terminal multiplexer for Ghostty";
              mainProgram = "haunt";
              platforms = pkgs.lib.platforms.linux;
            };
          };
        }
      );

      devShells = forAll (
        pkgs:
        let
          ghosttyLib = ghostty.packages.${pkgs.stdenv.hostPlatform.system}.libghostty-vt;
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
              ghosttyLib
            ];

            PKG_CONFIG_PATH = "${ghosttyLib.dev}/share/pkgconfig";
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          };
        }
      );
    };
}
