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
        rustSpec = (builtins.fromTOML (builtins.readFile ./rust-toolchain.toml)).toolchain;
        toolchain = (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml).override {
          extensions = rustSpec.components ++ [
            "rust-src"
            "rust-analyzer"
            "llvm-tools-preview"
          ];
          targets = [ "wasm32-unknown-unknown" ];
        };

        # The Nix and Mise environments must resolve the SAME versions, so the
        # tools whose version this flake would otherwise leave to nixpkgs read
        # their pin from mise.toml instead. `just toolchain-check` gates the
        # rest of the matrix (Cargo manifests, clippy MSRV, CI, containers) and
        # now also gates this file, so no surface can move alone.
        miseTools = (builtins.fromTOML (builtins.readFile ./mise.toml)).tools;

        # Bun is pinned to an exact release rather than whatever nixpkgs
        # carries. docs/site builds and tests with it, its `@types/bun` tracks
        # that release, and docs/site/worker/Dockerfile pins the same tag for
        # production builds. nixpkgs lagged at 1.3.13 while the pin was 1.4.0,
        # so the dev shell was type-checking the site against a runtime nobody
        # ships — the exact drift this whole block exists to prevent.
        #
        # The DIGESTS are updated by hand on a version bump, deliberately: a
        # bump that forgets them fails here loudly ("no digests pinned") rather
        # than silently resolving to a different bun. Get a new one with
        #   nix store prefetch-file --hash-type sha256 <asset-url>
        bunVersion = miseTools.bun;
        bunAssets = {
          aarch64-darwin = "bun-darwin-aarch64";
          x86_64-linux = "bun-linux-x64";
          aarch64-linux = "bun-linux-aarch64";
        };
        bunDigests = {
          "1.4.0" = {
            aarch64-darwin = "sha256-xmnpf2Fk4cluBwF0jbmN+ndJKQjL2DlMdVcTSnNd44E=";
            x86_64-linux = "sha256-LQP7X7g6yLVnrKCigbLOGhoZ1Ij1bClo2Iw/Jekv5FI=";
            aarch64-linux = "sha256-SxozLuhhmD65O8/m93D/+U4+MbLDiL2uo8jtNeWO7Q4=";
          };
        };
        bunPinned =
          if pkgs.bun.version == bunVersion then
            # nixpkgs caught up; prefer its build and let the digests go stale.
            pkgs.bun
          else
            let
              digests =
                bunDigests.${bunVersion}
                  or (throw "flake.nix: no bun digests pinned for ${bunVersion} (mise.toml). Add them to bunDigests.");
              asset =
                bunAssets.${system} or (throw "flake.nix: bun has no release asset for ${system}");
            in
            pkgs.bun.overrideAttrs (_: {
              version = bunVersion;
              src = pkgs.fetchurl {
                url = "https://github.com/oven-sh/bun/releases/download/bun-v${bunVersion}/${asset}.zip";
                hash = digests.${system};
              };
            });
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
            # component included in this shell; cargo-bloat
            # attributes release binary size by crate/function. Pinned here
            # (not cargo-install like samply) so the recipes work out of the
            # box in the dev shell and versions stay reproducible.
            pkgs.cargo-llvm-lines
            pkgs.cargo-bloat
            # Web client (clients/phux-web, clients/phux-vt-web) toolchain.
            # wasm-bindgen-cli MUST match the `wasm-bindgen` crate version
            # pinned in the client manifests (=0.2.128); the test harness
            # rejects a schema mismatch.
            pkgs.wasm-pack
            pkgs.wasm-bindgen-cli
            pkgs.binaryen
            pkgs.trunk
            pkgs.chromedriver
            # npm integration gates and workflow contracts use Node too.
            pkgs.nodejs_24
            # Documentation site builds use Bun, at the exact release pinned
            # in mise.toml (see bunPinned above) so this shell, the Mise path
            # and the site's production builder all agree.
            bunPinned
            # Shell linting for scripts/ and examples/agents/ (just shellcheck).
            pkgs.shellcheck
            # GitHub workflow syntax plus expression validation (`just
            # workflow-check`). This is release infrastructure, so it belongs
            # in the pinned shell rather than a developer's global tools.
            pkgs.actionlint
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
          # Supplement the Darwin archive tools; SDK selection still needs
          # the host-Xcode preference below for cold Ghostty builds.
          ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.cctools ];

          env.RUST_BACKTRACE = "1";

          # Ghostty invokes `xcrun nmedit`; Nix's SDK alone lacks the tool,
          # and CLT-only SDK discovery has failed (phux-4xdh). Prefer a host
          # Xcode with the real binary, otherwise retain Nix's SDK and diagnose
          # the missing prerequisite. Native setup uses the same host contract:
          # docs/SETUP.md#platform-packages.
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

        # Browser CI gets a browser matched to chromedriver without making
        # every native Linux lane download Chromium. Darwin uses host Chrome
        # and an explicitly matching driver (see docs/SETUP.md).
        devShells.browser = self.devShells.${system}.default.overrideAttrs (old: {
          nativeBuildInputs =
            old.nativeBuildInputs ++ pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.chromium ];
        });

        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}
