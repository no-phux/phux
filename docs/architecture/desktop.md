---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-23
---

# Desktop architecture

**TL;DR.** Solid owns desktop chrome; a native GPUI terminal element paints
immutable runtime frames. One loaded host shares client handles with a
mechanical NAPI encoder. Independent terminal views and targeted geometry are
required runtime extensions. This document separates the existing substrate
from the accepted integration contract, including identity, event drainage,
presentation acknowledgement, tooling, and teardown.

The [product contract](../consumers/desktop.md) owns desktop terminology and
interaction. [ADR-0139](../adr/0139-solid-desktop-over-native-runtime-views.md)
owns this architecture choice. The following target design is authorized for
implementation; it is not a claim that `clients/desktop` already delivers it.

## Existing substrate and actual seams

The source inspection for this contract establishes:

| Source | Present behavior | Integration consequence |
|---|---|---|
| [`runtime.rs`](../../crates/phux-client-runtime/src/runtime.rs) | `Runtime::connect` owns a driver; `Client` clones share `Arc<Inner>`, one listener, and one control plane. | Clone is a handle, not an independent presentation. |
| [`engine/owner.rs`](../../crates/phux-client-runtime/src/engine/owner.rs) | Projectors, selections, gestures, and viewport anchors are keyed by `ResourceId`. | Introduce runtime view ownership before duplicate-view UI can qualify. |
| [`session.rs`](../../crates/phux-client-core/src/session.rs) and [`history.rs`](../../crates/phux-client-core/src/history.rs) | The kernel keeps one history viewport; reconciliation can clear all document anchors when its pinned anchor is pruned. | Separate shared history loading from view-local presentation and invalidate only affected view anchors. |
| [`publication.rs`](../../crates/phux-client-runtime/src/publication.rs) | `GridFrame` is immutable; slots are terminal-keyed; removed retained slots acquire `None` while keeping their old generation. | Retain frames safely and handle removal independently of generation equality. |
| [`control/commands.rs`](../../crates/phux-client-runtime/src/control/commands.rs) | `resize_viewport` changes the session and foreign subscriptions; per-terminal attach uses the shared viewport. | Add targeted desired geometry and reconnect replay, not a loop of global resize calls. |
| [`runtime/input.rs`](../../crates/phux-client-runtime/src/runtime/input.rs) | Readiness, delivery fences, and projection acknowledgement are separate methods from raw input. | The host must gate raw dispatch and own proof of presentation. |
| [`control/input.rs`](../../crates/phux-client-runtime/src/control/input.rs) | Raw send checks the delivery fence and queues a frame; `acknowledge_projection` clears that fence. | A true send result is not readiness or server receipt; acknowledgement is consequential. |
| [`projection/`](../../crates/phux-client-ffi/src/projection/) | Shared Rust derives event, topology, agent, status, outcome, ID, and grid vocabulary. | Extend shared meaning here; keep the encoder mechanical. |
| [`projection/grid.rs`](../../crates/phux-client-ffi/src/projection/grid.rs) | `GridView` exposes a subset of the native frame. | Paint from native frame facts, including metadata and complete color state. |

Existing [runtime architecture](./client-runtime.md) and wire/input contracts
remain authoritative. Method names in this table exist. The view and host
operations below describe required seams, not a fabricated current API.

## Ownership and package boundaries

```text
Solid shell / workspace / agents / settings
                   |
          typed bridge commands and snapshots
                   |
   one loaded native host and Client registry
        |                            |
 optional phux-client-ffi NAPI     GPUI terminal element
   shared projection/                |
        |                      acquire Arc<GridFrame>
        +------ phux-client-runtime --+
                    |
          existing Ghostty / daemon / wire
```

The approved package layout is `clients/desktop/` with:

