{
  description = "phux — a terminal multiplexer built on libghostty-vt";

  nixConfig = {
    extra-substituters = [ "https://phux.cachix.org" ];
    extra-trusted-public-keys = [
      "phux.cachix.org-1:DXR/XX4dfm0juc8k04vgkKRY8V/IhUtgJF6ynxnqQOk="
    ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
        # Read channel/components from rust-toolchain.toml. No hash needed —
        # rust-overlay derives it from the rustup metadata.
        toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            toolchain
            # libghostty-vt-sys requires the exact Zig 0.16.0 toolchain.
            pkgs.zig_0_16
            pkgs.pkg-config
            # Developer ergonomics.
            pkgs.just
            pkgs.cargo-nextest
            pkgs.cargo-deny
            pkgs.cargo-watch
            pkgs.cargo-insta
            pkgs.cargo-mutants
            # Build observability (`just timings` / `just llvm-lines` /
            # `just bloat`). cargo-llvm-lines reads the `llvm-tools-preview`
            # component already pinned in rust-toolchain.toml; cargo-bloat
            # attributes release binary size by crate/function. Pinned here
            # (not cargo-install like samply) so the recipes work out of the
            # box in the dev shell and versions stay reproducible.
            pkgs.cargo-llvm-lines
            pkgs.cargo-bloat
            # Web client (clients/phux-web, clients/phux-vt-web) toolchain.
            # wasm-bindgen-cli MUST match the `wasm-bindgen` crate version
            # pinned in the client manifests (=0.2.121); the test harness
            # rejects a schema mismatch.
            pkgs.wasm-pack
            pkgs.wasm-bindgen-cli
            pkgs.binaryen
            pkgs.trunk
            pkgs.chromedriver
            # Shell linting for scripts/ and examples/agents/ (just shellcheck).
            pkgs.shellcheck
            # JSON plumbing for the CI observability scripts (scripts/ci/,
            # ADR-0047) and `just trace-attach`'s slow-render peek. The
            # hosted runners ship jq, but the scripts must also run in the
            # devshell (`just dep-stats`, local dashboard renders).
            pkgs.jq
            # scripts/gen-bitmap-font.py and its drift gate (`just font-check`,
            # a `just ci` and ci.yml step). Python is a TOOLING dependency only:
            # the glyph table it emits is committed, so no `cargo build` ever
            # needs an interpreter. Pinning it here is what lets the gate hard-
            # fail on a missing python3 instead of skipping itself.
            pkgs.python3
            # Debugging.
            pkgs.lldb
          ]
          # Fast linker for Linux builds (CI + Linux contributors). `mold`
          # backs the `-fuse-ld=mold` rustflags in .cargo/config.toml for the
          # linux-gnu targets; it has no mach-o backend, so it is Linux-only
          # and macOS keeps Apple's default linker (already the fast path).
          ++ pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.mold ]
          # `nmedit`, for macOS cold builds of libghostty-vt.
          #
          # ghostty's Darwin-only src/build/libsystem_override.sh rewrites the
          # static archive so consumers bind memcpy/sin/cos to Apple's
          # libSystem instead of Zig's compiler-rt. It shells out to
          # `xcrun nmedit`. Inside this devshell `xcode-select -p` resolves to
          # the nix apple-sdk, whose XcodeDefault toolchain ships `nm` but NOT
          # `nmedit` — so a macOS contributor's first (uncached) libghostty
          # build died with "error: tool 'nmedit' not found", and only that
          # build: everything else was already in the store.
          #
          # `cctools-binutils-darwin`, which the clang wrapper already pulls
          # in, does not carry nmedit either. The full `cctools` does. xcrun
          # falls back to PATH when the toolchain lacks a tool, so putting it
          # on PATH is enough — no DEVELOPER_DIR override, which would swap the
          # whole pinned SDK out for whatever Xcode the host happens to have.
          #
          # Linux is unaffected: the script is guarded Darwin-only, so CI and
          # the Linux release legs never invoke it.
          ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.cctools ];

          env.RUST_BACKTRACE = "1";

          # Ghostty's Darwin build (src/build/libsystem_override.sh) shells
          # out to `xcrun nmedit`, which resolves through $DEVELOPER_DIR. The
          # nix apple-sdk package mimics an Xcode `Developer` directory
          # closely enough that stdenv points DEVELOPER_DIR/SDKROOT at it,
          # but it ships only an `nmedit` *specification* plist, not the
          # binary -- so every cold macOS build of libghostty-vt-sys in this
          # shell died with "error: tool 'nmedit' not found" (phux-4xdh).
          # Only a full Xcode install carries the real binary; the Command
          # Line Tools package alone does not, and pointing zig's SDK
          # discovery at a CLT-only DEVELOPER_DIR makes it report
          # DarwinSdkNotFound instead. So: prefer a host Xcode whose
          # toolchain actually has nmedit (checked, not assumed), and only
          # when none is found fall back to nix's own DEVELOPER_DIR (today's
          # behavior) with a loud warning instead of a silent later failure.
          shellHook =
            pkgs.lib.optionalString pkgs.stdenv.isDarwin ''
              _phux_nix_developer_dir=$DEVELOPER_DIR
              _phux_xcode_select=$(/usr/bin/xcode-select -p 2>/dev/null)
              _phux_host_developer_dir=""
              for _phux_candidate in "$_phux_xcode_select" /Applications/Xcode.app/Contents/Developer; do
                if [ -n "$_phux_candidate" ] && {
                  [ -x "$_phux_candidate/usr/bin/nmedit" ] ||
                  [ -x "$_phux_candidate/Toolchains/XcodeDefault.xctoolchain/usr/bin/nmedit" ]
                }; then
                  _phux_host_developer_dir=$_phux_candidate
                  break
                fi
              done
              if [ -n "$_phux_host_developer_dir" ]; then
                export DEVELOPER_DIR=$_phux_host_developer_dir
              else
                export DEVELOPER_DIR=$_phux_nix_developer_dir
                echo "phux: warning: no host Xcode with a real nmedit was found" >&2
                echo "  (checked \`xcode-select -p\` = '$_phux_xcode_select' and" >&2
                echo "  /Applications/Xcode.app). libghostty-vt-sys's Darwin build" >&2
                echo "  will likely fail with \"tool 'nmedit' not found\"." >&2
                echo "  Install full Xcode (the Command Line Tools package alone is" >&2
                echo "  not enough -- zig reports DarwinSdkNotFound against it) and" >&2
                echo "  run: sudo xcode-select -s /Applications/Xcode.app" >&2
              fi
              unset _phux_nix_developer_dir _phux_xcode_select _phux_host_developer_dir _phux_candidate
            ''
            + ''
              echo "phux dev shell — $(rustc --version)"
            '';
        };

        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}
