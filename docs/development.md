# Development

How to build, test, and validate a change. [CONTRIBUTING](../CONTRIBUTING.md)
is the short version with the rules; [architecture](./architecture.md) is
where the code lives.

## Choosing an environment

Two supported environments provide the toolchain. Pick one per shell.

|             | Mise (`mise.toml`)                                    | Nix (`flake.nix`)                                                                 |
| ----------- | ----------------------------------------------------- | --------------------------------------------------------------------------------- |
| gives you   | compilers, runtimes, the root gate tools              | everything, including browser toolchains and pinned system libraries              |
| runs        | `just check`, once `cargo-nextest` is installed       | every `just check` lane                                                           |
| first use   | seconds                                               | a dev-shell build                                                                 |
| choose when | you have a working native toolchain and want the pins | you want one command that reproduces any CI lane, or you touch the browser client |

```sh
mise install     # or: nix develop
just doctor      # checks the tools for the native scope; also core, docs, integrations, web, desktop, ci
```

`just` is the command runner for this repo, not a third environment. Recipes
live in `just/*.just`; `just` alone lists them.

With direnv, `.envrc` loads Nix. To load Mise instead, create an untracked
`.envrc.local` containing `export PHUX_ENV=mise` and run `direnv allow`.
Don't activate Mise inside a Nix shell or the other way round; `mise exec`
changes tool resolution but leaves Nix's compiler variables in place.

`just toolchain-check` fails if `mise.toml`, `flake.nix`,
`rust-toolchain.toml`, `.config/zig-toolchain.json`, the Cargo manifests,
and the workflows disagree about a version. `just toolchain-parity`
resolves both environments and compares the binaries they hand you.

