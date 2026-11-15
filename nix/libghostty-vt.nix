# Build libghostty-vt from Ghostty's source without taking Ghostty's flake as
# an input. Adapted from nix/libghostty-vt.nix at the pinned revision.
{
  callPackage,
  lib,
  stdenv,
  fetchFromGitHub,
  git,
  pkg-config,
  zig_0_17,
  darwin,
  fixDarwinDylibNames,
  xcbuild,
}:
stdenv.mkDerivation (finalAttrs: {
  pname = "libghostty-vt";
  version = "1.0.1+108cacb-nix";

  # Must match GHOSTTY_COMMIT in libghostty-vt-sys's build.rs.
  src = fetchFromGitHub {
    owner = "ghostty-org";
    repo = "ghostty";
    rev = "108cacb17ed8990840cd14abfce43d8ed8503ca1";
    hash = "sha256-3XBFm8yq25P9eCdjLWwVwZ0tedWCN39U1FbjYD8C+w4=";
  };

  deps = callPackage ./zig-packages.nix { inherit zig_0_17; };

  nativeBuildInputs = [
    git
    pkg-config
    zig_0_17
  ]
  ++ lib.optionals stdenv.hostPlatform.isDarwin [
    darwin.cctools
    fixDarwinDylibNames
    xcbuild
  ];

  buildInputs = [ ];

  doCheck = false;
  dontSetZigDefaultFlags = true;

  zigBuildFlags = [
    "--system"
    "${finalAttrs.deps}"
    "-Dlib-version-string=${finalAttrs.version}"
    "-Dcpu=baseline"
    "-Doptimize=ReleaseFast"
    "-Dapp-runtime=none"
    "-Demit-lib-vt=true"
    "-Dsimd=true"
  ];

  outputs = [
    "out"
    "dev"
  ];

  postInstall = ''
    mkdir -p "$dev/lib"
    mv "$out/lib/libghostty-vt.a" "$dev/lib/"
  '';

  postFixup = ''
    substituteInPlace "$dev/lib/pkgconfig/libghostty-vt-static.pc" \
      --replace-fail "$out" "$dev"
  '';

  meta = {
    homepage = "https://ghostty.org";
    license = lib.licenses.mit;
    platforms = zig_0_17.meta.platforms;
    pkgConfigModules = [
      "libghostty-vt"
      "libghostty-vt-static"
    ];
  };
})
