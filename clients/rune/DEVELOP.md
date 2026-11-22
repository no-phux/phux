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

Keep all Rune dependencies and the six `[patch.crates-io]` entries on the
same upstream base: Cargo does not inherit Rune's workspace patches. Normally
all ten revisions match. The `winit` override temporarily uses an exact
[owned-fork](https://github.com/no-phux/rune) descendant of that base, containing
only a Wayland caret-rectangle publication fix; the other nine entries stay
upstream. A bump must rebase and review this small patch on the new upstream
head, not drop the fix or retain an older fork base. Reunify all ten when
upstream supplies equivalent behavior. On every host, `rune-check` compiles the
actual pinned Wayland cursor helper with `rustc --test` and runs its six
headless regressions. This does not compile the Linux native integration or
replace mandatory real-compositor qualification in Linux CI.

The `tooling`
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

## Keystroke latency

Rune writes its hops into the server's keystroke-echo trace
(`PHUX_ECHO_HOPS=FILE`, `pid TAB hop TAB unix_nanos`): `stdin` when the key
event arrives, `send` when the input is handed to the runtime,
`client_recv` when the runtime wakes the window, `build` when the focused
pane builds a frame with a new publication, and `stdout` when the first
frame showing it is drawn (after present in a window, after submit
headless). The server's hops fall between `send` and `client_recv`.
`scripts/bench/rune-echo.sh` runs it end to end against a temp-socket
server and prints p50 and p99 per hop and for `stdin->stdout`:

```sh
just perf-rune-echo        # or, built already:
bash scripts/bench/rune-echo.sh target/release/phux clients/rune/target/release/phux-rune 200 120 60
```

The arguments after the binaries are keystrokes, the gap between them in
ms, and the headless frame rate. At 60 the frames are paced like a 60 Hz
display; at 1000 a frame starts as soon as the last one ends, which leaves
the client's and server's work. `--window` (macOS) types into a real window
with `post-keys.swift` instead, which needs the Accessibility permission and
a window that stays uncovered for the run: Rune draws nothing while
occluded. `PHUX_ECHO_FAST=0` turns off the server's fast paths and the
echo wait below, for a before/after with one build.

A key runs inside a frame that Rune presents, so an echo that lands after
it waits for the display's next opportunity, a refresh or more. The key's
frame therefore holds up to 4 ms for the view's next publication, woken by
the runtime's publish rather than polling (`desk/input/echo.rs`,
`TerminalPublication::wait_past`), and a local server's echo draws in that
same frame.

The trace stops at the present. `just perf-rune-photon` (or
`rune-echo.sh --photon`) goes on to the screen: `key-photon.swift` posts
each key to the window's process with `CGEvent.postToPid`, so nothing else
receives it, watches that window's pixels with ScreenCaptureKit at the
display's refresh rate, and prints key-to-changed-pixels p50 and p99, for
Rune and then for Ghostty running the same shell at the same font size on
the same display. It needs the Screen Recording and Accessibility
permissions for the app that runs it, an unlocked screen with the display
awake, and both windows uncovered. The display's scanout and panel response
come after the captured frame and are the same for both apps; every number
rounds up to a refresh interval, so compare medians over 100 or more keys.

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
| `desk` | the window's preferences, platform services and composition; `desk/connection` owns a runtime client's workspace, view leases and request namespace |
| `desk/surfaces`, `ui` | one back-to-front surface order for painting, keyboard, paste and native editing; playground scenes share one active slot |
| `features/changes` | checkout presentation, feature-owned state transitions, view and tests; `desk/changes` supplies focus and window effects |
| `desk/fleet` | cross-machine agent projection, ordering, identity and navigation, independent of any scene |
| `playground`, `ui/playground/*` | deterministic scene support and shared drawing; each scene keeps its own geometry and simulation |
| `desk/scene_host` | the fleet scenes' (hive, kitchen, pitch, pit lane, reef) Desk state and ways in and out, written once; each sim states what differs through `FleetScene`. The galaxy stays apart |
| `test_support` | isolated window preferences and configurable embedded clients, shared by feature tests and renderer budgets |
| `links` | OSC 8 hyperlinks and written URLs under a cell |
| `activity` | per-terminal agent state, questions, commands and bells, and what needs you |
| `inbox` | what is waiting on you, most urgent first |
| `alerts`, `dock` | system notifications, their clicks, and the Dock count |
| `handoff` | the pane in front as an `NSUserActivity` (`sh.phux.pane`) the phone app continues; advertised only from the signed bundle |
| `theme` | terminal themes: the shared catalog, Ghostty and Omarchy adapters, the desktop appearance |
| `palette` | palette items and fuzzy ranking |
| `onboarding`, `desk/onboarding`, `ui/onboarding` | first-run steps: the CLI's `--json` answers parsed, step state in settings, the view |
| `grid` | cell metrics and the draw list as Rune elements |
| `keys` | Rune keys as phux key atoms and app shortcuts |
| `ui` | window composition; `ui/*`: sidebar, tabs, panes, pane overlays (find, history), inbox, settings, palette |

