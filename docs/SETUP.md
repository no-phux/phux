---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-09
---

# Contributor setup

**TL;DR.** Choose an environment (Nix or Mise), then pick the area you are
changing and install only its prerequisites. Both environments run the same
build and test commands and are held to the same tool versions by
`just toolchain-check` and `just toolchain-parity`. Start with a scoped check,
then expand validation for shared code. Browser-client work uses the committed
engine binary; rebuilding it uses verified pinned source.

## Choosing an environment

Two are supported. They are not tiers of the same thing: Nix provisions every
tool this repository uses, Mise provisions the toolchains and lets your own
system supply the rest.

| | Nix (`flake.nix`) | Mise (`mise.toml`) |
|---|---|---|
| Provides | Compilers, every root gate tool, browser and Cockpit toolchains, observability and debugging tools, pinned system libraries | Compilers and runtimes, plus the root gate tools |
| Runs | Everything, including `just ci-full`, the browser lanes, and Cockpit | `just ci`, once `cargo-nextest` is installed separately |
| Costs | A dev-shell build on first use | Seconds; per-tool downloads |
| Choose it when | You want one command to reproduce any lane, or you are touching the browser client, Cockpit, or release infrastructure | You already run a working native toolchain and want the pins managed without adopting Nix |

```sh
nix develop             # Nix: the fully provisioned shell
mise install            # Mise: toolchains and gate tools
```

