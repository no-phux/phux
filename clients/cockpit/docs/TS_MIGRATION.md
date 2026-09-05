# Migrating Cockpit's authoring to TypeScript + `.native` markup

Status (2026-09-04): **shipped.** Root `src/core.ts` and `src/app.native` are
Cockpit's only app coordinator and visible-chrome authoring path. Beneath them,
`src/cockpit/native/terminal_painter.zig` draws real local or Phux-backed
terminals through the unchanged native engine.
The seam (`ts_protocol.zig`, `ts_snapshot.zig`, `ts_engine.zig`, the fork's
`native_extension` hook in `src/native_extension.zig`) carries
intents, snapshots and invalidations; the engine now also owns the PTY spawn,
output, key encoding, committed text and the resize pump, through the same
`terminal_runtime.zig` the shipping app uses (made generic over the effects
type). No terminal byte enters the compiled core: shell events are consumed in
the pty event constructor and the core receives a void `engine_wake`. Guards
under the extension prove boot, resync, the revision fence, key routing and
shell ownership red-then-green (`zig build test -Dplatform=null`). The phase 2
parity harness remains authoritative: `ts-chrome-parity` solves the
compiled `app.native` at every declared window size and density, in every
chrome state the core can reach, and runs the toolkit's layout audit on it,
driving the real core through the rig so there is one rule. It found two
defects on its first run (an unbounded sixteen-tab run, and a drag row that
pushed twenty drag regions at a sixteen-region platform table); the engine now
owns the tab run through the shipping `visibleTabRun` projection and the
snapshot carries it. The overlays are real: the switcher lists the
engine's tabs by strip position and title, filters on a typed needle through
the toolkit's own text input, and selects through the seam; the settings
surface shows the builtin theme catalog the snapshot trailer carries, saves
through a `set_theme` intent (the engine writes the config and reports a
refusal), reveals the config file, and probes the file once on opening
(`probe_config`), never per frame. Escape, the arrows and Enter reach the
overlays through the extension's key fallback while they own the keyboard.
The native behaviours are the engine's too: raw surface pointer input over
a pane's frame routes through the shipping `pointer_input.handleTerminalPointer`
(selection, mouse reporting, wheel scrollback, link hover and open), with the
same capture rules CockpitHost applies; cmd+F search, cmd+C copy, cmd+V
paste (bracketed, or into the needle), cmd+A and the keyboard-selection
chord mirror `update.zig`'s terminal block; a bell while the app is
deactivated notifies on its rising edge; a mouse-protocol flip ends
mismatched captures. `pointer_input.zig` and `pasteClipboardText` are generic
over the effects type for the same reason `terminal_runtime.zig` is. Secondary
windows are model-declared: the snapshot carries a section per open window,
the core exports `windows(model)` with the shipping scene's own labels, each
slot binds one shared markup template (`src/windows/`), and the engine
paints, pumps and routes every window through the scene's canvas table; the
platform's focused window is the active one, and a tab intent from a window's
chrome names that window. Menus and physical shortcuts, splits and divider
dragging, Finder drops, selection autoscroll, split-aware cwd projection, and
the switcher/settings overlays now follow the same native seams. Every
secondary window uses the shared overlay-capable markup component and names its
window in positional intents; a platform event may use window `255` only to
target the focused window the native host already adopted. Topology
persistence uses the shipping 750ms debounce and retry accounting and flushes
synchronously on stop.

Configured startup is shared in `src/cockpit/startup.zig`, so both composition
roots load the same config, restore topology and cwd, apply tab-placement
precedence, and select the same provider. The TypeScript graph honors
`-Dphux-enabled=true`; with the same-checkout client FFI built, its full graph
passes 58/58 tests and reports the Phux transport, host, provider, pointer and
extension as compiled and tested. The adapter's real lifecycle now opens,
drains, reconnects, and closes the provider and pointer channels behind the
native engine seam; TypeScript receives only ordered snapshot invalidations.
Direct local terminals remain intentionally ephemeral; provider-qualified Phux
terminals remain the durable identity. Focused extension tests also cover split
PTY creation, Finder drops, selection autoscroll, split-aware cwd invalidation,
configured shell/scrollback, restored topology/cwd, and tab-placement
precedence.

Packaged automation is green on the shipping root with both the direct-local
and real-Phux provider variants. The isolated runs verify packet presentation,
startup structure, `cmd+t`, `cmd+f`, Escape, zero dispatch errors, a matching
`publisher_pid`, and exactly one switcher in the focused secondary window. The
Phux run additionally waits for a provider-backed terminal to be admitted from
inventory, then proves `cmd+t` adds exactly one local tab without assuming
provider titles use local `Terminal N` numbering. Root ownership removes the
pinned SDK packaging workaround: `zig build package` now consumes production
`app.zon` directly, and `scripts/automate-smoke.sh` always drives the shipping
TypeScript graph.

The snapshot's bounded cwd field does **not** bump protocol version 1. This is
an internal, statically linked seam: encoder and decoder ship in one binary,
there is no persisted packet or independently deployable peer, and the decoder
rejects trailing or malformed framing. Version 1 identifies the current
lockstep Cockpit seam; it will bump only if compatibility across independent
producers and consumers becomes a requirement.

Building Cockpit needs the SDK package's TypeScript toolchain, which the
tarball pin does not carry. Once per pin, on the package `zig build` resolved:

```sh
cd zig-pkg/native_sdk-*/packages/core && npm ci --include=dev
zig build test -Dplatform=null
```

