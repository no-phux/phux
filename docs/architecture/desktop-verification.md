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
architecture decision. [Desktop architecture](./desktop.md) owns the seams;
the [consumer contract](../consumers/desktop.md) owns user-visible behavior.
Implementation is authorized and supervised directly. Beads remains the task
tracker; the matrix below defines acceptance scope, not a second work queue.

## Dependency and integration order

The implementation starts with a matched GPUIX source/native toolchain.
Native-host proof and TypeScript-tooling proof can proceed independently after
that foundation. The binding then establishes shared registry identity and
view-aware handles. Runtime independent views precede the native painter;
input and integrated feasibility follow. Full product-shell implementation is
accepted only after the feasibility gate passes.

From the shell, workspace, connections, and settings are separate lanes.
Restoration follows workspace; agent projection follows workspace and
connections. Duplicate-view UX follows workspace plus the runtime view seam.
Native application behavior joins input, workspace, restore, and settings;
trust/diagnostics joins restore, connections, and agents. The final quality gate
joins these surfaces **and independent-view UX**, then package preparation.
Linux qualification follows the Apple-silicon package lane.

Use one writer per owned seam and isolated worktrees. Interface changes to
runtime, projection, or host identity need explicit handoff before dependent
writers proceed. Integrate in dependency order; run affected shared-consumer
checks again on the integrated tree. A clean leaf branch is not proof that the
combined app works.

## Evidence receipt

Every gate records the exact revision, commands, exit status, test count,
fixture identity, and artifact paths. Native receipts also identify the app
binary/addon build, GPUIX/Zed pins, OS, architecture, GPU, display scale/refresh,
window/root/view identity, endpoint, server incarnation, and relevant frame
stream/bootstrap/generation. Store raw logs and captures with the receipt.
Redact credentials and user content; use isolated synthetic fixtures.

Tests use separate HOME/XDG/config/state directories, socket, daemon process,
and application identity. They cannot resize or terminate the user's terminals.
Each parallel worktree uses its own Cargo target directory and app build/cache
outputs. Test startup checks identity and fails if it connects to the wrong
daemon or launches a stale bundle.

Report headless/model, native interaction, real GPU, manual accessibility,
performance, and package evidence distinctly. Missing GPU access or an absent
platform test renderer is a blocked test, not a pass. Typechecking or a
screenshot of a static grid cannot stand in for a live native terminal journey.

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

The runtime-view suite must prune view A's pinned history anchor while view B's
valid viewport, selection and search anchors survive. Test true terminal-wide
invalidation separately, alongside legacy default-view behavior. This catches
the existing kernel history viewport's terminal-wide anchor-clearing hazard.

The feasibility gate fails closed if a native seam is unsound. A deterministic
model of a terminal element is useful regression coverage but cannot justify
proceeding past a missing native element, unsupported multi-window lifecycle,
or absent independent runtime views.

### Terminal fidelity matrix

Fixtures cover dense truecolor/palette/reverse-color grids, default colors,
cursor shapes/width/blink, underline variants and decorations, wide/spacer
cells, combining sequences, emoji, fallback fonts, long wrapped lines, and
clipping at fractional scale. Exercise shell prompt editing, full-screen editor
entry/exit, alternate screen, scrollback pin/follow-live, resize during output,
selection across history boundaries, links, and search match navigation.

Use the full native frame: grid, UTF-8 arena, metadata, color state, cursor,
scrollbar, and damage. Deliberately skip publications and compare incremental
output against a forced-full-paint oracle. Acquire then remove/re-add the
resource to test slot identity independently of generation values. Keep an old
`Arc<GridFrame>` through later publications to prove immutability.

Audit Kitty graphics and every claimed image path separately: engine support,
image storage/placement availability at the publication seam, native textures,
clipping, deletion, and lifecycle. The current `GridFrame` does not expose an
image-placement collection. Do not claim graphics fidelity from a Cargo
feature flag. Any unsupported case needs a named limitation, reproducible
fixture, and explicit release disposition; text fixtures cannot conceal it.

### Input and presentation matrix

Exercise Kitty keyboard behavior, modifiers, repeat/release, non-US layouts,
dead keys, native IME marked text, candidate placement, commit/cancel, paste,
application mouse modes, local selection override, and focus transitions.
Pointer mapping, IME rectangles, and paint must agree on geometry and scale.
Clipboard and file-drop paths use explicit data handling rather than shell
evaluation. Test malformed/untrusted titles and links as display data.

For Unknown delivery, interrupt an acknowledged action at the transport
boundary and retain its correlation. Acquire a fresh frame in a hidden window:
the fence must remain. Cancel a queued paint: it must remain. Present a fresh
authoritative frame visibly, then acknowledge the matching identity: recovery
may clear it. Deliver an old presentation callback after another reconnect or
view replacement: it must not clear the newer fence. Raw input stays gated
throughout. Cover both views of the same terminal because delivery uncertainty
belongs to the terminal, not just the focused placement.

