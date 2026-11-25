# Runtime-only environment for the hosted nextest archive shards.
{
  pkgs,
  toolchain,
  toolPins,
  bunPinned,
}:
pkgs.mkShellNoCC {
  # Rust metadata probes need rustc, not a native linker or SDK. Do not
  # realize Zig, browsers, desktop libraries, or development tools here.
  packages = [
    (toolchain.override {
      extensions = [ ];
      targets = [ ];
    })
    pkgs.cargo-nextest
    toolPins.just
    toolPins.python
    pkgs.jq
    # Native chat's real harness-adapter e2e executes TypeScript.
    # Reuse the pinned runtime; no node_modules or compiler is needed.
    bunPinned
  ];
  env.RUST_BACKTRACE = "1";
}
