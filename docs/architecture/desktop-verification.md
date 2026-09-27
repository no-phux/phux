---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-23
---

# Desktop verification

**TL;DR.** Desktop acceptance requires real native rendering and interaction,
runtime view isolation, compatibility checks, and identity-bound evidence.
Headless models cannot prove GPU, IME, or accessibility behavior. Establish the
host and independent-view seams before product UI fan-out; gate packaging on
the complete first-release surface, including duplicate views. This is the
verification contract, not a record of passing implementation tests.

[ADR-0139](../adr/0139-solid-desktop-over-native-runtime-views.md) owns the
decision, [desktop architecture](./desktop.md) the seams, and the
[consumer contract](../consumers/desktop.md) user-visible behavior. Beads
tracks the work; this page defines acceptance scope.

## Integration order

Matched GPUIX toolchain first; native-host and TypeScript-tooling proof in
parallel after it; then binding identity and view-aware handles, runtime
independent views, the native painter, input, and the integrated feasibility
gate. Product lanes (shell, workspace, connections, settings, restore,
agents, duplicate views, native behavior, trust) follow feasibility; the final
quality gate joins them before packaging, and Linux follows the Apple-silicon
package. Integrate in dependency order and rerun shared-consumer checks on the
integrated tree.

## Evidence receipt

Every gate records revision, commands, exit status, test count, fixture
identity, and artifact paths; native receipts add build, GPUIX/Zed pins,
OS/GPU/display, window/view identity, endpoint, server incarnation, and frame
generation. Tests use isolated HOME/XDG/socket/daemon/app identity and never
touch the user's terminals. Report headless, native, GPU, accessibility,
performance, and package evidence separately; missing GPU access is a blocked
test, not a pass.

## Foundation and feasibility gates

| Gate | Required evidence |
|---|---|
| Matched toolchain | Frozen clean install; source and artifact digests; exact GPUIX/Zed/native/Solid provenance and licenses; a real Solid window; production bundle launch; unchanged Ghostty pin. |
| Native extension | Terminal factory registered through the bounded patch; one GPUI type universe; command and painter handles resolve in one loaded registry; stale-handle rejection. |
| Multi-window | Two independently changing trees with correct focus, scroll, selection, menus, automation, and retained terminals; close/reopen with queued callbacks; no singleton cross-talk. |
| Binding | Shared projection parity; lossless IDs/counters; one listener and event drain; wake rearming races; disposal/reconnect fencing; optional NAPI feature tested explicitly. |
| Runtime views | Two views share output/process identity while scroll, selection, search, and follow-live remain independent; close one without detaching the other; rebootstrap, history eviction, stale callback and retained-frame lifetime tests. |
| Geometry | Differently sized panes and windows; focused-writable control; observer resize is inert; authoritative server readback; exact targeted attach/reconnect replay; other terminals untouched; existing size-policy behavior preserved. |
| Painter | Real GPU cells, native frame acquisition, no JS hot-path cell transport; full repaint after skipped generations, new slots, stream/bootstrap changes, theme/font/geometry changes; removal clears even at equal generation. |
| Input | Actual readiness and authority gates; raw queue success not reported as delivery; native IME and physical-key arbitration; no duplicate text; acknowledged outcomes and presentation-gated Unknown recovery. |
| Integrated feasibility | Live shell, editor, and agent TUI; output pressure, resize, selection/search, two windows, disconnect/reconnect, app restart, daemon restart, IME and screen reader; initial measured performance baseline. |

The runtime-view suite prunes view A's pinned history anchor while view B's
anchors survive, and tests terminal-wide invalidation separately. The
feasibility gate fails closed: a deterministic model cannot stand in for a
missing native element, multi-window lifecycle, or independent views.

### Terminal fidelity matrix

Fixtures cover dense truecolor/palette/reverse-color grids, default colors,
cursor shapes/width/blink, underline variants and decorations, wide/spacer
cells, combining sequences, emoji, fallback fonts, long wrapped lines, and
clipping at fractional scale. Exercise shell prompt editing, full-screen editor
entry/exit, alternate screen, scrollback pin/follow-live, resize during output,
selection across history boundaries, links, and search match navigation.

Paint from the full native frame and compare incremental output against a
forced-full-paint oracle with deliberately skipped publications; test slot
identity by removing and re-adding a resource, and immutability by holding an
old `Arc<GridFrame>`. `GridFrame` exposes no image placements today, so Kitty
graphics fidelity needs its own audit; any unsupported case needs a named
limitation, fixture, and release disposition.

### Input and presentation matrix

Exercise Kitty keyboard behavior, modifiers, repeat/release, non-US layouts,
dead keys, native IME marked text, candidate placement, commit/cancel, paste,
application mouse modes, local selection override, and focus transitions.
Pointer mapping, IME rectangles, and paint must agree on geometry and scale.
Clipboard and file-drop paths use explicit data handling rather than shell
evaluation. Test malformed/untrusted titles and links as display data.