| Path | Owns |
|---|---|
| `src/shell` | Application roots, command registry, menus, palette, and navigation composition. |
| `src/workspace` | Projects, folders, tabs, split trees, placements, and restore projection. |
| `src/terminal` | Terminal host-element wrapper and chrome; no JavaScript grid or VT parser. |
| `src/agents` | Agent details, attention, and approval presentation. |
| `src/connections` | Host inventory and connection/recovery UI. |
| `src/settings` | Settings, theme, font, and keymap UI. |
| `src/ui` | Native-renderer controls and design tokens. |
| `src/bridge` | Typed native boundary and generated type integration. |
| `src/services` | Actual TypeScript async services with scoped lifetimes. |
| `native/src` | GPUI host, painter, session registry, platform integration, persistence, diagnostics. |
| `tests` | Contract, model, tooling, native integration, and evidence fixtures. |
| `tools/oxlint` | Native Solid rule configuration and reviewed vendored anti-slop rules. |
| `toolchain` | Immutable source/artifact provenance and bounded GPUIX patches. |
| `scripts`, `packaging` | Reproducible build, checks, and package preparation. |

Use one package-local manifest and lockfile. Avoid empty abstraction layers or
speculative services. UI modules import native operations through `src/bridge`;
they cannot import native internals, own a socket, parse VT, or implement a
second retry ladder. Solid owns reactive lifetime. Effect v4 owns only real
TypeScript async resources, cancellation, or external-data schemas. Rust keeps
engine, transport, reconnect, and acknowledged-delivery authority.

The optional NAPI encoder belongs in `phux-client-ffi`, alongside C and UniFFI,
with feature-scoped build/tests. Its values derive from `projection/`. The
desktop painter stays application-specific. Statically linking a registry into
two addons does not share it: commands and painter must resolve handles in the
same loaded host instance. Assert that identity in integration tests.

## Framework feasibility

