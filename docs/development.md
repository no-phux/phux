---
group: internals
order: 6
---

# Development

How to set up, build, and validate a change. [CONTRIBUTING](../CONTRIBUTING.md)
has the rules; [architecture](./architecture.md) maps the code.

## Choosing an environment

Pick one per shell. Mise (`mise.toml`) pins compilers, runtimes and gate
tools over your native toolchain. Nix (`flake.nix`) brings everything,
including browser toolchains and the SDK, and reproduces every CI lane.

```sh
mise install     # or: nix develop
just doctor      # scope: native (default), docs, core, server, integrations, web, desktop, ci
```

`bash dev/doctor.sh <scope>` works before `just` is installed; it only
checks and prints remedies.

With direnv, `.envrc` loads Nix; an untracked `.envrc.local` containing
`export PHUX_ENV=mise` loads Mise instead. Do not activate one environment
inside the other. Mise has no `cargo-nextest`: install the
[prebuilt binary](https://nexte.st/docs/installation/pre-built-binaries/).

`just toolchain-check` fails when the version pins disagree anywhere.

## Platform packages

Mise and Nix install compilers, not the host SDK or linker. Inside Nix none
of this applies.

```sh
# Ubuntu / Debian (.cargo/config.toml links Linux GNU builds with mold)
sudo apt-get install -y git curl ca-certificates build-essential mold pkg-config xz-utils

# macOS: libghostty and the FFI need full Xcode, selected
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
brew install pkgconf
```

CI's `native setup` lane uses neither Mise nor
Nix; reproduce it with `bash dev/setup-rust.sh native`, Zig from
`dev/install-zig.sh`, and `just native-smoke`.

## libghostty-vt

The terminal engine is Ghostty's C library ([libghostty in phux](./libghostty.md)).
On Linux and macOS (arm64, x86_64) the default static build links a
checksummed prebuilt `libghostty-vt.a`, so it needs no Zig. Other targets or
flavors, offline builds and failed downloads build from source with Zig
0.17.1, cached outside `target/`. `bun tools/main.ts ghostty-pin`
validates the pin and reports prebuilt coverage. `LIBGHOSTTY_VT_SYS_FROM_SOURCE=1`
forces a source build, `GHOSTTY_SOURCE_DIR` builds a local Ghostty checkout,
and `LIBGHOSTTY_VT_SYS_LIB_DIR` links a library you built.

## Build cache (mbx)

Every `cargo` from either environment, and every `just` recipe, runs through
[mbx](https://mr-boxington.jdx.dev): one content-keyed store shared by all
worktrees, a managed `target/` symlink, and a machine-wide compiler pool.
A `cargo` outside both environments bypasses it; use `mise exec -- cargo` or
`nix develop -c cargo`.

Opt out with `PHUX_NO_MBX=1` before `nix develop`, or `mr_boxington = false`
on the Rust tool in an untracked `mise.local.toml`.
Keep crates reusable across worktrees: no compile-time checkout path in
library or test code (read `CARGO_MANIFEST_DIR` at run time); `just guards`
enforces it. Rust `E0514` means the toolchain changed under a build: check
`rustc -V` and `mbx doctor`, then `mbx clean "$PWD"`.

## Pick your work area

| Area | First check |
| ---- | ----------- |
| docs | `just docs-check` |
| pure Rust | `just core-check` |
| server, TUI, CLI, FFI | `just crate-check phux-server` |
| agent integrations | `just integration-check pi` (or runtime, opencode-v2, omp, claude, grok, muse) |
| browser client | `just web-check` |
| Phux Rune desktop | `just rune-check` |
| everything | `just check` |

Run `just doctor <scope>` for the area first.

## Browser client

Client work uses the committed `clients/phux-vt-web/vendor/ghostty-vt.wasm`
and needs no Zig. It needs the Rust wasm target (`bash dev/setup-rust.sh web`),
Node, `wasm-pack`, `binaryen`, and `wasm-bindgen-cli` at the exact version the
client manifests pin; `just doctor web` checks all of them.

```sh
cd clients/phux-web && wasm-pack build --target web --release --out-dir pkg
wasm-pack test --node
```

Browser e2e needs Chrome, a matching chromedriver, and a server:
`PHUX_DEMO_PANE=cat cargo run --locked -p phux-server --example ws_demo_server`.
`bash clients/phux-vt-web/scripts/build-vt-wasm.sh` rebuilds the engine with the pinned Zig
(`--check` verifies the committed one).

## Rune desktop

[Phux Rune](../clients/rune/README.md) is its own Cargo workspace in
`clients/rune`. It builds on Apple silicon macOS and on x86_64/arm64 Linux with
X11 or Wayland and a Vulkan driver (lavapipe works for tests). Debian-family
packages:

```sh
sudo apt-get install -y libfontconfig1-dev libfreetype-dev libxkbcommon-dev \
  libxkbcommon-x11-dev libwayland-dev libx11-dev libxi-dev libxcb1-dev \
  libvulkan-dev mesa-vulkan-drivers
```

Run the app against a disposable dev server on a temp socket, never the
installed one ([Develop](../clients/rune/README.md#develop)).

## Validating a change

Start narrow, then widen with the blast radius:

```sh
just crate-check phux-server      # one crate: fmt, clippy, rustdoc, tests
just test                         # cargo nextest run --workspace
just test-report --base origin/main   # every test to the end; rerun failures on main
just check --dry-run              # the lanes your diff selects against origin/main
just check --preflight            # committed mergeability only, no builds
just check                        # run those lanes: the PR bar
```

`just check` runs the lanes `ci.yml` would run for your diff, through the
same recipes; [CI](./ci.md) explains the routing. A scoped pass is not a
full pass. `just check` first simulates merging your committed branch into
`origin/main` and names conflicting paths; fetch `origin`, merge it into your
branch, and resolve them before spending time on builds. Uncommitted edits
count for routing, not for the merge simulation.

Test processes never inherit `PHUX_*`: `dev/test-env-runner.sh` strips
all but its listed knobs, so add a new test knob there.

Easy to forget:

- A CLI, config, or widget change needs `just docs-gen`; a unit test
  byte-compares `docs/reference/`.
- A design-token change needs `just design-gen` ([design language](./design-language.md)).
- A wire change needs `docs/spec/wire-schema.json` regenerated
  ([protocol](./protocol.md#changing-the-wire)).
- Every commit and the PR title must be a conventional commit (`commitlint`).
- A new `crates/*` directory must be listed in [architecture](./architecture.md).
- Tests read the `.cast` recordings under `docs/assets/`; keep them there.

### Worktrees

Never develop on `main` or in the primary checkout; use a branch in a linked
worktree. Gate recipes lease the worktree to the session that runs them and
refuse another live session with exit 3. `just worktree-release` hands one
on; `PHUX_WORKTREE_TAKEOVER=1` is for a deliberate handoff.

### Running the result

```sh
just build        # target/debug/phux and phux-mcp
just install-dev  # install them to Cargo's bin dir
just rebuild      # install, then hot-swap the dev server, keeping its sessions
```

A development build uses the `dev` profile, with its own socket, state and
server; it refuses the installed release's socket. Never replace the installed
`phux` or `phux upgrade` the production server.

## Performance

`just perf-icount` counts instructions with callgrind over fixed hot paths and
fails the `rust icount` lane when a count rises more than five percent over
`benchmarks/icount-baseline.json`. Run it inside `nix develop` on x86_64;
other platforms' counts are not comparable. To accept a new baseline, run
`just perf-icount --update`, commit the file with the change that justifies
it, and add a `Perf-Baseline: update` commit trailer or the `perf-baseline`
PR label. CI reads the label from the event, so add it before `ci` runs.

Build observability: `just timings -p phux` (one package's Cargo timeline;
compare like with like), `just dep-stats` (the whole lock graph;
`cargo tree --locked -p phux-mcp -e normal,build` for one build),
`just complexity`, `just llvm-lines`, `just bloat`. At run time: `phux perf`, `phux perf trace` and
`phux perf alloc` ([CLI reference](./reference/cli.md)). A new hot path needs
only a `phux_perf` histogram to appear in reports and traces.

## Mutation testing

Opt-in, bounded runs that find tests which would not notice a change.

```sh
just mutation-rust-install                     # pinned cargo-mutants
just mutation-rust --file crates/phux-core/src/window.rs --limit 16
just mutation-rust --in-diff origin/main --list
```

A missed mutant is a place to look, not necessarily a test to add.
