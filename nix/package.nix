# The phux CLI (client and server in one binary) and phux-mcp, built from
# source: `nix build`, `nix run`, `nix profile install github:no-phux/phux`.
#
# libghostty-vt-sys would download a prebuilt engine or clone Ghostty, and the
# Nix sandbox allows neither. So the engine is built here, the way its
# build.rs builds it, from the commit build.rs pins, with that commit's own Zig
# package set and the pinned Zig release, and handed over through
# LIBGHOSTTY_VT_SYS_LIB_DIR. scripts/check-ghostty-pin.sh fails when
# ghosttyRev drifts from build.rs.
{
  lib,
  stdenv,
  makeRustPlatform,
  callPackage,
  cctools,
  xcbuild,
  mold,
  toolchain,
  zigPinned,
}:
let
  ghosttyRev = "ce42b4c3df8d206092d55d7add0a4f15ac29a0a2";
  # Fetched at evaluation (no import-from-derivation) so deps/packages.nix
  # can be imported from it.
  ghosttySrc = builtins.fetchTree {
    type = "github";
    owner = "phall1";
    repo = "ghostty";
    rev = ghosttyRev;
    narHash = "sha256-eEVJxWGtbHd1D+ZjIg3ZpuYFvXMK8zipIHm3VmZu9Xc=";
  };
  ghosttyZigDeps = callPackage "${ghosttySrc}/deps/packages.nix" { zig_0_17 = zigPinned; };
  libghosttyVt = stdenv.mkDerivation {
    pname = "libghostty-vt";
    version = builtins.substring 0 12 ghosttyRev;
    src = ghosttySrc;
    # Ghostty's Darwin build runs `xcrun nmedit`.
    nativeBuildInputs = [
      zigPinned
    ]
    ++ lib.optionals stdenv.hostPlatform.isDarwin [
      cctools
      xcbuild
    ];
    dontConfigure = true;
    dontInstall = true;
    dontFixup = true;
    # build.rs's flags for a release build. On Linux the target is explicit:
    # the static Zig release finds no ELF interpreter in the sandbox (no
    # /usr/bin/env) and would detect musl as the native ABI.
    buildPhase = ''
      runHook preBuild
      export ZIG_GLOBAL_CACHE_DIR="$TMPDIR/zig-global-cache"
      zig build -Demit-lib-vt=true -Doptimize=ReleaseFast -Dcpu=baseline \
        -Demit-xcframework=false -Dapp-runtime=none \
        ${lib.optionalString stdenv.hostPlatform.isLinux "-Dtarget=${stdenv.hostPlatform.parsed.cpu.name}-linux-gnu"} \
        --system ${ghosttyZigDeps} --cache-dir "$TMPDIR/zig-cache" --prefix "$out"
      runHook postBuild
    '';
  };
  rust = toolchain.override {
    extensions = [ ];
    targets = [ ];
  };
  rustPlatform = makeRustPlatform {
    cargo = rust;
    rustc = rust;
  };
  cargoToml = builtins.fromTOML (builtins.readFile ../Cargo.toml);
in
rustPlatform.buildRustPackage {
  pname = "phux";
  inherit (cargoToml.workspace.package) version;
  src = lib.cleanSource ../.;
  cargoLock = {
    lockFile = ../Cargo.lock;
    allowBuiltinFetchGit = true;
  };

  # The release tarball's members (scripts/pack-release.sh).
  cargoBuildFlags = [
    "--bin"
    "phux"
    "--bin"
    "phux-mcp"
  ];

  # .cargo/config.toml links Linux targets with mold.
  nativeBuildInputs = lib.optional stdenv.hostPlatform.isLinux mold;

  env.LIBGHOSTTY_VT_SYS_LIB_DIR = "${libghosttyVt}/lib";

  # The suite needs PTYs, sockets and the test runner; CI runs it.
  doCheck = false;

  doInstallCheck = true;
  installCheckPhase = ''
    $out/bin/phux --version | grep -F "$version"
  '';

  meta = {
    description = "A terminal multiplexer whose server you can drive from code";
    homepage = "https://phux.sh";
    license = lib.licenses.asl20;
    mainProgram = "phux";
    platforms = lib.platforms.darwin ++ lib.platforms.linux;
  };
}
