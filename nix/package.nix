# The phux CLI (client and server in one binary) and phux-mcp, built from
# source: `nix build`, `nix run`, `nix profile install github:no-phux/phux`.
#
# libghostty-vt-sys would download a prebuilt engine or clone Ghostty, and the
# Nix sandbox allows neither. So this hands it the same engine source its
# build.rs pins (GHOSTTY_SOURCE_DIR), that commit's own Zig package set for
# `zig build --system` (GHOSTTY_ZIG_SYSTEM_DIR), and the pinned Zig release.
# scripts/check-ghostty-pin.sh fails when ghosttyRev drifts from build.rs.
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

  nativeBuildInputs = [
    zigPinned
  ]
  # .cargo/config.toml links Linux targets with mold.
  ++ lib.optional stdenv.hostPlatform.isLinux mold
  # Ghostty's Darwin build runs `xcrun nmedit`.
  ++ lib.optionals stdenv.hostPlatform.isDarwin [
    cctools
    xcbuild
  ];

  env = {
    GHOSTTY_SOURCE_DIR = "${ghosttySrc}";
    GHOSTTY_ZIG_SYSTEM_DIR = ghosttyZigDeps;
    # Same engine flavor as the release binaries (scripts/build-release-binaries.sh).
    LIBGHOSTTY_VT_SYS_CPU = "baseline";
  };

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
