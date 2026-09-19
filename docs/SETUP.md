---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-19
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
how you get a compiler. Do not add a third bootstrap (`setup-rust.sh` /
`install-zig.sh`) unless you are on the native CI path below.

With [direnv](https://direnv.net), `.envrc` loads Nix by default. To make Mise
the environment it loads, create an untracked `.envrc.local`:

```sh
echo 'export PHUX_ENV=mise' > .envrc.local && direnv allow
```

If you already activate Mise in your shell, you do not need direnv. `rustc` and
`zig` from mise shims are the Mise path.

**They are held together, not merely documented as similar.**
`just toolchain-check` is a `just ci` gate that reads `mise.toml`, `flake.nix`,
`rust-toolchain.toml`, `.config/zig-toolchain.json`, the Cargo manifests, the
workflows, and the container builders and fails if any of them names a
different Rust, Zig, Node, Bun, or usage CLI. `just toolchain-parity` is the
runtime half: it resolves both environments and compares the binaries they
actually hand you. Run it after bumping a pin or `flake.lock`.

`cargo-nextest` is the one root gate tool Mise does not supply: it has no
prebuilt entry in Mise's registry, and building it from source can require a
newer compiler than this repository's Rust pin. Install the
[official prebuilt binary](https://nexte.st/docs/installation/pre-built-binaries/);
`bash scripts/doctor.sh ci` reports it missing. Browser, Cockpit, and
build-observability tools are Nix-or-native; `mise.toml` lists them under
"deliberately absent".

## Pick your work area

Run commands from the repository root. `just` is a convenience command runner;
the shell, Cargo, and npm commands below also work directly.

| Work area | Prerequisites | First check |
|---|---|---|
| Handwritten docs | Git, Bash, standard Unix utilities | `bash scripts/check-docs.sh` |
| Pure Rust domain / wire codec | Rust, native compiler/linker | `just core-check` or `just crate-check phux-protocol` |
| Server, TUI, CLI, engine-dependent protocol helpers, FFI | Rust, Zig, platform packages | `just doctor native`, then `just crate-check phux-server` |
| Agent integrations | Node/npm | `just integration-check pi` (or `opencode`, `claude`) |
| Browser client | Rust WASM target, Node, WASM tools (engine binary is committed) | `just doctor web`; see [Browser client](#browser-client) |
| Cockpit app | Apple-silicon Mac, SDK, Zig, Node, Rust FFI, Python | `just doctor cockpit`, then `just cockpit-test` |
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
Bun and the usage CLI are the exceptions in the other direction: `flake.nix`
reads those pins from `mise.toml` directly and fetches the GitHub release
until nixpkgs matches. The usage CLI is the same 6.9.x train as the
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
This host requirement also applies inside Nix.

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

Node 24 LTS with npm (matching CI). Mise supplies Node; otherwise
`brew install node@24` or the [official installer](https://nodejs.org).

```sh
just doctor integrations
just integration-check pi    # or runtime, opencode, claude
```

Gates include dependency auditing and need network. A prebuilt phux binary is
enough for live dogfooding unless you also changed Rust. Shared helper or
version-contract changes should run `just agent-integrations-check`.

## Browser client

Ordinary client work uses the committed engine binary and needs no Zig. Add the
Rust WASM target (`rustup target add wasm32-unknown-unknown`, or
`bash scripts/setup-rust.sh web`), Node, and the packaging tools. Nix's default
shell already has them.

```sh
cargo install --locked wasm-pack --version 0.15.0
cargo install --locked wasm-bindgen-cli --version 0.2.128
# macOS; Linux equivalent: sudo apt-get install -y binaryen
brew install binaryen
```

`wasm-bindgen-cli` must match the exact `wasm-bindgen` version in the client
manifests; `doctor web` checks this. Browser-rendering tests need Chrome and a
compatible chromedriver. Node-only tests do not.

Regenerating or `--check`ing the engine (`bash scripts/build-vt-wasm.sh`) needs
the official Zig release binary from `bash scripts/install-zig.sh` ahead of any
other Zig on PATH: nixpkgs' `zig_0_16` on x86_64 Linux links a different LLVM
and compiles one function differently, so the Nix shell's Zig does not
reproduce the committed engine there.

```sh
just doctor web
cd clients/phux-web
wasm-pack build --target web --release --out-dir pkg
wasm-pack test --node
```

For browser rendering/e2e tests, start `cargo run --locked -p phux-server
--example ws_demo_server` from the repository root in a second terminal.
`clients/phux-vt-web/vendor/ghostty-vt.wasm` is committed; rebuild it with
`bash scripts/build-vt-wasm.sh` (or `--check`). `GHOSTTY_SRC=...` selects local
source and bypasses archive verification but still runs ABI tests.

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
