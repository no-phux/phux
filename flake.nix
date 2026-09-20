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
              asset = bunAssets.${system} or (throw "flake.nix: bun has no release asset for ${system}");
            in
            pkgs.bun.overrideAttrs (_: {
              version = bunVersion;
              src = pkgs.fetchurl {
                url = "https://github.com/oven-sh/bun/releases/download/bun-v${bunVersion}/${asset}.zip";
                hash = digests.${system};
              };
            });

        # usage CLI: same contract as bun. The Mise pin is the source of
        # truth; nixpkgs has lagged (6.4.1/6.6.1 while the crate and mise
        # pin are 6.9.0). Fetch the upstream release tarball until
        # `pkgs.usage.version` matches. Digests are hand-updated so a bump
        # that forgets them fails here instead of silently resolving an
        # older CLI. Get a new one with
        #   nix store prefetch-file --hash-type sha256 <asset-url>
        # Linux uses the musl builds so the pin does not need patchelf.
        usageVersion = miseTools.usage;
        usageAssets = {
          aarch64-darwin = "usage-universal-apple-darwin.tar.gz";
          x86_64-darwin = "usage-universal-apple-darwin.tar.gz";
          x86_64-linux = "usage-x86_64-unknown-linux-musl.tar.gz";
          aarch64-linux = "usage-aarch64-unknown-linux-musl.tar.gz";
        };
        usageDigests = {
          "6.9.0" = {
            aarch64-darwin = "sha256-nlMVFJ1aCNfRuGO06UcpqjYkA1PyhHUp0GrCH6x8bAE=";
            x86_64-darwin = "sha256-nlMVFJ1aCNfRuGO06UcpqjYkA1PyhHUp0GrCH6x8bAE=";
            x86_64-linux = "sha256-hpPIrev6w2IR6acGbTlMoLCKEK5XlECOh5G/MsEu7OA=";
            aarch64-linux = "sha256-beGIs56Fy9tIcoGBLCuzsVjYUrxLEiYPGbWpJri3vfc=";
          };
        };
        usagePinned =
          if (pkgs.usage.version or "") == usageVersion then
            pkgs.usage
          else
            let
              digests =
                usageDigests.${usageVersion}
                  or (throw "flake.nix: no usage digests pinned for ${usageVersion} (mise.toml). Add them to usageDigests.");
              asset = usageAssets.${system} or (throw "flake.nix: usage has no release asset for ${system}");
            in
            pkgs.stdenv.mkDerivation {
              pname = "usage";
              version = usageVersion;
              src = pkgs.fetchurl {
                url = "https://github.com/jdx/usage/releases/download/v${usageVersion}/${asset}";
                hash = digests.${system};
              };
              sourceRoot = ".";
              dontConfigure = true;
              dontBuild = true;
              installPhase = ''
                runHook preInstall
                mkdir -p $out/bin $out/share/man/man1
                install -m755 usage $out/bin/usage
                install -m644 usage.1 $out/share/man/man1/usage.1
                runHook postInstall
              '';
              meta = {
                homepage = "https://usage.jdx.dev";
                description = "CLI specification tool";
                license = pkgs.lib.licenses.mit;
                mainProgram = "usage";
              };
            };
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
            # usage CLI at the exact release pinned in mise.toml, so this
            # shell and the Mise path lint/render the same spec train as
            # the `usage-rs` crate (nixpkgs has lagged behind 6.9.0).
            usagePinned
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
          # Darwin archive tools, including the `nmedit` Ghostty's build
          # invokes through `xcrun`; with it the shell's own Apple SDK is
          # all a cold Ghostty build needs (see the shellHook).
          ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [ pkgs.cctools ];

          env.RUST_BACKTRACE = "1";

          # The wrapped cc/ld derive both -isysroot and -syslibroot from
          # DEVELOPER_DIR (nixpkgs' darwin-sdk-setup.bash), so this shell keeps
          # nixpkgs' default: its own Apple SDK. It used to adopt a host Xcode
          # here so Ghostty's build could find `xcrun nmedit`; that swapped the
          # linker's SDK under nixpkgs' ld64, which predates the `arm64e.x1`
          # TBD targets Xcode 27 ships, and every link in the shell failed.
          # `cctools` above provides nmedit, and xcrun resolves both the SDK
          # and nmedit under this DEVELOPER_DIR. The probe below only reports
          # a host where that resolution is broken (no Command Line Tools).
          shellHook =
            pkgs.lib.optionalString pkgs.stdenv.isDarwin ''
              if ! command -v xcrun >/dev/null 2>&1 ||
                 ! xcrun --show-sdk-path >/dev/null 2>&1 ||
                 ! xcrun --find nmedit >/dev/null 2>&1; then
                echo "phux: warning: xcrun cannot resolve the SDK or nmedit under" >&2
                echo "  DEVELOPER_DIR=$DEVELOPER_DIR; libghostty-vt-sys's Darwin build" >&2
                echo "  will likely fail. Install Apple's Command Line Tools; see" >&2
                echo "  docs/SETUP.md#platform-packages." >&2
              fi
            ''
            + ''
              export PATH="''${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
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
