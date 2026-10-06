---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-05
---

# Contributor setup

**TL;DR.** Pick Mise or Nix — both first-class — then run the same commands.
`mise install` manages compilers and gate tools on your OS packages; `nix
develop` provisions every lane. After that: `just doctor`, then a scoped check
or `just ci`. Just is the repo command layer, not a third environment. Browser
work uses the committed engine binary.

## Choosing an environment

Two are supported. They are not tiers of the same thing.

| | Nix (`flake.nix`) | Mise (`mise.toml`) |
|---|---|---|
| Provides | Compilers, every root gate tool, browser and Cockpit toolchains, observability tools, pinned system libraries | Compilers and runtimes, plus the root gate tools |
| Runs | Everything, including `just ci-full`, the browser lanes, and Cockpit | `just ci`, once `cargo-nextest` is installed separately |
| Costs | A dev-shell build on first use | Seconds; per-tool downloads |
| Choose it when | You want one command to reproduce any lane, or you are touching the browser client, Cockpit, or release infrastructure | You already run a working native toolchain and want the pins without adopting Nix |

```sh
mise install            # toolchains and gate tools
# or:
nix develop             # the fully provisioned shell
```

Then the same commands in either environment:

```sh
just doctor             # default scope: native; also core / docs / integrations / web / cockpit / ci
just ci                 # deterministic/unit bar
just ci-full            # that plus real-server e2e and agent smoke; the PR bar
```

`just` is how you run this repository after the tools are on PATH. It is not
how you get a compiler. Recipes live in `just/*.just` (`just cockpit-test`
still works); the root justfile is imports plus `just --list`. Do not add a
third bootstrap (`setup-rust.sh` / `install-zig.sh`) unless you are on the
native CI path below.

