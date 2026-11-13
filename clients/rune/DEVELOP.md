# Developing phux on Rune

`--phone` opens the phone form, where the phone app is growing ahead of
Rune's phone support: [MOBILE.md](./MOBILE.md) lists what works and what
waits on upstream.

The [native iOS host](./ios/README.md) renders the same phone view inside
UIKit using upstream's embedded Metal host, with system input and layout
guides instead of drawn keys/insets. CI builds simulator and device bundles
and exercises a live terminal in an isolated simulator. This is not yet a
provisioned device/TestFlight release or full native accessibility coverage.

Rune is pinned to a `dev` revision in `Cargo.toml`; this crate has its own
workspace and lock so Rune's graph stays out of the rest of phux. The
[Rune upstream follower](../../scripts/rune-upstream/README.md) moves the pin
twice a day.

Keep all Rune dependencies and the four `[patch.crates-io]` entries on the
same revision: Cargo does not inherit Rune's workspace patches. The `tooling`
feature is required in release builds too: packaged qualification and the
updater's GPU launch check use headless rendering. Desktop startup opts into
Rune's orderly exit so window destruction still closes popup commands and
releases server view leases.

```sh
cd clients/rune
cargo test
cargo run -- --socket /tmp/phux-test/s --session work
```

Point it at a disposable dev server, never the installed one: run a dev
build of `phux` with an explicit short socket under `/tmp`
(`PHUX_PROFILE=rune-test target/debug/phux --socket /tmp/phux-test/s server --ensure`)
and unset every inherited `PHUX_*` variable first.

Rune's headless tooling works on this binary, so changes can be checked
without a window:

```sh
cargo run -- --socket /tmp/phux-test/s --snap out.png --at 3 --size 1100x720
cargo run -- --socket /tmp/phux-test/s --script typing.json --snap out.png --at 5
```

`--tree out.json` writes the element tree, with the accessibility tree that
screen readers and automation tools see under `"accessibility"`. Every window
exposes that tree to the OS through Rune's
[accessibility](https://github.com/anomalyco/rune/tree/dev/crates/rune#accessibility)
support, which is also how tools drive a real window now that Rune's strobe
bridge is gone. Headless time does not synchronize phux's server workers;
use `--realtime` against a live server so frames keep pace with replies,
and wait for the attached terminal before measuring an interaction. Packaged
desktop and phone qualification uses this wall-clock pacing.

The resource smoke test also reads back a genuine Rune GPU frame containing
PNG and JSON panes together. Export that frame and its native accessibility
tree without starting any server or touching installed phux:

```sh
RUNE_RESOURCE_SNAP="$PWD/target/resource-native.png" \
  cargo test --locked native_png_headless_renderer_smoke -- --nocapture
```

The test checks decoded PNG pixels, native image draw commands, JSON text,
and the Read-only mirror label; `target/resource-native.json` is the tree.

`RUNE_ONBOARDING_SNAP=DIR cargo test --locked every_step_renders` writes
each onboarding step as a PNG under `DIR`.

On macOS, notifications need the signed bundle (`rune_notify::available`);
`cargo run` can't post, and Settings says so. On Linux any build posts while
`notify-send` is on `PATH`, and withdraws through `gdbus`.

`scripts/package.sh VERSION OUT_DIR` builds the release archive for the
host (ad-hoc signed; a local build may link the dev shell's libraries, so
ship only CI builds), and needs `bun` for its notices.
`just rune-check` is the gate, `just rune-deny` checks the lockfile, and both
run as the `rune` lane of `ci`, which also packages and launches the archive
with `scripts/qualify.sh ARCHIVE target/debug/phux`. On a dev machine that
check can fail on what a Nix shell linked; CI's rustup builds are the real
answer. [Releasing](../../docs/releasing.md#phux-rune) covers tags and what to
do about a bad release.

| Module | Owns |
| --- | --- |
| `mobile`, `desk/native_input`, `ios` | direct native transport, target-bound UIKit editing, embedded Metal host and simulator qualification; desktop `main` delegates to the shared library |
| `launch` | command line, finding the CLI, `phux server --ensure` |
| `updates` | the feed, staging and swapping a verified build, the phux CLI |
| `diag` | local logs, the panic hook, and the report Copy diagnostics makes |
| `desk` | the runtime client and what is on screen; `desk/*`: layout and splits, tabs, frames and fit, input, mouse, find, links, palette, sessions, inbox, settings, attachments |
| `links` | OSC 8 hyperlinks and written URLs under a cell |
| `activity` | per-terminal agent state, questions, commands and bells, and what needs you |
| `inbox` | what is waiting on you, most urgent first |
| `alerts`, `dock` | system notifications, their clicks, and the Dock count (`dock` is the crate's only `unsafe`) |
| `theme` | built-in terminal palettes and the chrome drawn from them |
| `palette` | palette items and fuzzy ranking |
| `onboarding`, `desk/onboarding`, `ui/onboarding` | first-run steps: the CLI's `--json` answers parsed, step state in settings, the view |
| `grid` | cell metrics and the draw list as Rune elements |
| `keys` | Rune keys as phux key atoms and app shortcuts |
| `ui` | window composition; `ui/*`: sidebar, tabs, panes, pane overlays (find, history), inbox, settings, palette |
