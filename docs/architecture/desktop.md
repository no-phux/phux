---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-02
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
authorizes this target design for implementation. It does not describe a
completed `clients/desktop` integration.

## Existing substrate and actual seams

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
remain authoritative. The table names existing methods; the view and host
operations below are required extensions.

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

UI modules reach native operations only through `src/bridge`: they never own
a socket, parse VT, or add a retry ladder. Rust keeps engine, transport,
reconnect, and acknowledged-delivery authority. The optional NAPI encoder
belongs in `phux-client-ffi` beside C and UniFFI, deriving from
`projection/`. Commands and painter must resolve handles in the same loaded
host instance (a registry linked into two addons is two registries).

## Framework feasibility

[GPUIX](https://github.com/remorses/gpuix) has a Solid universal adapter and
native custom-element traits, but its `custom_elements` module is private and
there is no terminal factory, so a bounded source patch must register one in
the single host using GPUIX's own GPUI types. Its scroll-handle state is
single-window-only; multi-window proof must cover tree identity, focus,
scrolling, selection, automation, callbacks, menus, and teardown. Build from
matched pinned source until a published Solid/native pair is verified; pins,
digests, and licenses live in `clients/desktop/toolchain/`, environment setup
in [SETUP](../SETUP.md).

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

Publication and damage are per view: engine dirty flags consumed by one
projection must still fan out to the others. Rebootstrap, removal, eviction,
and disposal invalidate or rebase affected handles; one view's search and
selection never mutate a sibling, and pruning one view's pinned history anchor
must not erase another's. History loading and cache budgets stay
terminal-owned. Move transfers a view without detach; duplicate creates a
view on the same terminal; closing one view never releases a sibling's
attachment; late replies cannot act on a recycled placement.

## Geometry

One geometry calculation maps font metrics, content bounds, scale, cell counts,
pointer hit-testing, IME rectangle, and clipping. It is used by both native
layout and paint, rather than independently rounded in TypeScript and Rust.

One writable view per terminal owns its size: the terminal's only visible
view, or among several, the one the user focused last. The shell names that
owner (`sizeOwner`); the native element proposes whole cells that fit its
painted bounds through `resize_terminal`, edge-triggered, once the size has held
still for 60 ms (a timer, not the next frame, sends it). A pane already at its
authoritative size sends nothing, and new terminals spawn at their predicted
size, so shells are not resized through intermediate layouts. Gaining ownership or
re-activating the window re-proposes, reclaiming a size another client changed.
Focus transfer changes the owner only after identity and role checks; observer
activity does not seize control. Other views crop/display authoritative
columns and rows, with their own scroll positions and unused space. No locally
invented reflow.

The desktop connects with a zero viewport, which casts no window-size vote
(L1 §9.2): attaching reshapes nothing another client is showing, spawns take
the server default unless given a predicted size, and quitting leaves the
explicit sizes in place instead of restoring headless geometry.

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
The bridge drains at most once per display frame (`DRAIN_INTERVAL_MS`): every
drain that changes the UI is a whole-window GPUIX draw, layout included.
Local scroll, selection, search, and settings changes also invalidate paint,
even when no network event arrives. A drain invalidates only what it can
repaint: output events bump their own terminal's paint revision, any other
non-badge event bumps every terminal's, so output in terminals nobody shows
redraws nothing.

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
`acknowledge_projection`. The desktop registers that call only from GPUI's
drawable-presented callback, after AppKit visibility and an unclipped terminal
bounds check. Paint and `on_next_frame` do not clear the fence. Record the qualified terminal, view, connection,
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

Placement snapshots are bounded, versioned, atomic, serialized, and
namespaced apart from Cockpit; they hold host references, qualified identity,
layout, and view preferences, never credentials, pointers, live anchors, or
process checkpoints, and resolve against live inventory before reattach.
Configuration and registry/dialer authority are the existing shared ones;
project organization never rewrites another consumer's layout, and shared
agent meaning belongs in the shared projection.

## Tooling contract

Bun per the repository pin; native TypeScript 7 typechecks with no emit and
does not replace GPUIX's Solid Babel transform. Oxlint runs type-aware with
`@oxlint/plugins` matched exactly, Solid reactivity rules via GPUIX
`moduleSources`, and vendored [anti-slop](https://github.com/dmmulroy/anti-slop)
rules with provenance and licenses. Required: strict typecheck, warning-free
lint, unused-suppression detection, format convergence, generated-type
freshness, import-boundary checks, and negative fixtures. Effect v4 is
exact-pinned only when a real service needs it. Exact versions live in
`clients/desktop/package.json`.

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