Also introduce a newer Unknown between host validation and acknowledgement,
without reconnect. Conditional runtime acknowledgement must reject the older
fence epoch atomically and preserve the newer fence. Connection/frame identity
alone does not distinguish two delivery fences on the same connection.

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

Native accessibility and IME require actual platform observation with recorded
steps, not only synthetic event dispatch. State clearly which cases are
automated and which have a reproducible manual receipt.

## Tooling acceptance

Run exact pinned native TypeScript 7 against GPUIX JSX and Bun types. Separately
compile Solid universal JSX through the GPUIX development and production build
paths. A no-emit typecheck does not prove the custom-renderer transform works.

Negative fixtures must fail for lost Solid reactivity, unsafe casts,
floating/misused promises, module mocks, invalid/unused suppression, and
forbidden imports. Positive fixtures must accept valid native host events and
explicit external-data parsing. Verify type-aware Oxlint actually ran with the
nearest intended tsconfig and installed plugin versions; missing plugins or
silently skipped files fail the gate. Run vendored anti-slop RuleTester tests.
Formatting/lint fixes converge, and a repeated check leaves no generated drift.
If an Effect module exists, test its actual pinned v4 API, Bun execution,
cancellation, and cleanup. Merely installing Effect is not service coverage.

## Performance and compatibility

Establish hardware-tagged budgets at feasibility, before UI expansion, for
input-to-visible-present p50/p95/p99, paint time, sustained-output throughput,
idle CPU/wakeups, attach/reconnect time, and RSS. Record sample count, warmup,
fixture dimensions, display refresh, run duration, and process breakdown. Run
without concurrent builds. Compare one terminal/one view, one terminal/two
views, multiple terminals, hidden windows, and sustained output plus input.
Measure incremental per-view CPU/memory and long-running open/close soak.

Numeric budgets are not invented here. The feasibility receipt must commit
measured baseline and explicit regression thresholds before the final gate;
missing budgets block performance qualification. A PTY echo measurement alone
does not measure native presentation latency. Native text rendering must not
quietly move cells through JavaScript to pass an easier synthetic benchmark.

Use [SETUP](../SETUP.md) and `bash scripts/doctor.sh <area>` for prerequisites.
Run the scoped runtime/FFI/core tests for view and geometry changes; explicitly
enable optional binding features so their tests cannot disappear from a
crate-scoped run. Preserve C ABI and UniFFI behavior, default-view semantics,
TUI/web compilation and relevant behavior, and Cockpit fixture/native tests.
Shared protocol, FFI, Cargo, or build-input changes require expanded consumer
validation. The integrated root PR bar remains `just ci-full`; desktop-native
and GPU gates supplement it. CI routing must prove desktop-only and shared
changes select the appropriate lanes and retain required aggregate checks.

## Package and platform qualification

The Apple-silicon package contains the matched addon, UI bundle, CLI sidecar,
and license inventory with distinct development/release/Cockpit identity.
Test clean-user install, offline launch, missing or incompatible sidecar,
signature/update verification, tampered update rejection, interrupted download,
and UI update while daemon work continues. Prepare signed artifacts only with
available credentials and record exactly what was verified. Public release is
a separate authorization boundary; unsigned preparation is not signed release
evidence.

Linux requires an explicit distro/architecture/display-server matrix and real
Wayland/X11 input, IME, clipboard, accessibility, window/DPI, GPU/performance,
install, and update evidence. Reuse shared semantics. An unavailable GPUIX Linux
test renderer cannot count as coverage, and macOS receipts do not qualify it.

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

### Initial framework receipt (2026-09-23)

The source-bootstrap implementation is commit `642f4fed`. GPUIX
`6d5e6887ad8dc6e94eb66043394f3d17a462c56a`, Zed
`81c99f816b4a5f69d3c014774068034c24d1d7af`, Bun 1.4.0 and phux Rust 1.98.1
were used on Apple-silicon macOS. Commands completed with exit zero:

- `just desktop-source-build`: release addon, both loaders and Solid adapter.
- `bun run test` in the source `packages/solid`: 14 tests, 42 assertions,
  including native GPU rendering/events and compiled package consumers.
- `bun chat.automation.ts` in source `examples/solid`: six live-window
  scenarios, zero failures, background focus. Captures are under the source
  checkout's `examples/screenshots/solid-chat-{initial,final}.png`.
- `just doctor desktop`, `just setup-check`, `just shellcheck`, and source
  pin regression tests pass.
- `just desktop-framework-check`: byte-identical generated native declarations
  and Solid distribution, production-bundled native window, reactive click and
  text-input round trip, and `clients/desktop/dist/framework/window.png` capture.
  `just desktop-check` passes 23 tooling fixtures and nine source-pin tests,
  plus the selected anti-slop RuleTester suites, native TS7 and Oxc checks.

The raw build and automation logs are recorded in `phux-d4x9.1`. These are
framework receipts, not phux terminal, patched-host, performance, accessibility,
or packaged-desktop qualification.
