---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-05
---

# Contributor setup

**TL;DR.** Pick the area you are changing and install only its prerequisites.
Native tools and the Nix shell run the same build and test commands. Rust and
Zig use repository pins; Nix additionally pins system dependencies. Start with
a scoped check, then expand validation for shared code. Browser-client work
uses the committed engine binary; rebuilding it uses verified pinned source.

## Pick your work area

Run commands from the repository root. `just` is a convenience command runner;
the shell, Cargo, and npm commands below also work directly.

| Work area | Prerequisites | First check |
|---|---|---|
| Handwritten docs | Git, Bash, standard Unix utilities | `bash scripts/check-docs.sh` |
| Pure Rust domain / wire codec | Rust, native compiler/linker | `just core-check` or `just crate-check phux-protocol` |
| Server, TUI, CLI, engine-dependent protocol helpers, FFI | Rust, Zig, platform packages | `just doctor native`, then `just crate-check phux-server` (choose the affected crate) |
| Agent integrations | Node/npm | `just integration-check pi` (or `opencode`, `claude`) |
| Browser client | Rust WASM target, Node, WASM tools (engine binary is committed) | `just doctor web`; see [Browser client](#browser-client) |
| Cockpit app | Apple-silicon Mac, SDK, Zig, Node, Rust FFI, Python | `just doctor cockpit`, then `just cockpit-test` |
| Full root validation | Native prerequisites plus gate tools | `just doctor ci`, then `just ci-full` |

`bash scripts/doctor.sh <area>` works before installing `just`. It checks tool
availability/versions and prints remedies; it does not install, compile, or
claim your change passes tests. `docs` and `integrations` do not invoke Rust or
Zig. The scripts work with macOS's Bash 3.2; workflow routing checks additionally
use Python 3.11+ and Node.

These are dependency boundaries, not arbitrary directories: `phux-protocol`'s
wire codec and input atoms are pure Rust. Its `server` feature adds libghostty
conversions and rendering/replay helpers. `just crate-check phux-protocol`
checks the former without Zig; `just crate-check phux-protocol server` also
runs the engine-dependent tests. The feature argument applies to Clippy,
rustdoc, and tests alike. Broaden wire changes to native consumers before
handoff; the full root Clippy/rustdoc gates compile all features. Cockpit's
Phux provider needs the same-checkout FFI; `just cockpit-test` builds it first.

## Native setup

### Platform packages

**Ubuntu 24.04 / Debian-family Linux (GNU targets):**

```sh
sudo apt-get update
sudo apt-get install -y git curl ca-certificates build-essential mold
# Add these for the native terminal / FFI / browser engine:
sudo apt-get install -y pkg-config xz-utils
# Optional command runner:
sudo apt-get install -y just
```

`mold` is required by the checked-in `.cargo/config.toml` for x86_64/aarch64
Linux GNU builds, including build scripts in otherwise small Rust packages.
Other distributions can install equivalent packages. For an environment where
mold is unavailable, `RUSTFLAGS='' cargo test --locked -p phux-core` overrides
the repository flags and uses the system linker; use that override consistently
to avoid rebuilding artifacts. The doctor checks the standard mold setup.

**macOS:**

For pure Rust work, install Apple's Command Line Tools (`xcode-select --install`)
if a compiler is not already installed. For libghostty/native FFI builds, use
full Xcode and select it:

```sh
sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
xcrun --show-sdk-path
xcrun --find nmedit
brew install pkgconf just
```

The two `xcrun` probes must succeed. The pinned Ghostty Darwin build needs SDK
discovery and `nmedit`; a CLT-only setup has failed this build. This host
requirement also applies inside Nix. If `DEVELOPER_DIR`/`SDKROOT` came from a
previous shell, open a clean native shell before diagnosing Xcode discovery.

### Rust

Install [rustup](https://rustup.rs) once, choosing the minimal profile. Then:

```sh
bash scripts/setup-rust.sh core
cargo test --locked -p phux-core
```

The helper reads `rust-toolchain.toml`, installs that compiler with rustfmt and
clippy, and preserves an existing global default. Cargo also discovers the pin
automatically. Neither path installs WASM, LLVM tools, or editor components by
default. Add them only when needed:

```sh
bash scripts/setup-rust.sh web        # wasm32-unknown-unknown
bash scripts/setup-rust.sh profiling  # llvm-tools-preview
bash scripts/setup-rust.sh editor     # rust-src + rust-analyzer
```

Cargo may fetch workspace dependency metadata, including Git dependencies,
even for a narrow `-p` build. A network connection is needed on first use, but
that does not mean Cargo compiles the other workspace members.

### Zig and the native terminal

If `zig version` already matches the release pin, keep using it. Otherwise:

```sh
zig_bin="$(bash scripts/install-zig.sh "${XDG_DATA_HOME:-$HOME/.local/share}/phux/toolchains")"
export PATH="$zig_bin:$PATH"
bash scripts/doctor.sh native
cargo build --locked -p phux -p phux-mcp
```

The helper reads the version and SHA-256 digests from
`.config/zig-toolchain.json`, verifies the official download before
extracting it, and installs only under the directory you supplied. Repeating it
reuses that compiler. It does not change shell startup files or replace a
system Zig. Keep the printed directory on PATH in subsequent shells.

The helper supports the release hosts: Linux x86_64/aarch64 and macOS arm64.
On Intel macOS, install the matching official Zig archive manually from
[Zig downloads](https://ziglang.org/download/) and run the doctor; there is no
release-matrix checksum for that host. Do not substitute an arbitrary newer
Zig: the native dependency requires the exact compiler version.

`just native-smoke` runs the same setup smoke as native Linux CI: core tests,
protocol tests including engine helpers (`--features server`), both CLI builds,
and version probes.
It exercises the actual libghostty build and native linker. It does not replace
workspace or ignored real-server tests.

### Agent integrations

Use Node 24 LTS with npm (matching CI). Install it with your existing Node
manager, the [official installer](https://nodejs.org), or Homebrew:

```sh
brew install node@24
export PATH="$(brew --prefix node@24)/bin:$PATH"
```

Then, without Rust, Zig, or Nix:

```sh
bash scripts/doctor.sh integrations
npm --prefix integrations/pi ci
npm --prefix integrations/pi run gates
# Equivalent: just integration-check pi
```

Substitute `opencode` or `claude` for the other packages. Their lockfiles supply
host SDK/CLI test dependencies. Gates include dependency auditing and need
network access. A prebuilt phux binary is sufficient for live integration
dogfooding unless you also changed Rust. Changes to shared integration helpers
or version contracts should run `just agent-integrations-check` for all three.

### Browser client

Add the Rust WASM target using `setup-rust.sh web`, Node, and the packaging
tools. Ordinary client work uses the committed engine binary and needs no Zig:

```sh
cargo install --locked wasm-pack --version 0.15.0
cargo install --locked wasm-bindgen-cli --version 0.2.121
# macOS; Linux equivalent: sudo apt-get install -y binaryen
brew install binaryen
```

`wasm-bindgen-cli` must match the exact `wasm-bindgen` version in the client
manifests; `doctor web` checks this. Browser-rendering tests additionally need
Chrome and a compatible chromedriver. Node-only tests do not need a browser.
Regenerating or `--check`ing the engine (`bash scripts/build-vt-wasm.sh`) needs
the official Zig release binary from `bash scripts/install-zig.sh` ahead of any
other Zig on PATH: nixpkgs' `zig_0_16` on x86_64 Linux links a different LLVM
and compiles one function differently, so the Nix shell's Zig does not
reproduce the committed engine there.

```sh
bash scripts/doctor.sh web
cd clients/phux-web
wasm-pack build --target web --release --out-dir pkg
wasm-pack test --node
# With the demo server running (see below) and a matching Chrome/ChromeDriver:
wasm-pack test --headless --chrome
```

For browser rendering/e2e tests, start the seeded server in a second terminal
from the repository root, and stop it when the tests finish:

```sh
cargo run --locked -p phux-server --example ws_demo_server
```

This server build adds the native prerequisites. The Node tests need no server.
If your installed Chrome differs from Nix's ChromeDriver, supply a matching
driver with `wasm-pack test --headless --chrome --chromedriver /path/to/chromedriver`.

**Engine regeneration:** `clients/phux-vt-web/vendor/ghostty-vt.wasm` is
committed. To rebuild it, install the pinned Zig above, Node, curl, tar, and a
SHA-256 utility (`sha256sum` or macOS's `shasum`), then run from the root:

```sh
bash scripts/build-vt-wasm.sh          # fetch, verify, build, ABI-test, replace
bash scripts/build-vt-wasm.sh --check  # rebuild and compare without replacing
```

The script owns the immutable commit and archive SHA-256 from the public
`phall1/ghostty` standalone checkpoint-WASM branch. It checks source integrity
before extracting, uses stripped ReleaseSafe output with fixed version metadata,
and runs that revision's Node ABI/corruption corpus before replacing anything.
The `web-check` CI lane checks byte-for-byte regeneration, runs the Rust adapter
and Node session tests, and builds the shipping browser package. Updating
the source pin requires updating its digest and regenerating the binary together.
`GHOSTTY_SRC=/path/to/compatible/ghostty` explicitly selects local source for
engine development; this bypasses archive verification but still runs ABI tests.

### Cockpit

Use the native Rust/Zig/macOS setup above, Node 24, and Python 3. From the root:

```sh
brew install python
bash scripts/doctor.sh cockpit
just cockpit-test
just cockpit-build
just cockpit-dev
```

The recipes build `phux-client-ffi` from this checkout. The Zig build supplies
the pinned Native SDK and runs the shipping TypeScript compilation; npm is
available through Node. See [Cockpit's guide](../clients/cockpit/README.md) for
the isolated development app and the distinction between headless tests and
actual host-rendering evidence.

#### Native SDK live development

Use the SDK's [native dev](https://native-sdk.dev/docs/cli/dev) loop for UI
iteration. Cockpit uses the fork pinned in `clients/cockpit/build.zig.zon`;
build its matching CLI once rather than using an unrelated npm CLI:

```sh
just cockpit-ffi
cd clients/cockpit
eval "$(./scripts/build-automation-cli.sh --export)"
export ZIG_GLOBAL_CACHE_DIR="$PWD/.zig-global-cache"
mkdir -p .dev-run/native-real
touch .dev-run/native-real/config
export PHUX_COCKPIT_CONFIG="$PWD/.dev-run/native-real/config"
export PHUX_COCKPIT_STATE="$PWD/.dev-run/native-real/layout.json"
export PHUX_SOCKET="/tmp/phux-$USER/phux.sock"
"$NATIVE" dev -Dautomation=true -Dphux-enabled=true \
  -Dphux-client-ffi-profile=ffi-dev \
  -Dphux-client-ffi-include-dir="$PWD/../../crates/phux-client-ffi/include" \
  -Dphux-client-ffi-lib-dir="$PWD/../../target/ffi-dev"
```

Set `PHUX_SOCKET` to the endpoint reported by your running server's
`phux status --json`; the example uses the normal local endpoint when
`XDG_RUNTIME_DIR` is unset. Set `PHUX_SESSION` to select a particular session.
For an isolated server, use its socket and a separate config/layout pair.
These settings choose the connection without restarting the server.

Keep the working directory at `clients/cockpit`: registered fragment paths
start with `src/`. Native's Debug default enables their hot-reload watcher;
ReleaseSafe disables it. Markup edits reload in the running window. TypeScript,
Zig, and FFI changes need a rebuild/relaunch; rerun the command, rebuilding
`cockpit-ffi` first if Rust changed. Zig reuses unchanged build steps. To run
the existing binary without a build, execute `./zig-out/bin/phux-cockpit` from
this same directory and environment. At the current pin, `native dev --binary`
is the WebView frontend workflow and refuses Cockpit with `MissingFrontend`.

In a second shell at the same app root, use the matching CLI's
[`automate`](https://native-sdk.dev/docs/automation) commands: `snapshot`,
`assert`, and `profile on`. Verify `publisher_pid` against the launched process
and `markup_watch=armed` before live editing. The SDK also provides `provenance`
and `edit`, but Cockpit's composed toolbar currently reports `authored=zig`,
so source write-back is unavailable there; edit the `.native` source directly.
This direct SDK run uses the ordinary app name; distinguish it by executable
path and PID. The separate-name bundled runner remains available for
release-mode acceptance.

### Mutation testing

Mutation tools are optional and separate from contributor and ordinary CI
gates. See [Scoped mutation testing](TESTING_MUTATIONS.md) for pinned runners,
bounded scopes, report interpretation, and the Rust/FFI/Zig evidence boundary.

### Full root validation

Add `just`, `cargo-nextest`, `cargo-deny`, Bash 4+, `actionlint`, `shellcheck`,
`jq`, Python 3, curl, and Node 24 to the native tools. With Homebrew:

```sh
brew install just cargo-nextest cargo-deny bash actionlint shellcheck jq python node@24
export PATH="$(brew --prefix node@24)/bin:$(brew --prefix)/bin:$PATH"
bash scripts/doctor.sh ci
just ci-full
```

On Linux, standard packages supply Bash, jq, Python, curl and ShellCheck.
Use the official [nextest](https://nexte.st/docs/installation/pre-built-binaries/),
[cargo-deny](https://github.com/EmbarkStudios/cargo-deny/releases), and
[actionlint](https://github.com/rhysd/actionlint/releases) prebuilt binaries for
fast installs. Their current source releases may need a newer compiler than
the project's Rust pin; installing auxiliary binaries avoids rebuilding tools
or changing the application compiler.

The full gate spans multiple areas. It also uses network services for npm
auditing and advisory data; availability failures are not source-code verdicts.
See [Contributing](../CONTRIBUTING.md#gate-by-gate-local-vs-ci) for the gate map.

## Nix: fully provisioned environment

```sh
nix develop             # or direnv allow, if you use direnv
just doctor ci
just ci-full
```

The default shell retains the browser, editor, profiling, and gate tools,
including Node/npm. It reads the same Rust pin and explicitly adds the optional
Rust components. Nix additionally locks system packages; native builds use
your platform's SDK/libraries. Both paths use the same Cargo/npm lockfiles and
test recipes. Neither can remove the cold libghostty compilation cost when
you actually change code that depends on it.

## Validation scope and maintenance

Start with the smallest relevant gate. Expand to downstream crates and clients
when changing shared APIs, protocol, FFI, Cargo inputs, or build scripts.
Formatting a doc does not require `just ci-full`; changing server lifecycle
does require real-server coverage. CLI changes must regenerate reference docs
with `just docs-gen`. Report exactly which commands ran and which broader
checks remain for CI; a scoped pass is not a full-workspace pass.

Native Linux CI runs these same setup helpers without Nix or a target cache.
Cockpit CI uses the same Rust/Zig helpers and doctor on macOS, then its existing
FFI tests and production app build. Setup regression tests (`just setup-check`)
exercise missing/wrong tools, SDK errors, optional Rust components, and compiler
checksum rejection without downloading tools. The existing Nix lanes retain
the full root test bar.

Rust versions belong in `rust-toolchain.toml`; Zig archive pins belong in the
release matrix and remain checked by `just zig-pin-check`. Update matching Nix
packages and this guide when requirements change. Agent instruction files
should link here, rather than carrying independent install/version lists.
