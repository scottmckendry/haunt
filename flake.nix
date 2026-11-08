{
  description = "haunt — a small terminal multiplexer for Ghostty";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    # Must match GHOSTTY_COMMIT in libghostty-vt-sys's build.rs.
    ghostty = {
      url = "github:ghostty-org/ghostty/a887df42c56f6de86c0fe6da9c4eeca37931e083";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { self, nixpkgs, ghostty }:
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
            cargoLock.lockFile = ./Cargo.lock;

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
              # libghostty-vt-sys builds Ghostty's VT library with Zig when
              # pkg-config is unavailable (for example outside the dev shell).
              # Ghostty's pinned commit requires exactly 0.15.x.
              zig_0_15
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