## Ownership

`Desk` is Rune's single root state, not a second terminal runtime. Its
`Connection` holds handles, view leases, layout, resource publications and
request correlations from one runtime client. Preferences, native trust,
window geometry, update workers and quick-terminal registration stay on the
window.

A transport reconnect on the same client retains replay-safe correlations.
Replacing an ended client retires the old views and popup command before
resetting its namespace. A retry restores terminal focus or a document's stable
key and keeps window-local Settings and drafts. A machine switch also clears
machine-bound confirmations, navigation and activity. Neither path constructs
another window or reloads its preferences. Image-cache admission accounting
survives resource retirement together with its cache.

Painting and input use the same ordered surfaces on desktop and phone.
Settings and its theme palette may nest; playground scenes are exclusive.
Queued clipboard and native editing callbacks are fenced to their originating
runtime namespace and incarnation, not just a numeric pane handle.

Keep feature state transitions with the feature, with small `Desk` adapters
for window effects. Pure presentation and the Rune view remain separate
responsibilities inside that module. Shared chrome belongs in `ui`; test
fixtures belong in `test_support`, not a benchmark or another feature.

## Design tokens

The chrome draws only from `ui::tokens::Tokens`, built per appearance from
the design tokens through `phux_design` ([design language](../../docs/design-language.md)).
`Tokens::for_window` gives the window's (the chosen theme derived, or the
house look in the system appearance; the increased-contrast variant and
forced colours when the system asks); `Tokens::for_phone` gives the phone's,
in the terminal ground's mode at the iOS text size. Both read reduce
transparency and the text size from `Desk`. Memos key on `tokens.version`, never on a
theme id: Omarchy swaps palettes under one id.

- Colours are roles (`t.roles.layer.raised`, `t.roles.status.needs_you_wash`),
  painted with `Paint::paint`. Translucent roles carry their alpha.
- Type is `t.ty.{micro,caption,label,body,lead,title,display,mono}`; space,
  radius and icon sizes are `t.space`, `t.radius`, `t.icon`; springs are
  `t.motion` pairs; shadows are `t.elevate(e, Tier::…)`.
- Status is `t.status.state(phux_design::State::…)`: ink, companions,
  vector mark and word. `ui::badge::{mark, tone_badge}` draw it.
- phux's own rows are `ui::rows::Row`, with a forced `State` for the
  gallery; kit components take `&t.kit`.
- Kit overlays mount above the window through `ui::kit_overlay`, and their
  open state is a `Surface` (`Menu`, `Select`, `Toasts`).