For Unknown delivery: a fresh frame in a hidden window or a cancelled paint
must leave the fence; a visible authoritative presentation with matching
identity may clear it; a stale callback after reconnect, view replacement, or
a newer Unknown (between validation and acknowledgement) must not. Raw input
stays gated throughout, across both views of the terminal.

## Product acceptance journeys

| Surface | Required functional and failure-path evidence |
|---|---|
| Terminal-first shell | First launch to usable local terminal; optional folder/project entry; keyboard and pointer command parity; loading, empty, error, recovery states rendered natively. |
| Projects and worktrees | Plain folder, repository, sibling worktree, multi-folder project, and same path on two hosts; observed CWD does not silently reorganize placements; removing organization leaves files/processes intact. |
| Workspace | Rename/reorder/split/move across windows without restart; deterministic minimum-size layout; cancellation; pending spawn after destination disposal; exact focus restoration. |
| Duplicate views | Open Another View versus Reveal Existing; two panes/two windows; independent scroll/selection/search and shared process; geometry arbitration; close/move/reopen and restoration; no wrong-view input. |
| Persistence | Same-incarnation reattach; changed-incarnation tombstone and no recycled-ID binding; offline host; monitor remap; bounded/corrupt/newer snapshots; migration and concurrent saves; no implicit command replay. |
| Connections | Missing local daemon and bundled-CLI failure; compatible/mismatched server; concurrent local/remote hosts; credential expiration/refusal; capability/role transitions; no duplicate retry loop. |
| Settings | Effective/default/source display; atomic comment-preserving edits; external edit race; malformed config; live versus next-start behavior; theme/font invalidation; shortcut conflict and recovery. |
| Agents | First agent reveals contextual details; AgentSession parent/child lifecycle; emitted state versus detector fallback; bounded ordered event tail, truncation/gaps, unknown words and tombstones; deduplicated attention; stale/unauthorized approval refusal. |
| Native application | Menus, Dock/reopen, dialogs/drop/links, hidden windows, multiple displays, scale/appearance/reduced motion/high contrast, notifications, sleep/wake, close versus quit. |
| Accessibility | Real screen-reader journey from project navigation to terminal text, selection/search, agent details, approvals and recovery; accessible names, keyboard focus order, native IME, no trapped focus. |
| Diagnostics and trust | Typed refused/unknown outcomes; registry authority and server approvals respected; bounded/redacted identity-rich receipt; teardown races and malicious display data. |
| Lifecycle | App close/crash/update preserves daemon processes; explicit termination closes the resource for every view; daemon crash/restart honestly shows lost processes and volatile scrollback. |

Accessibility and IME require real platform observation; mark each case as
automated or manual-with-receipt.

## Tooling acceptance

Typecheck with the pinned native TypeScript 7 and separately compile Solid
universal JSX through GPUIX's development and production builds; a no-emit
typecheck does not prove the transform. Negative fixtures fail for lost
reactivity, unsafe casts, misused promises, module mocks, bad suppressions,
and forbidden imports; type-aware Oxlint must provably run with the intended
tsconfig and plugins; fixes converge with no generated drift. An Effect
module, if any, is tested against its pinned v4 API.

## Performance and compatibility

Before UI expansion, commit hardware-tagged baselines and regression
thresholds for input-to-present p50/p95/p99, paint time, throughput, idle
CPU/wakeups, attach/reconnect time, and RSS, across one and two views, many
terminals, hidden windows, and a soak. A PTY echo number is not presentation
latency, and cells must not move through JavaScript.

View and geometry changes run the scoped runtime/FFI/core tests with optional
binding features enabled, and preserve C ABI, UniFFI, default-view, TUI, web,
and Cockpit behavior. `just ci-full` remains the root bar; desktop GPU gates
supplement it.

## Package and platform qualification

The Apple-silicon package (addon, UI bundle, CLI sidecar, licenses, distinct
identity) needs clean install, offline launch, sidecar mismatch, signature
and tampered-update, interrupted download, and update-while-daemon-runs
evidence. Unsigned preparation is not signed-release evidence. Linux needs its
own distro/display-server matrix with real input, IME, accessibility, and GPU
evidence; macOS receipts do not qualify it.

## Status

These are remaining acceptance gaps under
[ADR-0139](../adr/0139-solid-desktop-over-native-runtime-views.md), not completed
test results.

| Target | Current evidence and gap | Tracked work |
|---|---|---|
| Foundation and feasibility | Matched source release build, 14 upstream Solid GPU/consumer tests and six upstream live-window scenarios pass; patched native host, bridge, view runtime, painter, input and integrated feasibility remain unqualified. | phux-d4x9.1–.6, phux-d4x9.17, phux-d4x9.20 |
| Complete product journeys | Contract specified; shell/workspace/restore/connections/settings/agents/native/trust and duplicate-view evidence required. | phux-d4x9.7–.14, phux-d4x9.18 |
| Regression and compatibility | Native CI/routing, cross-consumer tests, measured budgets and soak receipts required. | phux-d4x9.15 |
| Apple-silicon package | Clean install/update and signed-package preparation evidence required. | phux-d4x9.16 |
| Linux qualification | Subsequent platform matrix and native evidence required. | phux-d4x9.19 |