With [direnv](https://direnv.net), `.envrc` loads Nix by default. To make Mise
the environment it loads, create an untracked `.envrc.local`:

```sh
echo 'export PHUX_ENV=mise' > .envrc.local && direnv allow
```

Use one environment per shell. After changing `.envrc.local`, run `direnv
reload` and let the shell's direnv hook apply it before building. Leave an
explicit `nix develop` shell before switching to Mise; `mise exec` changes tool
resolution but does not unload inherited Nix SDK/compiler variables. Conversely,
use Nix in a separate shell rather than activating Mise over its toolchain.

If you already activate Mise in your shell, you do not need direnv. `rustc` and
`zig` from mise shims are the Mise path.

**They are held together, not merely documented as similar.**
`just toolchain-check` is a `just ci` gate that reads `mise.toml`, `flake.nix`,
`rust-toolchain.toml`, `.config/zig-toolchain.json`, the Cargo manifests, the
workflows, and the container builders and fails if any of them names a
different Rust, Zig, Node, Bun, usage CLI, or mbx. `just toolchain-parity` is the
runtime half: it resolves both environments and compares the binaries they
actually hand you. Run it after bumping a pin or `flake.lock`.

`cargo-nextest` is the one root gate tool Mise does not supply: it has no
prebuilt entry in Mise's registry, and building it from source can require a
newer compiler than this repository's Rust pin. Install the
[official prebuilt binary](https://nexte.st/docs/installation/pre-built-binaries/);
`bash scripts/doctor.sh ci` reports it missing. Browser, Cockpit, and
build-observability tools are Nix-or-native; `mise.toml` lists them under
"deliberately absent".

## Binary cache

CI pushes the dev shell's build outputs to `https://phux.cachix.org`, and
`flake.nix` asks Nix to use it. The Nix daemon honours a flake's substituter
only for a trusted user or a cache in its own `trusted-substituters`, so on a
default multi-user install every `nix develop` prints `ignoring untrusted
substituter` and builds those outputs locally instead. `just doctor` reports
this and prints the one-time fix: append the cache and its public key to the
daemon's config (`/etc/nix/nix.custom.conf` on a Determinate install, else
`/etc/nix/nix.conf`) as `extra-trusted-substituters` and
`extra-trusted-public-keys`, then restart the daemon. That trusts this one
cache, which is narrower than adding yourself to `trusted-users`.

## Build cache (mbx)

Both environments run `cargo` through [mbx](https://mr-boxington.jdx.dev)
(Mr. Boxington), pinned in `mise.toml` and fetched at the same release by
`flake.nix`. A phux worktree build costs tens of gigabytes, and plain Cargo
gives every worktree its own cold build and a `target/` that only grows: six
concurrent agent worktrees filled a 927 GB disk on 2026-09-29. mbx answers that
with three things:

- **One shared store.** Compilations, native links and build-script runs are
  keyed independently of the checkout path, so building one worktree warms the
  next: a fresh worktree restores the whole dependency graph. On APFS, Btrfs,
  XFS (reflink) or ZFS, restored outputs are copy-on-write clones of the
  store, not copies. A fresh worktree's `cargo check -p phux-server` restores
  every unit (2-5 s); its first `cargo nextest run --workspace --no-run`
  restores all but the insta-using unit-test harnesses, `phux-client-ffi`
  (an rlib+staticlib+cdylib crate mbx does not cache) and the leaves above it.
- **Managed targets.** `target` becomes a symlink into mbx's cache directory,
  and the whole cache has a disk budget that scales with the disk. Targets are
  collected when their checkout disappears, goes unused, or exceeds its budget,
  so a removed worktree no longer strands its build.
- **A machine-wide compiler pool.** Concurrent Cargo commands share CPU and
  memory permits and deduplicate identical compilations in flight, which is
  what keeps a fleet of agents from oversubscribing the machine.

How it is wired: Mise sets `mr_boxington = true` on the `rust` entry, so mise's
`cargo` command wrapper runs mbx. The Nix shell puts mbx's own `cargo` wrapper
first on `PATH`. Either way plain `cargo` (and every `just` recipe) is cached.
Calling `~/.cargo/bin/cargo` directly bypasses it; so does an agent harness
whose `PATH` never loaded either environment, which should use
`mise exec -- cargo ...` or `nix develop -c cargo ...`. `just doctor` reports
whether `mbx` is on `PATH`. In the Nix shell the wrapper also unsets `SDKROOT`, `CC` and
`CXX` for Cargo: nixpkgs' `xcrun` cannot describe an SDK by path, which made
every native link uncacheable, and mbx caches build-script C only when it picks
the compiler; `cc`, `c++` and `xcrun --sdk macosx` resolve to the same tools. CI
shells keep plain Cargo unless a job sets `PHUX_CI_MBX=1`, which the hosted
Linux Rust lanes do after `.github/actions/setup-rust-lane` restores their mbx
cache. Upstream ships no Intel-macOS binary, so that platform builds without it.

```sh
mbx doctor            # tools, cache access, and reflink/hard-link/copy mode
mbx stats             # lifetime savings and cross-worktree sharing
mbx explain --last    # why the last build hit, missed, or bypassed
mbx gc --dry-run      # what collection would remove
```

To opt out: in the Nix shell, set `PHUX_NO_MBX=1` before `nix develop`; on the
Mise path, put `[tools] rust = { version = "<the pin>", mr_boxington = false }`
in an untracked `mise.local.toml`. mbx's `share_workspace_root` stays at its
default (off): turning it on would make panic locations and debug info name a
placeholder instead of the real source path, and the panic hook's location line
is what operators read.

If Rust reports `E0514` (a dependency compiled by a different compiler), fix the
shell's toolchain selection first. Check `rustc -V`, `cargo -V`, and `mbx doctor`.
Then `mbx clean "$PWD"` clears only this checkout's managed target and learned
incremental state; shared cache objects remain. Retry the same build in the
selected environment. Do not share a `CARGO_TARGET_DIR` between worktrees or
make cache deletion part of every build.

Two things keep a crate reusable across worktrees, and both are enforced:

- No compile-time checkout path in library or test code (`just
  cache-portable-check`): read `CARGO_MANIFEST_DIR` and the test binary paths
  from the test runner at run time (`crates/phux/tests/common/runner.rs`), and
  pass a binary's own checkout path in from its `main.rs`.
- The Zig engine builds once per input set, outside any target directory:
  `libghostty-vt-sys` keeps it under `$XDG_CACHE_HOME/libghostty-vt-sys` when
  that is set, else `~/Library/Caches` (macOS) or `~/.cache`, and copies the
  install tree into `OUT_DIR`.
  `LIBGHOSTTY_VT_SYS_CACHE_DIR` moves it; set it empty to build in `OUT_DIR`.

On Linux, keep mbx's cache directory on a filesystem with reflinks (ext4 falls
back to read-only hard links). Rust build output compresses well, so a
dedicated ZFS volume with `compression=zstd` or `lz4` stretches the same disk
much further for a heavy multi-agent machine.

## Pick your work area

Run commands from the repository root. `just` is a convenience command runner;
the shell, Cargo, and npm commands below also work directly.

| Work area | Prerequisites | First check |
|---|---|---|
| Handwritten docs | Git, Bash, standard Unix utilities | `bash scripts/check-docs.sh` |
| Pure Rust domain / wire codec | Rust, native compiler/linker | `just core-check` or `just crate-check phux-protocol` |
| Server, TUI, CLI, engine-dependent protocol helpers, FFI | Rust, Zig, platform packages | `just doctor native`, then `just crate-check phux-server` |
| Agent integrations | Node/npm and Bun | `just integration-check pi` (or `runtime`, `opencode-v2`, `omp`, `claude`) |
| Browser client | Rust WASM target, Node, WASM tools (engine binary is committed) | `just doctor web`; see [Browser client](#browser-client) |
| Cockpit app | Apple-silicon Mac, SDK, Zig, Node, Rust FFI, Python | `just doctor cockpit`, then `just cockpit-test` |
| GPUIX desktop development | Apple-silicon Mac, Xcode with Metal, native Rust/Zig, Bun, Node, Python | `just doctor desktop`, then `just desktop-app` |
| Full root validation | Native prerequisites plus gate tools | `just doctor ci`, then `just ci-full` |

`bash scripts/doctor.sh <area>` works before installing `just`. It checks tool
availability/versions and prints remedies; it does not install, compile, or
claim your change passes tests. `docs` and `integrations` do not invoke Rust or
Zig. The scripts work with macOS's Bash 3.2; workflow routing checks additionally
use Python 3.11+ and Node.

### Where the pins actually live

[`mise`](https://mise.jdx.dev/) reads the checked-in `mise.toml`, but that file
is a mirror for shell setup, not the source of truth for everything in it.
`rust-toolchain.toml` remains Cargo/rustup's authoritative Rust input and
`.config/zig-toolchain.json` remains the verified Zig release-and-digest input.
On Linux `flake.nix` installs Zig from those archives rather than nixpkgs'
`zig_0_16`, whose GCC 16 build emits corrupt ELF section symbols that break
the libghostty link; macOS keeps nixpkgs' Zig.
Bun, the usage CLI, and mbx are the exceptions in the other direction: `flake.nix`
reads those pins from `mise.toml` directly and fetches the GitHub release
until nixpkgs matches. The usage CLI is the same 6.12.x train as the
`usage-rs` crate the phux binary parses with.

These are dependency boundaries, not arbitrary directories: `phux-protocol`'s
wire codec and input atoms are pure Rust. Its `server` feature adds libghostty
conversions and rendering/replay helpers. `just crate-check phux-protocol`
checks the former without Zig; `just crate-check phux-protocol server` also
runs the engine-dependent tests. Broaden wire changes to native consumers
before handoff. Cockpit's Phux provider needs the same-checkout FFI;
`just cockpit-test` builds it first.

## Native setup

### Platform packages

Mise and Nix install compilers. They do not replace the host SDK or linker.

**Ubuntu 24.04 / Debian-family Linux (GNU targets):**

```sh
sudo apt-get update
sudo apt-get install -y git curl ca-certificates build-essential mold pkg-config xz-utils
```

`mold` is required by `.cargo/config.toml` for Linux GNU builds. If mold is
unavailable, `RUSTFLAGS='' cargo test --locked -p phux-core` uses the system
linker; use that override consistently to avoid rebuilding artifacts.

**macOS:** For pure Rust, Apple's Command Line Tools are enough. For
libghostty/native FFI, use full Xcode and select it:

```sh
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
xcrun --show-sdk-path
xcrun --find nmedit
brew install pkgconf
```

The two `xcrun` probes must succeed. A CLT-only setup has failed this build.

Inside Nix this does not apply. The dev shell keeps `DEVELOPER_DIR` on its
own Apple SDK and supplies `nmedit` through `cctools`; only Apple's Command
Line Tools are needed, for `xcrun` itself. The shell must not adopt the host
Xcode SDK: nixpkgs' `ld64` predates the `arm64e.x1` TBD targets Xcode 27
ships, and every link in the shell would fail against it.

### Native CI (no Mise, no Nix)

Hosted `native-setup.yml` has neither environment. It uses the rustup and Zig
helpers, which read the same pins:

```sh
bash scripts/setup-rust.sh native
zig_bin="$(bash scripts/install-zig.sh "${XDG_DATA_HOME:-$HOME/.local/share}/phux/toolchains")"
export PATH="$zig_bin:$PATH"
bash scripts/doctor.sh native
just native-smoke
```

`setup-rust.sh` installs the `rust-toolchain.toml` compiler with rustfmt and
clippy and leaves an existing global default alone. Extra components are
opt-in: `web` (wasm32-unknown-unknown), `profiling` (llvm-tools-preview),
`editor` (rust-src + rust-analyzer). `install-zig.sh` verifies the official
archive against `.config/zig-toolchain.json` and installs only under the
directory you supplied. Linux x86_64/aarch64 and macOS arm64 only; Intel macOS
installs Zig by hand from [Zig downloads](https://ziglang.org/download/). Do
not substitute a newer Zig.

`just native-smoke` is that lane locally: core tests, protocol tests including
engine helpers, both CLI builds, and version probes. It is environment
coverage, not a workspace suite.

## Agent integrations

Node 24 LTS with npm, plus Bun for OMP and OpenCode (matching CI). Mise supplies
both; otherwise use the [Node installer](https://nodejs.org) and [Bun installer](https://bun.sh).

```sh
just doctor integrations
just integration-check pi    # or runtime, opencode-v2, omp, claude
```

The Pi and Claude gates include dependency auditing and need network.
OMP and OpenCode gates verify their packed artifacts and native registration.
A prebuilt phux binary is enough for live dogfooding unless you also changed
Rust. Shared helper or version-contract changes should run
`just agent-integrations-check`.

## Browser client

Ordinary client work uses the committed engine binary and needs no Zig. Add the
Rust WASM target (`rustup target add wasm32-unknown-unknown`, or
`bash scripts/setup-rust.sh web`), Node, and the packaging tools. Nix's default
shell already has them.

```sh
cargo install --locked wasm-pack --version 0.15.0
cargo install --locked wasm-bindgen-cli --version 0.2.129
# macOS; Linux equivalent: sudo apt-get install -y binaryen
brew install binaryen
```

`wasm-bindgen-cli` must match the exact `wasm-bindgen` version in the client
manifests; `doctor web` checks this. Browser-rendering tests need Chrome and a
compatible chromedriver. Node-only tests do not.

Regenerating or `--check`ing the engine (`bash scripts/build-vt-wasm.sh`)
installs the official Zig release binary with `scripts/install-zig.sh` under
`PHUX_TOOLCHAIN_DIR` (default `${XDG_DATA_HOME:-$HOME/.local/share}/phux/toolchains`)
and runs it by absolute path, ignoring any Zig on PATH: nixpkgs' `zig_0_16`
links a different LLVM and compiles one function differently. Even the official
compiler occasionally emits different code from identical inputs, so `--check`
retries up to three fresh builds and a regeneration needs two that agree.

```sh
just doctor web
cd clients/phux-web
wasm-pack build --target web --release --out-dir pkg
wasm-pack test --node
```

For browser rendering/e2e tests, start `PHUX_DEMO_PANE=cat cargo run --locked
-p phux-server --example ws_demo_server` from the repository root in a second
terminal (without `PHUX_DEMO_PANE`, its pane is a shell for trying the client).
`clients/phux-vt-web/vendor/ghostty-vt.wasm` is committed; rebuild it with
`bash scripts/build-vt-wasm.sh` (or `--check`). `GHOSTTY_SRC=...` selects local
source and bypasses archive verification but still runs ABI tests.

## GPUIX desktop

Apple-silicon Mac. **Prefer Mise plus host Xcode/Metal for the daily desktop
loop**; keep Nix for separately invoked fully provisioned validation. Both
remain supported, but desktop doctor and builds reject a Nix shell whose Cargo
or Rust compiler is shadowed by Mise, or whose explicit selection is Mise. This
check runs before Apple-toolchain normalization, so doctor cannot hide a mixed
CLI environment. Stale Rust versions also fail before compilation.

The app builds from the pinned GPUIX/Zed source in this
checkout, not from a published addon. One command clones that source, applies
the reviewed patches, builds the native host, builds `phux` from this tree,
starts a local server if needed, and opens the desktop:

```sh
just doctor desktop
just desktop-app
```

`just doctor desktop` verifies that the Metal compiler executes, not merely
that Xcode exists. Install the separate component with
`xcodebuild -downloadComponent MetalToolchain` if needed. The first run is a
from-source native build; later runs reuse it.

See [desktop source toolchain](../clients/desktop/toolchain/README.md) for
source pins, frozen installs, and the inherited Nix SDK caveat. The desktop
build reuses the repository's Apple-toolchain environment helper, so it can
select host Xcode/Metal without mixing in Nix's linker wrappers.

## Cockpit

Apple-silicon Mac, native Rust/Zig/Xcode, Node 24, Python 3:

```sh
just doctor cockpit
just cockpit-test
just cockpit-node-test   # npm ci --ignore-scripts in clients/cockpit first
just cockpit-build
just cockpit-dev
```

The recipes build `phux-client-ffi` from this checkout. See
[Cockpit's guide](../clients/cockpit/README.md) for the isolated development
app versus headless tests versus host-rendering evidence.

### Native SDK live development

The `native dev` loop, automation CLI, and identity-bound diagnostics live in
[Cockpit's guide](../clients/cockpit/README.md#native-sdk-live-development).

## Full root validation

Add `cargo-nextest` to the Mise tools (Nix already has it). Then:

```sh
just doctor ci
just ci-full
```

The full gate uses network services for npm auditing and advisory data;
availability failures are not source-code verdicts. See
[Contributing](../CONTRIBUTING.md#gate-by-gate-local-vs-ci) for the gate map.

Every tool can be present and still not link: a linker that predates the SDK
it is pointed at rejects its stub files, and an `SDKROOT` or `DEVELOPER_DIR`
inherited from another shell is enough to cause it. `just doctor` therefore
links and runs a throwaway binary, so that class of mismatch fails in a
second rather than part-way through a cold build.

## Validation scope

Start with the smallest relevant gate. Expand to downstream crates and clients
when changing shared APIs, protocol, FFI, Cargo inputs, or build scripts.
Formatting a doc does not require `just ci-full`; changing server lifecycle
does require real-server coverage. CLI changes must regenerate reference docs
with `just docs-gen`. Report exactly which commands ran; a scoped pass is not
a full-workspace pass.

Rust versions belong in `rust-toolchain.toml`; Zig archive pins belong in
`.config/zig-toolchain.json`. Update matching Nix packages and this guide when
requirements change. Agent instruction files should link here rather than
carrying independent install/version lists.
