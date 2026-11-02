{
  description = "A terminal multiplexer whose server you can drive from code.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # 26.11 drops Intel Darwin; use the newest maintained stable channel there.
    nixpkgs-intel-darwin.url = "github:NixOS/nixpkgs/nixpkgs-26.05-darwin";
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
      nixpkgs-intel-darwin,
      flake-utils,
      rust-overlay,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import (if system == "x86_64-darwin" then nixpkgs-intel-darwin else nixpkgs) {
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
        darwinSdk = import ./nix/darwin-sdk.nix { inherit pkgs; };
        toolPins = import ./nix/toolchain-pins.nix { inherit pkgs miseTools; };

        # Bun is pinned to an exact release rather than whatever nixpkgs
        # carries. nixpkgs lagged at 1.3.13 while the pin was 1.4.0, so the
        # dev shell was type-checking against a runtime nobody ships — the
        # exact drift this whole block exists to prevent. The demo image in
        # no-phux/website pins the same tag; that pin is checked there.
        #
        # The DIGESTS are updated by hand on a version bump, deliberately: a
        # bump that forgets them fails here loudly ("no digests pinned") rather
        # than silently resolving to a different bun. Get a new one with
        #   nix store prefetch-file --hash-type sha256 <asset-url>
        bunVersion = miseTools.bun;
        bunAssets = {
          aarch64-darwin = "bun-darwin-aarch64";
          x86_64-darwin = "bun-darwin-x64-baseline";
          x86_64-linux = "bun-linux-x64";
          aarch64-linux = "bun-linux-aarch64";
        };
        bunDigests = {
          "1.4.4" = {
            aarch64-darwin = "sha256-wkDX8My5DCY6hJ/NsJt+5iYw2SE0jtaq/ahrnVd7xRA=";
            x86_64-darwin = "sha256-1KFVrfv9CRVes7oFyeOlRPOSl9emhhbTx3WfZ6tcR9w=";
            x86_64-linux = "sha256-v4zL+ysLgBWeD5ptFcVTF63YB3U028YTCj7bUvzLOVw=";
            aarch64-linux = "sha256-uqa7NvUdblhWHgK76oJQOKnfRU9nPW13bou/hvjHhQ4=";
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
          "7.1.4" = {
            aarch64-darwin = "sha256-yuGGWyu064wiCHOB1PvCk0SDeWDHHb+GXcVSlsGdU10=";
            x86_64-darwin = "sha256-yuGGWyu064wiCHOB1PvCk0SDeWDHHb+GXcVSlsGdU10=";
            x86_64-linux = "sha256-Zi/gSiobeUfnO3rU1gj1C5alAOW8LJ0ZYeKIlKjIf10=";
            aarch64-linux = "sha256-yCVsOOyCooKIMm1I08j8+lbh+xx26C9mxYrV+hiWXhs=";
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

        # Zig on Linux is the official ziglang.org release, verified against
        # the digests in .config/zig-toolchain.json (the same archives
        # scripts/install-zig.sh and the site builder install). nixpkgs'
        # zig_0_16 rebuilt on the GCC 16 / glibc 2.44 stdenv emits corrupt
        # ELF objects: some section symbols come out with st_shndx = SHN_UNDEF,
        # a different random subset on every run. libghostty-vt's compiler_rt.o
        # then fails the Rust link with mold "undefined symbol: " (empty name).
        # The release binary is statically linked and built by Zig's own
        # bootstrap, so no nixpkgs stdenv can reach it. macOS keeps nixpkgs'
        # clang-built Zig, which is unaffected.
        zigPins = builtins.fromJSON (builtins.readFile ./.config/zig-toolchain.json);
        zigPinned =
          if pkgs.stdenv.hostPlatform.isLinux then
            let
              pin =
                zigPins.archives.${system}
                  or (throw "flake.nix: .config/zig-toolchain.json pins no Zig archive for ${system}");
            in
            pkgs.stdenvNoCC.mkDerivation {
              pname = "zig";
              version = zigPins.version;
              src = pkgs.fetchurl {
                url = "https://ziglang.org/download/${zigPins.version}/${pin.archive}";
                sha256 = pin.sha256;
              };
              dontConfigure = true;
              dontBuild = true;
              dontStrip = true;
              dontPatchELF = true;
              installPhase = ''
                runHook preInstall
                mkdir -p $out/bin $out/libexec
                cp -r . $out/libexec/zig
                # Zig finds its lib/ next to its real path, so a symlink works.
                ln -s $out/libexec/zig/zig $out/bin/zig
                runHook postInstall
              '';
              meta = {
                homepage = "https://ziglang.org";
                description = "Zig compiler (official release binary)";
                license = pkgs.lib.licenses.mit;
                mainProgram = "zig";
              };
            }
          else
            pkgs.zig_0_16;

        # mbx (jdx/mr-boxington): the Cargo wrapper behind the shared build
        # cache. mise.toml is the source of truth for the release, same
        # contract as bun and usage. The shellHook puts libexec/mbx (a `cargo`
        # that runs mbx in Cargo shim mode, as mbx's own flake ships it) first
        # on PATH. Upstream publishes no Intel-macOS binary, so that system
        # simply has no mbx and builds with plain Cargo.
        #
        # The wrapper drops SDKROOT. mbx keys every native link (proc macros,
        # build scripts, binaries) on `xcrun --sdk "$SDKROOT" --show-sdk-*`,
        # and nixpkgs' xcbuild xcrun accepts an SDK name but not the path this
        # shell exports, so with SDKROOT set every link bypassed the cache and
        # every crate above a proc macro missed. Unset, mbx asks for `macosx`,
        # which resolves to the same SDK, and so do rustc and cc: a binary
        # built either way records the same `sdk` in LC_BUILD_VERSION.
        #
        # It drops CC and CXX for the same reason: mbx routes build-script C
        # and C++ (ring, *-sys crates) through its cache only when it supplies
        # the compiler, and stdenv's CC=clang / CXX=clang++ name the very
        # wrappers `cc` and `c++` resolve to on this PATH.
        mbxVersion = miseTools."mr-boxington";
        mbxAssets = {
          aarch64-darwin = "mbx-aarch64-apple-darwin.tar.gz";
          x86_64-linux = "mbx-x86_64-unknown-linux-musl.tar.gz";
          aarch64-linux = "mbx-aarch64-unknown-linux-musl.tar.gz";
        };
        mbxDigests = {
          "1.29.0" = {
            aarch64-darwin = "sha256-VnWyo7FgWXV5M0SLj9cFeXFvNWyuMRdmvs2do4yYOrU=";
            x86_64-linux = "sha256-jk5vzfmqEtuDzj2uEvy0XdKun05Rg8xb3dUar/AQ48A=";
            aarch64-linux = "sha256-F7NfXCcGPFafAgFkAIzW89ZESn56QM57+ElUsnJdKys=";
          };
        };
        mbxPinned =
          if !(mbxAssets ? ${system}) then
            null
          else
            pkgs.stdenv.mkDerivation {
              pname = "mbx";
              version = mbxVersion;
              src = pkgs.fetchurl {
                url = "https://github.com/jdx/mr-boxington/releases/download/v${mbxVersion}/${mbxAssets.${system}}";
                hash =
                  (mbxDigests.${mbxVersion}
                    or (throw "flake.nix: no mbx digests pinned for ${mbxVersion} (mise.toml). Add them to mbxDigests.")
                  ).${system};
              };
              sourceRoot = ".";
              nativeBuildInputs = [ pkgs.makeWrapper ];
              dontConfigure = true;
              dontBuild = true;
              installPhase = ''
                runHook preInstall
                install -Dm755 mbx $out/bin/mbx
                makeWrapper $out/bin/mbx $out/libexec/mbx/cargo \
                  --set MBX_CARGO_SHIM_MODE 1 \
                  --set MBX_CARGO_SHIM_PATH $out/libexec/mbx/cargo \
                  --unset SDKROOT \
                  --unset CC \
                  --unset CXX
                runHook postInstall
              '';
              meta = {
                homepage = "https://mr-boxington.jdx.dev";
                description = "Shared build cache and target-directory manager for Cargo";
                license = pkgs.lib.licenses.mit;
                mainProgram = "mbx";
              };
            };
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            toolchain
            # libghostty-vt-sys's source build, its fallback when it cannot
            # download the prebuilt archive, requires the exact Zig 0.16.0.
            zigPinned
            pkgs.pkg-config
            # Developer ergonomics.
            toolPins.just
            pkgs.cargo-nextest
            toolPins.cargo-deny
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
            # pinned in the client manifests (=0.2.130); the test harness
            # rejects a schema mismatch.
            toolPins.wasm-pack
            toolPins.wasm-bindgen-cli
            pkgs.binaryen
            pkgs.trunk
            pkgs.chromedriver
            # npm integration gates and workflow contracts use Node too.
            pkgs.nodejs_26
            # Documentation site builds use Bun, at the exact release pinned
            # in mise.toml (see bunPinned above) so this shell, the Mise path
            # and the site's production builder all agree.
            bunPinned
            # usage CLI at the exact release pinned in mise.toml, so this
            # shell and the Mise path lint/render the same spec train as
            # the `usage-rs` crate (nixpkgs has lagged behind 6.9.0).
            usagePinned
            # mbx itself (`mbx doctor`, `mbx gc`, `mbx stats`); the shellHook
            # is what routes plain `cargo` through it.
          ]
          ++ pkgs.lib.optional (mbxPinned != null) mbxPinned
          ++ [
            # Shell linting for scripts/ and examples/agents/ (just shellcheck).
            pkgs.shellcheck
            # GitHub workflow syntax plus expression validation (`just
            # guards`). This is release infrastructure, so it belongs
            # in the pinned shell rather than a developer's global tools.
            pkgs.actionlint
            # JSON plumbing for the CI observability scripts (scripts/ci/,
            # ADR-0047) and `just trace-attach`'s slow-render peek. The
            # hosted runners ship jq, but the scripts must also run in the
            # devshell (`just dep-stats`, local dashboard renders).
            pkgs.jq
            # The demo setup scripts and their test (just guards), Rust
            # test fixtures, examples and benchmarks. Python is a TOOLING
            # dependency only: no `cargo build` or CI decision needs it.
            toolPins.python
            # Debugging.
            pkgs.lldb
          ]
          # Fast linker for Linux builds (CI + Linux contributors). `mold`
          # backs the `-fuse-ld=mold` rustflags in .cargo/config.toml for the
          # linux-gnu targets; it has no mach-o backend, so it is Linux-only
          # and macOS keeps Apple's default linker (already the fast path).
          ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
            pkgs.mold
            # GPUI's X11/Wayland backends, font system and Vulkan renderer.
            pkgs.fontconfig
            pkgs.freetype
            pkgs.libxkbcommon
            pkgs.wayland
            pkgs.libx11
            pkgs.libxi
            pkgs.libxcb
            pkgs.vulkan-loader
            pkgs.mesa
          ]
          ++ darwinSdk.packages;

          env.RUST_BACKTRACE = "1";

          shellHook =
            pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
              # wgpu and xkbcommon load these at runtime, not through the linker.
              export LD_LIBRARY_PATH="${
                pkgs.lib.makeLibraryPath [
                  pkgs.vulkan-loader
                  pkgs.libxkbcommon
                  pkgs.wayland
                  pkgs.libx11
                  pkgs.libxcb
                ]
              }''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
            ''
            + darwinSdk.shellHook
            + ''
              # CARGO_HOME/bin holds `just install-dev`'s phux, which should
              # beat a `curl | sh` install in ~/.local/bin. It goes after this
              # shell's own store paths, not before them: it also holds
              # rustup's cargo/rustc proxies and cargo-installed tools, and
              # ahead of the pinned toolchain they made the shell run whatever
              # rustup or an inherited RUSTUP_TOOLCHAIN picked (Rust 1.98.1
              # after the 1.99.0 bump) instead of rust-toolchain.toml's.
              phux_store_path= phux_rest_path= phux_ifs=$IFS
              IFS=:
              for phux_entry in $PATH; do
                case $phux_entry in
                  /nix/store/*) phux_store_path=''${phux_store_path:+$phux_store_path:}$phux_entry ;;
                  *) phux_rest_path=''${phux_rest_path:+$phux_rest_path:}$phux_entry ;;
                esac
              done
              IFS=$phux_ifs
              export PATH="$phux_store_path:''${CARGO_HOME:-$HOME/.cargo}/bin:$phux_rest_path"
              unset phux_store_path phux_rest_path phux_ifs phux_entry RUSTUP_TOOLCHAIN
            ''
            # First on PATH; the wrapper delegates to the next `cargo`, the
            # pinned toolchain's. CI keeps plain Cargo unless the job opts in
            # with PHUX_CI_MBX=1, as the hosted Rust lanes do once
            # .github/actions/setup-rust-lane has set up mbx's GitHub cache;
            # release and other CI jobs stay on plain Cargo. PHUX_NO_MBX=1
            # opts a local shell out the same way.
            + pkgs.lib.optionalString (mbxPinned != null) ''
              if { [ -z "''${CI:-}" ] || [ -n "''${PHUX_CI_MBX:-}" ]; } && [ -z "''${PHUX_NO_MBX:-}" ]; then
                export PATH="${mbxPinned}/libexec/mbx:$PATH"
              fi
            ''
            + ''
              echo "phux dev shell — $(rustc --version)"
            '';
        };

        # Browser CI gets a browser matched to chromedriver without making
        # every native Linux lane download Chromium. Darwin uses host Chrome
        # and an explicitly matching driver (see docs/development.md).
        devShells.browser = self.devShells.${system}.default.overrideAttrs (old: {
          nativeBuildInputs =
            old.nativeBuildInputs ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.chromium ];
        });

        formatter = pkgs.nixfmt-rfc-style;
      }
    );
}