Without it the build stops at "the @native-sdk/core frontend cannot resolve
its TypeScript toolchain" and names the exact directory. `build.zig` installs
that package-local toolchain once per SDK pin when necessary. Node 24+ is the
supported host.

## Target

The Native SDK's primary authoring path is a TypeScript app core (`src/core.ts`:
`Model`, `Msg`, `update`, `subscriptions`) plus declarative markup
(`src/app.native`). It compiles ahead-of-time to native code — no JS runtime in
the binary. Cockpit now uses that path end to end for app coordination and
visible chrome. Native modules own terminal/provider correctness and paint an
app-owned canvas prefix beneath the declarative tree.

The goal is to move what the SDK's TS tier is good at (declarative chrome,
bindings, derived values, the automation-checked markup contract) onto that
path, and keep everything that must sit next to libghostty-vt native. The SDK
tier split is binary: an app graph is EITHER a Zig core OR a TS core. There is
no second app loop, so this is a swap of the core staged behind proven seams —
not a file-by-file port. Rendering CAN compose: a custom Zig view may call
`canvas.CompiledMarkupView(...).build` and place the native terminal subtree
beside/under that node. That is the shipped shape: one TS model/update
loop, `.native` chrome, and one app-owned Zig terminal module.

## What moves, what stays

Grounded in a survey of `src/cockpit/native/` (2026-08-24):

**Moved to `.native` markup + TS core:** tab strip / side rail triggers,
new-tab and overflow controls, status and config notices, switcher and settings
overlays, split controls, tooltips, context menus, menus and shortcuts. Their
former `view.zig` implementations retired with the Zig composition root.

**Stays native regardless** (no markup equivalent, or correctness lives beside
emulator state):
- Terminal cell grids and emulator-adjacent painting in
  `terminal_painter.zig`: dim scrims, focus edges, OSC 8 preview band,
  hand-managed command-id namespaces and per-pane budgets.
- `grid.Session` + libghostty-vt, key encoding (`terminal_runtime.zig:198` —
  kitty-protocol encoding depends on live emulator modes), the lossless
  outbound ring and its back-pressure invariants (`:80`–`:146`), the resize
  pump (needs the measured cell box only the native painter writes,
  `workspace_projection.zig:1439`), providers, and extension event routing.
- Native terminal search and input fallback while markup textboxes own modal
  keyboard focus.
- The single-geometry law: `resolvePanesIn` / `workspaceChromeIn` remain THE
  source for painter rects, hit-test rects, and PTY sizing. Markup layout must
  consume these derivations, not re-derive them; `ts-chrome-parity` audits the
  solved markup tree against that projection.

## The seams (must exist and be tested BEFORE any swap)

1. **Engine → core:** `Cmd.channelOpen` channels fed by a native poster
   (pattern already in-tree: `openPhuxChannel`, `openPointerMonitor`,
   `update.zig:108`–`:146`) deliver chrome-state snapshots and terminal frame
   notifications. Known bound: channel posts are capped at 4096 bytes
   (`FINDINGS.md:258`), so payloads are chunked or summarized, never raw
   scrollback.
2. **Core → engine:** how a TS core issues commands (pty spawn/write/resize/
   kill, tab ops) to a native extension is now settled: `Cmd.host` /
   `Cmd.request` ride `TsUiApp.CoreOptions.host_calls`, whose public
   `HostCallBinding` is synchronous-or-async and answers requests through the
   ordinary effect result path. Cockpit exposes one versioned intent command
   and one snapshot request rather than mirroring every native operation into
   the interface. The SDK-generated runner does not expose that option to an
   app, so the fork needs one narrow native-extension hook to configure
   `CoreOptions`, configure the composed view/chrome options, and decorate the
   resulting `native_sdk.App` with Cockpit's event/focus host.
3. **Terminal pixels:** two candidates, decide by measurement not taste:
   - Keep native painting of grids into the chrome display list (today's path)
     and let markup own only the interactive chrome around it.
   - `media-surface` leaves (`gpu_surfaces` capability) composited into the
     tree, fed by a native RGBA producer. Costs: loses the wire-v6 incremental
     patch path for those cells, and accessibility must come from a parallel
     surface. Only wins if it removes more native paint code than it costs.

## Completed phases

0. **Toolchain proof (spike).** Install `@native-sdk/cli`; build and drive one
   fork example (`examples/gpu-components`) through `native dev` /
   `native test -Dplatform=null` on this machine. Add the TS compile step to a
   scratch app target. Exit criterion: a TS-core binary runs and snapshots via
   the automation harness.
1. **Seam contracts.** Land (2) above behind tests, in the Zig app, using the
   channel pattern that already exists. No product change. Guards prove the
   4096-byte chunking and ordering invariants.
2. **Parity harness.** `ts-chrome-parity` drives the real compiled core and
   audits solved markup trees against the register ladder at every declared
   window size and reachable chrome state.
3. **Swap, window by window.** Main-window chrome to `app.native` + TS core;
   secondary windows follow (`#351` exposes model-declared windows to TS).
   Each swap keeps the automation smoke green and ships behind no flag — the
   binary either is the cockpit or is not merged.
4. **Delete the old path.** Completed with the root shipping flip: `view.zig`,
   the Zig composition root, and their chrome-only tests retired. Native engine
   and terminal tests remain behind the test-only facade.

## Non-goals

- Rewriting the terminal engine in TS (impossible: subset has no FFI, no raw
  bidirectional streams, no PTY ioctl).
- A web/WebView frontend. The TS path compiles to native; nothing here adopts
  a browser runtime.
- Faking parity. The swap shipped only after the TypeScript graph passed the
  parity and packaged automation gates; there is no dual-maintained app graph.
