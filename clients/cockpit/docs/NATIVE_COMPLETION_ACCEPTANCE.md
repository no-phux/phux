# Native Phux completion: validation record

> **Historical evidence (2026-09-08).** Results and guard references below
> describe that acceptance run. The permanent guard ledger is retired; see
> the current [mutation testing policy](../../../docs/TESTING_MUTATIONS.md).

## Verdict (2026-09-08)

The composed native implementation passes the automated gates below. Final
interactive acceptance of this exact app build is **blocked by the build Mac's
locked console**. This record does not claim a completed on-glass acceptance.

The app was built from `468a1b18`, rebased onto `main` at `c6c37118`. Subsequent
changes re-prove a retained guard, repair a server test fixture, and record
validation; they do not change the app executable.

## Shipped implementation in this batch

- Provider-qualified native key, text, clipboard, pointer and viewport routing.
- Coordinator-backed creation, split and window transactions, with durable
  attachment identity and persisted placement restoration.
- Explicit detach/reattach, bounded replica retirement, and offline placement
  close without terminating durable execution.
- Compiled-chrome terminal geometry shared by paint and pointer interaction;
  unchanged GPU frames reuse the measured geometry.
- Terminal-scoped Find across rebootstrap, with replica-specific results and
  selection state fenced until replacement publication.
- Deep-owned last-proven display across disconnect, without admitting input
  or an unaccepted replacement's pixels.
- Client-only Clear preserving partial CSI, OSC, DCS and UTF-8 parser input.
- Configured terminal colors, title replacement, per-window recovery/history
  status, and owner-qualified native bell attention.

## Automated evidence

All commands exited zero in the native-completion checkout unless a diagnostic
failure is explicitly described below. Local logs are under
`/private/tmp/opencode/`.

| Check | Result | Log |
|---|---|---|
| `bash scripts/doctor.sh cockpit` | No prerequisite problems | Session tool output |
| `just cockpit-test` | 535 passed, 2 skipped; Phux compiled and tested; worktree-private cache and correct source root | `final-cockpit-test.log` |
| `PHUX_ZIG_BUILD_TIMEOUT=1200 just cockpit-build` | Pass, same-checkout FFI and coordinator CLI | `final-cockpit-build-retry.log` |
| Private-cache `zig-build.sh --timeout 1200 package -j2 -Doptimize=ReleaseSafe -Dautomation=true -Dphux-enabled=true -Dphux-client-ffi-profile=ffi-dev` | Pass | `final-cockpit-package.log` |
| Core and C ABI library tests, all features | 236 core and 59 FFI passed | `final-rust-lib.log` |
| `cargo nextest run --locked -j2 -p phux-server --lib --all-features` | 959 passed, none skipped | `final-server-nextest.log` |
| Server `attach` and `hub_relay_federation` integration tests | 46 and 15 passed | `final-server-integration.log` |
| CLI `service` tests with `--include-ignored` | 39 passed | `final-service.log` |
| All-target/all-feature Clippy for client-core, client-ffi, server and CLI, `-D warnings` | Pass | `final-clippy.log` |
| Workspace rustdoc, all features, `-D warnings` | Pass | `final-rustdoc.log` |
| Core/FFI doctests | 2 passed | `final-api-doctests.log` |
| Node navigation tests with `navigation-loader.mjs` | 9 passed | `final-node-tests.log` |
| `cargo fmt --all --check`, `git diff --check main`, `just docs-check` | Pass | Session output / `cockpit-completion-docs.log` |
| C ABI operations, pointer and detach probe compilation | Pass with system Clang and explicit macOS SDK | Session tool output |
| Real-server C ABI detach probe | 20 durable spawn/detach cycles; original terminal reattached and still producing output | `final-detach-live.log` |

The first app build exhausted the default 600-second cold-build timeout; its
1200-second retry passed. The first concurrent server-library run missed two
500 ms hangup-grace assertions. Investigation found 18 abandoned escaped-child
test fixtures, including CPU-burning shell loops. The fixture omitted the
environment variable used to record its cleanup PID. The repair sets that
variable and makes cleanup run on assertion unwinding as well as success.
Its named regression failed before the repair, then passed. Both strict-grace
tests and the complete 959-test server suite subsequently passed. No abandoned
escaped-spewer fixture remained after that run.

## Regression and complexity evidence

The retained guards record named-test failure with each fix disabled, then
restored green. New composition proofs cover Find rebootstrap, frozen Find,
frozen display, identity-pending paint, parser-safe Clear, remote bell/status,
compiled terminal-space geometry, and GPU-frame measurement reuse. The Clear
counterfactual rebuilds the Rust FFI: escape injection produces literal `1mX`
where the interrupted CSI must produce `X`.

The geometry-cache guard counts actual measurement calls: eight unchanged GPU
frames cause zero additional measurements; resize and chrome rebuild each
cause one. This is an allocation/work-avoidance result, not a latency benchmark.

No project Zig/Rust cyclomatic-complexity threshold is configured. Source
counts include written branches, loops, `catch`/`orelse` and boolean decisions.
The earlier interaction refactors reduced `applyIntent` 18→8, `onText` 11→6,
and `sendMouse` 11→3. The signal projection was split into state admission,
phase mapping and history mapping; its admission function is CC 8 and history
mapping CC 4. The new owned-canvas clone is CC 4; the fixture cleanup's `Drop`
method is CC 1. Detailed metadata measurements and independent-review
dispositions remain in [REMOTE_GRID_METADATA.md](REMOTE_GRID_METADATA.md).

## Live acceptance status

Earlier live acceptance at `da36dc77` proved typing, two remote splits,
byte-identical quit/reopen topology, continuing execution, reconnect, title
replacement, offline close, catalog reattachment and exact selection/copy.
That build predates the final geometry, Find, frozen-display and signal fixes.

The final isolated bundle launched through `scripts/dev-run.sh` with publisher
PID **44885**, separate config/state and coordinator socket under
`/private/tmp/opencode/cockpit-final/`. Its initial snapshot proves connected
Phux and terminal bounds `(8,58 1084x542)` inside the compiled slot
`(0,50 1100x558)`. The locked console never granted window focus: snapshots
report `focused=false`, macOS reports `IOConsoleLocked = Yes`, and assistive
window access is refused. Background committed text is correctly fenced, so
those attempts are not counted as successful input acceptance.

To finish: unlock that Mac, foreground **Phux Cockpit (dev)**, verify its current
publisher PID, and complete typing/split, quit/reopen with continuing work,
reconnect/frozen display, Find/resize, pointer, Clear, theme and bell checks.
The exact remaining interactive evidence is tracked in `phux-slogic.4.4`.
CPU reference screenshots do not establish AppKit/CoreText raster fidelity.