`cargo-nextest` is not in Mise's registry. Install the
[prebuilt binary](https://nexte.st/docs/installation/pre-built-binaries/);
Nix provides it.

## libghostty-vt

The terminal engine is Ghostty's C library. What it does in a running phux,
and what it does not, is [libghostty in phux](./libghostty.md). Its build script downloads a
prebuilt, checksummed `libghostty-vt.a` for the pinned Ghostty commit on
the three release targets and needs only `curl` and `tar`. Anything else
(other targets, `LIBGHOSTTY_VT_SYS_FROM_SOURCE=1`, offline, a failed
download) builds it from source with Zig, about 200 seconds once per set
of inputs, cached outside any `target/` directory. `GHOSTTY_SOURCE_DIR`
builds a local Ghostty checkout; `LIBGHOSTTY_VT_SYS_LIB_DIR` links one you
built. `just ghostty-pin-check` confirms the pinned revision publishes an
archive for every target.

## Build cache (mbx)

Both environments run `cargo` through [mbx](https://mr-boxington.jdx.dev).
A phux build is tens of gigabytes and this repo often has several agent
worktrees at once; mbx gives them one content-keyed store (a fresh worktree
restores the whole dependency graph instead of compiling it), a managed
`target/` symlink with a disk budget, and a machine-wide compiler pool.

Plain `cargo`, and every `just` recipe, go through it. `~/.cargo/bin/cargo`
by absolute path does not, nor does a harness whose `PATH` never loaded
either environment; use `mise exec -- cargo ...` or `nix develop -c cargo ...`.

```sh
mbx doctor            # tools, cache access, link mode
mbx stats             # savings, cross-worktree sharing
mbx explain --last    # why the last build hit, missed, or bypassed
mbx clean "$PWD"      # this checkout's managed target only
```

Opt out with `PHUX_NO_MBX=1` before `nix develop`, or
`[tools] rust = { version = "<pin>", mr_boxington = false }` in an untracked
`mise.local.toml`.

Two rules keep crates reusable across worktrees, both enforced by
`just cache-portable-check`: no compile-time checkout path in library or
test code (read `CARGO_MANIFEST_DIR` at run time), and the Zig engine
build stays outside `target/`. Rust `E0514` means the shell's toolchain
changed under a build: check `rustc -V`, `cargo -V`, `mbx doctor`, then
`mbx clean "$PWD"`.

## Pick your work area

| Area                          | Needs                                                         | First check                                                                        |
| ----------------------------- | ------------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| docs                          | Git, Bash                                                     | `just docs-check`                                                                  |
| pure Rust: domain, wire codec | Rust, a linker                                                | `just core-check` or `just crate-check phux-protocol`                              |
| server, TUI, CLI, FFI         | Rust, Zig, platform packages                                  | `just doctor native`, then `just crate-check phux-server`                          |
| agent integrations            | Node 24, Bun                                                  | `just integration-check pi` (or `runtime`, `opencode-v2`, `omp`, `claude`, `grok`) |
| browser client                | Rust wasm target, Node, wasm tools                            | `just doctor web`                                                                  |
| desktop                       | Apple silicon Mac with Xcode, or Linux with desktop libraries | `just doctor desktop`, then `just desktop-app`                                     |
| everything                    | all of the above plus gate tools                              | `just doctor ci`, then `just check`                                                |

`bash scripts/doctor.sh <area>` works before `just` is installed. It checks
and prints remedies; it installs nothing.

`phux-protocol`'s codec is pure Rust; its `server` feature adds the
libghostty conversions. `just crate-check phux-protocol` needs no Zig;
`just crate-check phux-protocol server` runs the engine-dependent tests too.

## Platform packages

Mise and Nix install compilers, not the host SDK or linker.

Ubuntu 24.04 / Debian:

```sh
sudo apt-get install -y git curl ca-certificates build-essential mold pkg-config xz-utils
```

`.cargo/config.toml` uses `mold` for Linux GNU builds.

macOS: Apple's Command Line Tools are enough for pure Rust. libghostty and
the FFI need full Xcode, selected:

```sh
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
xcrun --show-sdk-path && xcrun --find nmedit
brew install pkgconf
```

Inside Nix none of this applies; the shell brings its own SDK and must not
inherit the host Xcode's.

`just doctor` links and runs a throwaway binary, so a linker that predates
the SDK it points at, or an `SDKROOT` inherited from another shell, fails in
a second instead of twenty minutes in.

### Native CI

The `native setup` lane in `ci.yml` has neither Mise nor Nix. It uses the same pins through
two helpers, which also work locally:

```sh
bash scripts/setup-rust.sh native      # rustup toolchain from rust-toolchain.toml
zig_bin="$(bash scripts/install-zig.sh "${XDG_DATA_HOME:-$HOME/.local/share}/phux/toolchains")"
export PATH="$zig_bin:$PATH"
just native-smoke
```

## Browser client

Ordinary client work uses the committed `clients/phux-vt-web/vendor/ghostty-vt.wasm`
and needs no Zig. You need the Rust wasm target (`bash scripts/setup-rust.sh web`),
Node, `wasm-pack`, `wasm-bindgen-cli` at exactly the version the client
manifests pin (`just doctor web` tells you), and `binaryen`. Nix's shell
has all of them.

```sh
just doctor web
cd clients/phux-web && wasm-pack build --target web --release --out-dir pkg
wasm-pack test --node
```

Browser e2e tests need Chrome and a matching chromedriver, and a server:
`PHUX_DEMO_PANE=cat cargo run --locked -p phux-server --example ws_demo_server`
from the repo root. `bash scripts/build-vt-wasm.sh` rebuilds the engine
(`--check` verifies it) with the pinned Zig release, installed by
`scripts/install-zig.sh`, never the one on `PATH`.

## GPUI desktop

Apple silicon macOS, or x86_64/arm64 Linux with X11 or Wayland and a Vulkan
driver (Mesa's lavapipe works for tests). On Debian-family Linux:

```sh
sudo apt-get install -y libfontconfig1-dev libfreetype-dev libxkbcommon-dev \
  libxkbcommon-x11-dev libwayland-dev libx11-dev libxi-dev libxcb1-dev \
  libvulkan-dev mesa-vulkan-drivers
```

On macOS use Mise plus the host Xcode for the daily loop; the desktop
gates refuse a Nix shell whose compiler is shadowed by Mise. The desktop
supports macOS 26+ and full Xcode 26+ compatible with the host OS.
`bash scripts/setup-macos-desktop.sh` detects the host version and selected
Xcode (or an installed `/Applications/Xcode*.app` when needed), downloads
only a missing matching Metal component, and verifies that it executes.
Rerunning it is a no-op when Metal works. An explicit `DEVELOPER_DIR` selects
that Xcode or fails rather than silently using another. It never installs
Xcode, changes the global `xcode-select` setting, or replaces the installed app.
Builds perform this setup automatically and enforce a macOS 26.0 deployment
target even on newer hosts; `doctor desktop` only checks, never installs.

```sh
just doctor desktop
just desktop-app      # build the app and the CLI from this tree, start a server, open it
```

[`clients/desktop`](../clients/desktop) has the app's own commands and gates.

For native MSVC builds, isolated WSL coordinator paths, and qualification,
see [Windows desktop development](./windows-desktop-development.md).

## Archived Zig Cockpit

The Zig Cockpit is tabled, not a supported build or install on `main`.
Published recovery refs are branch `archive/zig-cockpit` and annotated tag
`archive/zig-cockpit-2026-10-14`. The GPUI desktop is the maintained app.
Use the [snapshot recovery guide](https://github.com/no-phux/phux/blob/3cd9ddac22075c1b3db2674e2edc4e00c91d67f4/docs/consumers/cockpit.md)
in a separate worktree. Its SDK, fixtures and hooks belong to that snapshot;
never restore retired CI, installers or publication without an explicit
revival decision and same-checkout protocol/FFI and isolated app evidence.
Shared FFI, mobile bindings and Zig for libghostty remain supported.

## Validating a change

Start narrow, then widen with the blast radius:

```sh
just check --dry-run              # lanes selected against origin/main
just check --preflight            # committed branch vs fetched origin/main; no builds
just check                        # run those lanes: the PR bar
just crate-check phux-server      # scoped fmt, clippy, rustdoc, tests
just test                         # cargo nextest run --workspace
```

`just check` uses the same routing and recipes as `ci.yml`. Guards run on
every change; Rust lint, unit, e2e, browser, desktop and integration lanes
run when their inputs change. There is no run-everything recipe. `rust
icount` and `native setup` are CI-only; stress and real-GPU desktop
qualification are local. A scoped pass is not a full CI pass.

Fetch `origin` before the preflight when other agents are landing changes.
`just check` checks committed mergeability first and reports conflicting paths
without changing the checkout or index. Merge `origin/main` into your feature
worktree and resolve those paths before spending time on builds. A stale branch
that merges cleanly can proceed; a base update alone does not require a rebase.
Uncommitted edits count for lane routing but are not part of the merge simulation.
During a merge, resolve and stage all conflicted paths, then `just check` can
validate the working tree before you commit. The committed merge simulation is
deferred until the merge commit exists; `--preflight` reports that explicitly.

Scoped gates for other surfaces: `just integration-check <pkg>`,
`just desktop-check`, `just tools-check` (the Bun package under `tools/`,
including the CI routing table), `just guards` (workflow syntax, pins,
routing truth table, shell scripts, docs, and install contracts).

Things that fail CI and are easy to forget:

- A CLI, config, or widget change without `just docs-gen`: the generated
  pages under `docs/reference/` are byte-compared by a unit test.
- A wire change without regenerating `docs/spec/wire-schema.json`
  ([protocol](./protocol.md#changing-the-wire)).
- A commit message that is not a conventional commit. `commitlint` checks
  every commit in the PR and the title.
- A new `crates/*` directory not listed in [architecture](./architecture.md).

Real-PTY tests set `PHUX_NO_UPDATE_CHECK=1` in the child environment so an
update prompt cannot eat a scripted keystroke. The shared spawn path seals
that opt-out after `env_clear`, and the variable also suppresses a cached
notice.

### Worktrees

Never develop on `main` or in the primary checkout; make a branch and a
linked worktree. The gate recipes lease the worktree to the session that
runs them and refuse another live session with exit 3, so one agent
session per worktree. `just worktree-release` hands one on;
`PHUX_WORKTREE_TAKEOVER=1` is for a deliberate handoff.

### Running the result

```sh
just build        # target/debug/phux and phux-mcp
just install-dev  # install them to Cargo's bin dir
just rebuild      # install, then hot-swap the dev server, keeping its sessions
```

A development build runs under the `dev` profile: its own socket, state,
and server. It refuses to talk to the installed release's socket. Never
copy a build over the installed `phux`, point a build at the production
socket, or `phux upgrade` the production server; verify against the dev
server or an explicit `--socket` under a temp directory.

## CI

One classifier, `bun tools/main.ts detect-changes` over
[`tools/src/routing/routes.ts`](../tools/src/routing/routes.ts), decides
which lanes a change can affect. `ci.yml` owns all these lanes. Unknown
paths fail closed and run everything; handwritten docs normally run only
guards. Docs read by tests, including `docs/agents.md` and generated
references, also select their Rust consumers. A library change runs the
tests of its dependents. The required `ci`
aggregate rejects a failed or cancelled lane. `just tools-check` tests the
routes.

The test lanes compile once (`just test-archive`) and run the unit halves
and e2e from that nextest archive. Pull requests restore caches and never
save them; `main` pushes save one entry per lane under a 10 GB repository
budget, pruned by `prune-build-cache`. Runners are the free hosted ones:
Ubuntu x64 and arm64, and macOS 26 plus the `xcode-27` image. Desktop releases
build on macOS 26 (the supported floor); PRs qualify both Apple toolchains.
The desktop lane shares a handful of macOS slots with release builds.

Workflow files carry their own comments; `.github/workflows/` is the
reference. `just guards` lints them.

## Performance

`just perf-icount` counts instructions with callgrind on a fixed set of hot
paths (VT apply, frame encode and decode, agent stream append, kernel
history slice, PTY batch copy, one organization commit) and fails the
`perf-icount` CI lane when a count rises more than five percent over
`benchmarks/icount-baseline.json`. Run it inside `nix develop` on x86_64;
counts from another libc or architecture are not comparable.

To accept a new baseline, run `just perf-icount --update` there, commit the
file with the change that justifies it, and either add the trailer
`Perf-Baseline: update` to a commit or the `perf-baseline` label to the PR
before `ci` runs. The lane refuses an unauthorized baseline edit in
seconds.

`just timings`, `just llvm-lines`, `just bloat`, and `just dep-stats` are
the local build-observability recipes. For the server at run time,
`phux perf` reads its counters, and the `tokio-console` and `dhat-heap`
features in `phux-server` and `phux` attach the usual profilers.

`phux perf trace --seconds N` records every timed span in the server for N
seconds and writes Chrome trace-event JSON under the server's state
directory (`traces/`, beside `server.log`); open it in
<https://ui.perfetto.dev>. `phux perf alloc on|off` switches heap
accounting, which then shows as `heap` in `phux perf`. A server started
with `PHUX_DEV=1` has heap accounting on from the start. Both use
`PERF_CONTROL`, gated on `features_ext.PERF_CONTROL`.
It is restricted to the ordinary owner socket, not delegated or remote
grants. An abandoned server capture stops and saves after ten minutes;
the server retains the newest 20 trace files. Exports use exclusive,
owner-readable files so simultaneous captures cannot overwrite each other.

`PHUX_DEV=1` is the dev-mode switch every phux process reads
(`phux_perf::dev`). The desktop also has a saved **Developer mode** setting
(**Settings > Developer**) that does the same without a restart. In the
desktop, Developer mode also turns on the performance HUD and
a dev socket, `desktop-<pid>.dev.sock` beside the server socket, that
answers line JSON (`clients/desktop/native/src/devtools/socket.rs`).
`phux perf --desktop [--watch S] [--json]` reads it, `phux perf trace
--desktop` records the desktop and server onto one Perfetto timeline, and
`phux perf hud on|off` toggles the HUD. Every `phux_perf` histogram becomes
a trace span with no extra code, so a new hot path needs only a histogram
in its crate's `TABLE` to show in reports, traces and the HUD.
The dev socket verifies the connecting UID and requires a non-symlink,
owner-only parent directory; `PHUX_DESKTOP_DEV_SOCKET` overrides its path
but does not relax those checks. Discovery checks the associated server
socket, including when several server sockets share a directory. Development
builds retain the production-socket guard, including
explicit dev-socket overrides and desktop calls through an installed CLI;
an override must identify the same permitted server before dispatch.
Requests are limited to 64 KiB, 16 concurrent connections, and five-second I/O
deadlines; a desktop trace records at most 60 seconds. `--reset` returns
each snapshot before clearing its counters, including in watch mode.

## Mutation testing

Regression tests belong in the ordinary suite; mutation runs are a bounded,
opt-in way to find tests that would not notice a change.

```sh
just mutation-rust-install                                     # pinned cargo-mutants
just mutation-rust                                             # default scope
just mutation-rust --file crates/phux-core/src/window.rs --limit 16
just mutation-rust --in-diff origin/main --list
```

A caught mutant is evidence for that test; a missed one is a place to look,
not necessarily a test to add. Unviable mutants and timeouts are not
evidence of anything.

## Demos and recordings

`scripts/demo-setup.sh` stages a demo session and prints the runbook.
`scripts/demo-record.sh` records `docs/assets/recording-demo.cast` and
renders the GIF, under a 1 MiB budget because GitHub serves the whole file
before the first frame. `scripts/demo-pi-setup.sh` sets up the pi fleet
demo. The `.cast` files under `docs/assets/` are read by tests in
`phux-record` and `phux`, so re-record them deliberately.
