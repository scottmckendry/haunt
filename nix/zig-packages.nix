# The Zig packages libghostty-vt needs, laid out the way `zig build --system`
# expects. This is the subset of Ghostty's generated deps/packages.nix listed
# in deps/libghostty-vt.txt at the pinned revision; copying the whole file
# would pull in every GUI dependency Ghostty has.
{
  lib,
  fetchzip,
  runCommand,
  zig_0_17,
}:
let
  packages = {
    "aro-0.0.0-JSD1QtuBNwCASyBtNF3pqTl_W3oAJQGEVyFAtrBSE_Pa" = fetchzip {
      name = "aro";
      url = "https://deps.files.ghostty.org/aro-d0c8c4d9c55daa7ef6e40cf0f630a5b5e900989b.tar.gz";
      hash = "sha256-hGxCOwFcqc+1V55j/4rmvMlwvISRHFwA5eK5CHrLaA8=";
    };
    "N-V-__8AAGmZhABbsPJLfbqrh6JTHsXhY6qCaLAQyx25e0XE" = fetchzip {
      name = "highway";
      url = "https://deps.files.ghostty.org/highway-66486a10623fa0d72fe91260f96c892e41aceb06.tar.gz";
      hash = "sha256-1xn+WAxeQyrz9KA3y4EOCr5Z94mom7J5TNdippPe2Sw=";
    };
    "translate_c-2.0.0-Q_BUWltOBwBYABF84EUL208pIFeUIwbBs29FmVBaafN5" = fetchzip {
      name = "translate_c";
      url = "https://deps.files.ghostty.org/translate_c-02ff0c523fb92a38939b04d0145c4a9602a74d56.tar.gz";
      hash = "sha256-13iPyWXUhBQXhF7AKbzKwrVdIrDQrMBNL8OfMyFdde0=";
    };
    "uucode-0.2.0-ZZjBPm6FVgBcY-79AHV8ckQj86z0JwOyVyj89VeZrEiv" = fetchzip {
      name = "uucode";
      url = "https://deps.files.ghostty.org/uucode-1fb73433bba5d93366c57f23ff2e9d7939746500.tar.gz";
      hash = "sha256-fZTYVFN0N10R0Ty+3tpMkTLqEBq4tTpkBK6G1m6VgmY=";
    };
    "N-V-__8AAP5JWgCGP_AD0teWpa4krRvE9VPZzvviGdbmN4jI" = fetchzip {
      name = "wuffs";
      url = "https://deps.files.ghostty.org/wuffs-7411f488fe2e2c205c3d3b3d28638b7356522930.tar.gz";
      hash = "sha256-sOuwt3R6xG7oqXaA66QGUjFHrEjGnTXjwYs6rSRb+7I=";
    };
  };
in
runCommand "libghostty-vt-zig-packages" { nativeBuildInputs = [ zig_0_17 ]; } ''
  mkdir -p "$out"

  # Zig 0.17 compiles its fetch build runner into the global cache the first
  # time it runs. Do that once here, not in parallel for every package.
  export ZIG_GLOBAL_CACHE_DIR="$TMPDIR/cache"
  mkdir -p "$ZIG_GLOBAL_CACHE_DIR"
  zig fetch --help >/dev/null

  # `zig fetch` unpacks the package filtered by its manifest's `.paths` into
  # the global cache, named by the hash Zig knows it by. Extract that into
  # $out at the same name, which is the layout `zig build --system` wants.
  unpack() {
    local hash
    if ! hash="$(cd "$TMPDIR" && zig fetch "$2")"; then
      return 1
    fi
    if [ "$hash" != "$1" ]; then
      echo "$2 is $hash to Zig, not $1" >&2
      return 1
    fi
    tar -xzf "$ZIG_GLOBAL_CACHE_DIR/p/$hash.tar.gz" -C "$out"
    rm -f "$ZIG_GLOBAL_CACHE_DIR/p/$hash.tar.gz"
  }
  export -f unpack

  cores="''${NIX_BUILD_CORES:-0}"
  if [ "$cores" -le 0 ]; then cores="$(nproc)"; fi
  xargs -r -P "$cores" -n 2 bash -c 'unpack "$@"' unpack <<'EOF'
  ${lib.concatStrings (lib.mapAttrsToList (hash: src: "${hash} ${src}\n") packages)}
  EOF
''