Two guards keep raw styling out. `clippy.toml` disallows the raw calls
(`El::shadow`, `TextStyle::new`, `Color::alpha`, …); only owner and art
modules carry an outer `#[allow(clippy::disallowed_methods)]` on their
`mod` item (`ui::tokens`, `theme`, `grid`, the scenes, the onboarding
windows). `scripts/style_literals.py` counts raw literals per file against
`scripts/style_literals.baseline.json`, which is empty: a new literal fails.
Owner paths (`src/ui/tokens*`, `src/theme*`, the terminal's own `src/grid*`),
the scene art and test fixtures are exempt, and a literal `const` counts
only in a view (`src/ui*`, `src/features/`). `just rune-gallery DIR` paints every snapshot
suite, the design fixture included, blended in sRGB as the app is.

Idioms the audit's literal checks would otherwise flag, each with its
reason:

- **Hover reveal.** `ui::reveal` is the one `group_opacity` outside the
  owner paths: a tab's chord and close button, and a pane's drag grip,
  swap at 0 or 1 under the pointer without a view rebuild. It never dims.
- **Unfocused split dim.** With several panes, the panes without the
  keyboard sit under `layer.scrim`. It is the modal scrim's role
  used at pane scope, the one second meaning the role has: a pane behind
  the focused one is set back the way a window behind a sheet is.
- **BEL wash (audit row 28): n/a in Rune.** Rune draws no BEL wash; a bell
  off screen is the bell mark and word on the row (`activity::Mark::Bell`).
- **Lost and ended.** Rune sees no pane exit (a closed terminal leaves the
  tree) and reports a lost link window-wide in the title bar, so rows reach
  `tone_of` (`Activity::tone_input`) without `exit`, `crashed` or `link`.

### Kit deviations

rune-kit is adopted where its look fits the schema. Where it does not, the
difference is accepted and listed here, or phux keeps its own component.
The gallery's states page paints each, so a kit bump is reviewed.

| Deviation | Why it stands | Upstream candidate |
| --- | --- | --- |
| Literal kit radii: compact `Menu` 6, `Dialog` 6, `SegmentedControl` 6 and a fixed 28 height, `Toast` 8/4, `Select` 4/6, `Keybind` 2, radio 8/3, `Card` rule 2, `TabStrip` 6, tooltip 4, compact `Badge` 2, `SidebarRow` badge 8, switch tracks | Accepted where adopted; the tooltip takes `.radius(t.radius.md)`; `Dialog` and phone pills are phux's own | kit geometry reads `k.radius` |
| Kit `md` means "control", so `kit.radius = {xs, xs, sm, md, lg}` and `Card`, `Popover` and standard `Menu` panels draw at `radius.sm` | Accepted | separate control and surface radii |
| `SegmentedControl` focus ring (2, 1) against the tokens' offset 2 | Accepted | kit rings read `cx.focus_ring` |
| Kit controls draw disabled with `layer_opacity` 0.4–0.5 | Accepted for kit controls; phux's own use faint ink | kit disabled through `text_faint` |
| `Dialog` and `Menu` enter on `k.motion.overlay` with no scale | Accepted; phux's own surfaces scale from `motion.enter_scale` | an overlay enter-scale token |
| `TabStrip` (active treatment, fixed 28 row, `MORPH` lift, accent busy dot, no done word, internal keys) | Tabs are phux's own | selected-treatment and status hooks, `TabWords::done`, a height setting |
| `List` is label-only at 14/500 | Rows are phux's own (`ui::rows`) | a row builder with details and spans |
| `Badge` fills are fixed and it is a live region | `ui::badge::tone_badge` | `Badge::tone` and `.live(false)` |
| `Dialog` has a fixed height, no footer or scrolling body | `ui::Sheet` is phux's own, on Rune's `.modal` | a footer slot, top offset and scroll body |
| English kit strings (`SidebarHeader` "N items", `SidebarRow` state words) | Row descriptions are overridden; "N items" stands | kit `Words` |
| No spacing scale in the kit; `INDENT` 16 is a constant | Kit settings are set from `t.space` | kit `Space` tokens |
| `SplitView`'s 48 pt band and opaque root | Not adopted | `band_height`, a ground-fill option |
| No kit slider | Settings' glass slider is phux's own | a kit `Slider` |
| No reduce-transparency, low-power or layout-direction API on `Cx` | `Desk` reads reduce transparency itself (macOS `NSWorkspace`, the iOS bridge's `phux_rune_ios_traits`) with the iOS text size, and `Tokens` hashes both into `version`; the `ar` gallery page shows the script, not mirrored chrome | `cx.reduced_transparency()`, `cx.low_power()`, a layout direction |
| `SystemMaterial` has no iOS implementation | Phone glass is `backdrop_blur` only | `SystemMaterial` on iOS |
| `cx.web_focus()` (every pressable a Tab stop) | Off: it moves a modal's first focus onto its first control and focuses a control on press, so Enter no longer blurs Settings search (phone and window), the phone's top buttons and browser take focus, and a pointer-opened context menu walks from a different item. Rows, tabs and kit controls are focusable without it, and the update card's action is a Tab stop | a web-focus mode that leaves a modal's first focus to `El::autofocus` |
| Kit `TextInput` keys its editor `input` | Fields stay Rune `text_input` in a control-outline shell (`border.control`, `radius.sm`, the invalid state): native IME and tests find `palette-query`, `find-query`, `inbox-reply`, `settings-query`, `machine-field`, `rename-field` and the ACP fields by their own keys | `TextInput` takes an editor key |
| Kit `DiffView` tints from status tones and solves its own syntax hues | The changes diff is phux's own on `diff.*` and `syntax.*` (D8) | `DiffView` takes explicit diff and syntax roles |
| Kit `Toast` view: fixed width 320, truncated title, close named "Close", unkeyed actions | The toast is phux's own on tokens inside the kit `ToastRegion`, drawing the desk's kit `ToastStack` | `Toast` width, wrap and keys |
| `SegmentedControl` fixed height 28 | Find's case toggle is a one-segment control; the update channel is phux's own 44 pt radio group (touch minimum) | a height setting |
| Kit `Sidebar` puts every child in one `focus_follows` group and owns the scroll | The window's sidebar is phux's own list (`sidebar-list`, a following focus group) with other machines in their own non-following group of kit `SidebarRow`s; buttons inside rows are pointer-only, with reasons | a Sidebar section that does not follow focus |
| Kit button heights are fixed | Kit buttons get `.min_h(line + pad)` so they grow with the text size | buttons that size from their type |
| Kit `Switch` carries no row semantics | The setting row carries the switch role, name, description and state; the kit control inside it is `focusable(false)` and hidden from assistive tech | a switch row |
| Kit `Checkbox` label uses the kit's small type | Accepted | label type from `k.ty` roles |
| Kit `Select` needs its own open state | `Desk` holds it (`select_open`, `toggle_select`); the font setting stays a cycling row until it adopts one | — |