The inspected [GPUIX source](https://github.com/remorses/gpuix) has a Solid 1
universal adapter and native `CustomElement`, `CustomElementFactory`, and
`CustomElementRegistry` traits/types. Its `custom_elements` module is private;
there is no built-in terminal factory. A bounded source patch must expose or
integrate registration in the one host, using the same GPUI types as GPUIX.
It must not bolt on an unrelated second renderer.

The inspected `renderer.rs` explicitly calls its scroll-handle state singleton
single-window-only. Window/root ownership must cover retained tree identity,
focus, scrolling, selection, automation, callbacks, menus, and element teardown.
A second blank window is not multi-window proof. Solid's native test renderer
does not establish Linux graphics coverage.

Use a matched source build until a complete published Solid/native combination
is verified. Registry reinspection on 2026-09-23 found both `@gpuix/solid` and
`@gpuix/native` 0.10.0 published, superseding the earlier unavailable-Solid /
native-0.9.0 observation. Solid declares native `^0.10.0`, and native lists an
exact 0.10.0 Darwin arm64 optional package. Availability alone does not prove
source provenance, the required host patch, or a working matched build. Exact
source pins, digests, Zed gitlink, licenses, and build commands belong in
`toolchain/`; environment installation belongs in [SETUP](../SETUP.md), not
this page.

## Identity and independent views

Distinguish these identities at every boundary:

- A qualified terminal is endpoint/serving authority plus server incarnation
  plus `ResourceId`, including its satellite route when applicable.
- A runtime view is a generation-fenced native handle referencing that terminal.
- A placement is a stable local layout identity; it owns a view attachment.
- A window/root generation fences asynchronous callbacks and spawn destinations.
- A frame additionally carries stream, bootstrap, publication generation, and
  applied sequence identity. These are not interchangeable with server incarnation.

Resource strings reuse [`projection::id`](../../crates/phux-client-ffi/src/projection/id.rs).
Carry 64-bit counters as lossless strings or bigint at the JS seam, never
rounded numbers. Handles cannot be raw pointers or reusable slot indices
without generation checks. Every operation validates the view's live mapping.

Extend the runtime with view creation/release, per-view acquisition,
scroll/follow-live, selection/gesture, search/anchor ownership, and presentation
state. Preserve explicit default-view compatibility for existing consumers.
Canonical execution, output sequence, mode state, and input authority remain
terminal-owned. Runtime-owned engine state must provide independently anchored
viewports without a JS-side replica. Whether projection uses serially restored
engine viewport state or additional native replicas is an implementation
choice requiring measured costs and isolation proof; neither allows a new PTY.

Do not treat repeated engine dirty reads as independent damage histories.
Publication and damage are per view; the engine owner must fan out valid
updates even when the first projection consumes engine dirty flags. Rebootstrap,
resource removal, history eviction, and view disposal invalidate or explicitly
rebase affected handles. Search cursor and selection in one view cannot mutate
a sibling. Closing the last view releases view-owned state and the no-longer-
needed subscription, respecting existing session-level attachment ownership.

Shared history loading, cache budgets, and prefetch remain terminal-owned.
The kernel's current singleton history viewport is not multi-view authority.
Pruning one view's pinned anchor must not erase another view's still-valid
selection or search anchors. True replica replacement and terminal-wide history
invalidation remain explicitly terminal-wide; preserve legacy default-view behavior.

Move transfers ownership without detach/restart. Duplicate creates a new view
on the same qualified terminal. Closing one view never releases a sibling's
attachment. Pending operations retain original destination and incarnation;
late replies cannot act on a recycled placement. Temporary single-placement
guards require issue-linked comments and cannot pass the first-release gate.

## Geometry

One geometry calculation maps font metrics, content bounds, scale, cell counts,
pointer hit-testing, IME rectangle, and clipping. It is used by both native
layout and paint, rather than independently rounded in TypeScript and Rust.

The focused writable view proposes the terminal's desired size. Focus transfer
changes the controller only after identity and role checks. When no eligible
view is focused, retain the last authoritative geometry; observer activity does
not seize control. Other views crop/display authoritative columns and rows,
with their own scroll positions and unused space. No locally invented reflow.

Add a targeted runtime geometry seam that remembers desired geometry per
terminal, applies it only to the intended terminal on attach/reconnect, and
respects role and subscription confirmation. Existing server `window-size`
policy and explicit resize semantics still apply; read back actual geometry.
Coalesce intermediate drag sizes, but ensure the final requested size is sent.
Do not repeatedly call `Client::resize_viewport` for differently sized panes.

## Frames, wakes, and input

One host listener and one event drain exist per native `Client`. Drain once,
then fan out projected state and invalidations to every root/view. A component
cannot compete for `take_events` or replace the listener. Runtime wakes are
edge-triggered; schedule a bounded UI drain and preserve rearming under races.
Local scroll, selection, search, and settings changes also invalidate paint,
even when no network event arrives.

The painter retains an acquired `Arc<GridFrame>` through paint. The acquired
frame's generation is the truth; a preceding atomic generation poll is only a
hint. Use row damage only for the immediately consecutive generation of the
same slot/stream/bootstrap/view and rendering parameters. Skipped generations,
identity changes, font/theme/geometry changes, and a new slot require full
paint. A removed slot's `acquire(None)` clears stale presentation even when
its generation equals the cached number. Never publish a cached old frame as
evidence of recovery.

Native input combines focused live-view identity, actual `input_ready`,
role/lease authority, and delivery-fence checks. Raw-send booleans are local
queue results. Acknowledged operations retain their runtime correlation and
Delivered/Refused/Unknown result; JS cannot introduce an unsafe retry loop.
Physical key events and IME/text commits have one arbitration path.

Unknown delivery is terminal-wide, not view-local. A fresh authoritative frame
must reach a visible presentation before the host invokes
`acknowledge_projection`. Record the qualified terminal, view, connection,
stream/bootstrap, and actually presented generation in that acknowledgement
path. Acquisition, background layout, hidden windows, and queued-but-cancelled
paint are insufficient. Revalidate after a reconnect or root replacement so a
late presentation callback cannot clear a newer fence.

The current unconditional `acknowledge_projection(terminal_id)` is insufficient
for that guarantee. Add a conditional acknowledgement carrying the specific
delivery-fence epoch/correlation; validate identity and clear only that current
fence atomically on the serialized control owner. A host-side check followed by
an unconditional clear is racy even without reconnect: a newer Unknown can
arrive between the check and clear.

On disposal, invalidate handles and callback generations before removing UI
objects, unregister wakes, cancel queued work, release frames/views, and let
runtime shutdown finish safely. No native callback may enter a disposed Solid
root; no blocking driver join or daemon bootstrap belongs on the paint path.

## Persistence and external authority

Persist bounded, versioned, atomic local placement snapshots in a namespace
distinct from Cockpit and development/release siblings. Store host references,
qualified execution identity, layout, and view preferences; never credentials,
native pointers, live document anchors, or purported process checkpoints.
Resolve against authoritative inventory before reattachment. Keep offline
tombstones and original corrupt/newer snapshots recoverable. Serialize saves
and migration so concurrent window closure cannot lose newer state.

Use existing configuration catalogue/writer and registry/dialer authority.
Project organization does not rewrite another consumer's session layout. Agent
interpretation belongs in shared projection when it has shared meaning; UI
sorting and focus are local. Gaps and open vocabularies stay explicit. No
desktop-only coordinator or execution durability claim is introduced.

## Tooling contract

Use the repository Bun pin and compatible Node environment. Native TypeScript
7 typechecks with no emit; it does not replace GPUIX's Solid universal Babel
preload/build transform. Exact installed versions and commands must be recorded
by the toolchain lane. Registry metadata inspected for this design identified
native `typescript` 7.0.2 (`tsc`), Oxlint 1.85.0, Oxfmt 0.70.0,
`oxlint-tsgolint` 7.0.2002, `eslint-plugin-solid` 0.18.0, and Effect
4.0.0-rc.117 as candidates, not a tested combination. `tsgo` is the older
native-preview package binary; do not select it merely because of the name.

Match `@oxlint/plugins` exactly to Oxlint. Enable type-aware lint with its
separate `oxlint-tsgolint` installation and nearest-tsconfig discovery. Use
Solid reactivity rules through the JS plugin with GPUIX `moduleSources`
recognition; DOM-only rules and browser element assumptions do not apply to
native custom tags. Vendor reviewed generic
[anti-slop](https://github.com/dmmulroy/anti-slop) rules with upstream provenance
and all license notices. Document selected rules and exemptions; keep honest
unknown/schema parsing at external seams rather than forcing unsafe casts.

Strict native typechecking, warning-free lint, unused-suppression detection,
formatting convergence, generated-type freshness, architectural import checks,
and negative rule fixtures are required. Exact-pin Effect v4 when a real
service needs it; its installed release types/source, not v3 examples or
unmatched main-branch migration snippets, govern cancellation and API use.

## Status

Every target below is owned by [ADR-0139](../adr/0139-solid-desktop-over-native-runtime-views.md).

| Target | Present gap | Tracked work |
|---|---|---|
| Matched GPUIX host and tooling | Pinned source release build, 14 upstream Solid GPU/consumer tests, and six live-window automation scenarios pass; desktop patch, shared registry identity, and integrated tooling proof remain required. | phux-d4x9.1, phux-d4x9.2, phux-d4x9.20 |
| Shared NAPI projection and targeted geometry | C/UniFFI and global viewport exist; desktop encoder and desired-geometry seam required. | phux-d4x9.3 |
| Runtime independent views | Terminal-keyed owner/publication must gain view-local state and compatibility proof. | phux-d4x9.17 |
| Native painter/input/presentation | Immutable frames exist; GPUI painter, readiness gating, and presentation evidence required. | phux-d4x9.4–.6 |
| Product and platform integration | Package ownership is accepted; workspace, restoration, connections, settings, agents, native UX, and diagnostics remain unqualified. | phux-d4x9.7–.14, phux-d4x9.18 |
| Release qualification | Native CI, measured budgets, package isolation and update preparation remain required; Linux is subsequent. | phux-d4x9.15, phux-d4x9.16, phux-d4x9.19 |

## Where to go next

[Desktop verification](./desktop-verification.md) defines dependency order and
the concrete evidence required before calling any integration complete.