With [direnv](https://direnv.net), `.envrc` loads Nix by default. To make Mise
the environment it loads, create an untracked `.envrc.local`:

```sh
echo 'export PHUX_ENV=mise' > .envrc.local && direnv allow
```

**They are held together, not merely documented as similar.**
`just toolchain-check` is a `just ci` gate that reads `mise.toml`, `flake.nix`,
`rust-toolchain.toml`, `.config/zig-toolchain.json`, the Cargo manifests, the
workflows, and the container builders and fails if any of them names a
different Rust, Zig, Node, or Bun. `just toolchain-parity` is the runtime
half: it resolves both environments and compares the binaries they actually
hand you. Run it after bumping a pin or `flake.lock` — a static check cannot
see nixpkgs quietly resolving a different release than the one `mise.toml`
pins, which is how the Nix shell came to ship Bun 1.3.13 while `mise.toml`,
`@types/bun`, and the site's production builder were all on 1.4.0.

`cargo-nextest` is the one root gate tool Mise does not supply: it has no
prebuilt entry in Mise's registry, and building it from source can require a
newer compiler than this repository's Rust pin. Install the
[official prebuilt binary](https://nexte.st/docs/installation/pre-built-binaries/);
`bash scripts/doctor.sh ci` reports it missing. The browser, Cockpit, and
build-observability tools are likewise Nix-or-native; `mise.toml` lists them
under "deliberately absent" with the reason.

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

### Where the pins actually live

[`mise`](https://mise.jdx.dev/) reads the checked-in `mise.toml`, but that file
is a mirror for shell setup, not the source of truth for everything in it.
`rust-toolchain.toml` remains Cargo/rustup's authoritative Rust input and
`.config/zig-toolchain.json` remains the verified Zig release-and-digest input.
Bun is the exception in the other direction: `flake.nix` reads its pin from
`mise.toml` directly, so the Nix shell cannot lag behind it. `just
toolchain-check` gates every one of these surfaces against the others, so an
update cannot leave any of them behind.

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
cargo install --locked wasm-bindgen-cli --version 0.2.128
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
just cockpit-test-no-phux  # the default app graph, without the Phux provider
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

#### Retaining identity-bound Cockpit diagnostics

With the native Debug run above publishing, bind its PID from a second shell
at `clients/cockpit`. This companion uses the already-built matching CLI; it
does not compile or restart the app:

```sh
# APP_PID must be the process launched by your native dev invocation.
RUN="$(python3 scripts/dev-diagnostics.py begin --pid "$APP_PID" \
  --native "$NATIVE" --require-markup-watch \
  --ffi-lib "$PWD/../../target/ffi-dev/libphux_client_ffi.a" \
  --socket "$PHUX_SOCKET")"
python3 scripts/dev-diagnostics.py watch --run "$RUN"
```

`begin` prints the retained directory even if its first snapshot check fails;
check its exit status before starting `watch`. The default artifact is
`zig-out/bin/phux-cockpit`; `--binary PATH` selects another executable. The
publisher's executable path and CWD must match the artifact and app source root.
Both normal and dev-named Cockpit processes are counted: exactly the selected
PID must be live. Each capture checks the process start time, publisher PID,
nonempty window/view tree, and unchanged binary/CLI file identities on both
sides of inspection. Snapshot publication times/ages are retained; a file from
before this process started (or its ambiguous launch second) is refused. Retry
after the app has published. `--require-markup-watch` verifies the live `armed`
field, including when the binary was launched directly without building.

At a problem, use another shell at the app root:

```sh
python3 scripts/dev-diagnostics.py mark-problem --run "$RUN" \
  --target '@w1/phux-cockpit-canvas#123' --input-scope terminal
```

Replace the example target with the SDK widget address for the affected control,
or omit it if unknown. It must be present in that capture. The retained window,
view, and widget addresses include observed focus/selection flags. Input scope
is explicitly operator-declared (`terminal`, `chrome`, `switcher`, `settings`,
`web`, or `unknown`); the current snapshot has no authoritative provider resource
ID or input-routing scope, so those observed fields remain unavailable. Widget
addresses are presentation identities, not durable resource IDs.

The private `.dev-run/diagnostics/<timestamp>-<unique-id>/` directory retains an
immutable `run.json` and timestamped diagnostic samples/incident files. The
manifest records source root/revision/dirty/diff hashes, binary and CLI SHA-256,
SDK source pin and materialized package candidates, and available FFI input
hashes. Captures separately record current source identity, so a hot markup
edit cannot rewrite the original run identity. Untracked file status is hashed;
untracked contents are not included. Pass `--phux-cli PATH` with `--socket` at
`begin` to add read-only `phux status --json` peer-PID/process evidence. Socket
selection is operator-declared; coordinator incarnation is unavailable through
that status surface and remains null.

`--log PATH` at `begin` adds bounded, allowlisted diagnostic summaries and launch
phase timestamps from that invocation's log to each retained sample. The last
1 MiB is inspected; truncation is explicit. A two-second `watch` retains runtime
diagnostics until Ctrl-C or the first invalid capture, including process-exit
refusals. It never stops the app. Logs/snapshots are not copied wholesale:
terminal text, widget names/text values, clipboard, key payloads, dispatch-error
details, config contents, and process arguments/environment are excluded. Raw
logs redirected by the operator and SDK dropbox files stay outside this bundle
and may contain content. Retained evidence persists until explicitly deleted;
`dev-run.sh --fresh` removes the whole default `.dev-run` home, including it.

An on-disk hash is not proof of bytes already mapped into a process. Source SDK
pins, candidate FFI archives, and license notices do not attest what an existing
binary linked. The manifest says so: optimize/provider/linked SDK/linked FFI
configuration remains unknown. Likewise, `dev-run.sh --no-build` reports unknown
configuration and refuses first-frame measurement rather than labeling an old
package with newly supplied build flags. Use the normal build path for
configuration-dependent measurement.

Keep live automation serial. The companion's nonblocking lock serializes its
own captures; a busy capture is a retryable refusal, not permission to drive the
app concurrently with another automation owner. No screenshot in this evidence
claims real macOS raster output. Offline checks for this companion are
`python3 clients/cockpit/scripts/dev-diagnostics_test.py` from the repository root.

### Mutation testing

Mutation tools are optional and separate from contributor and ordinary CI
gates. See [Scoped mutation testing](TESTING_MUTATIONS.md) for pinned runners,
bounded scopes, report interpretation, and the Rust/FFI/Zig evidence boundary.

### Full root validation

Add `just`, `cargo-nextest`, `cargo-deny`, Bash 4+, `actionlint`, `shellcheck`,
`jq`, Python 3, curl, and Node 24 to the native tools. `mise install` supplies
all of these except `cargo-nextest`, Bash and curl; the Nix shell supplies all
of them. With Homebrew:

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
