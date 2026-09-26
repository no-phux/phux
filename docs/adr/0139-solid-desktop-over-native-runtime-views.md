---
audience: contributors, agents
stability: stable
last-reviewed: 2026-09-23
---

# 0139 — Solid desktop over native runtime views

**TL;DR.** The desktop is a separate GPUIX Solid consumer with a native GPUI
terminal painter and runtime-owned independent views of each terminal. One
loaded native host shares the client registry between the binding and painter.
Apple-silicon macOS is the first release target. Cockpit remains an independent
Native SDK experiment.

Status: Accepted
Date: 2026-09-23

## Context

The desktop needs a maintainable reactive shell, native terminal interaction,
project organization, and contextual agent information. Reusing Cockpit's UI
implementation would carry its Native SDK constraints into a different renderer.
The shared runtime already owns connection recovery, engine replicas, and
immutable grid publication; a desktop must preserve that authority.

GPUIX has a Solid universal renderer, but its native custom-element registry is
internal and some renderer state assumes one window. Those are integration
requirements to prove, rather than reasons to invent another terminal engine.
The existing runtime also keys presentation state by terminal. A cloned client
shares that state; it does not provide independent views.

## Decision

Build `clients/desktop` as a separate Solid application. Solid owns reactive
chrome. A native GPUI element paints runtime `Arc<GridFrame>` publications
without serializing terminal cells through JavaScript. Keep the existing
Ghostty pin, daemon, wire, and client-runtime authority.

Extend [ADR-0135](./0135-one-binding-crate.md)'s encoder set with an optional
mechanical NAPI encoder in `phux-client-ffi`. Shared product meaning belongs in
its `projection/` module. The application-specific painter belongs in the
desktop native host. Both access one actual loaded client registry and one
pinned GPUI type universe; two separately linked copies are not a shared host.

Independent simultaneous views of one running terminal are first-release
requirements. The runtime owns view identity, viewport, selection, search
anchors, and publication lifetime. A terminal still has one execution identity
and authoritative PTY geometry. The focused writable desktop view proposes its
size; other views display or crop the same authoritative columns, subject to
existing server sizing policy. They never create a second PTY or locally reflow
the application into a fictitious width.

Desktop placement is local presentation. Closing a pane, tab, window, or the
application detaches views; explicit **Terminate Terminal** ends execution.
This is the desktop's contract, not a change to Cockpit's
[close policy](./0114-cockpit-closes-terminals-and-detaches-windows.md).
Restoration remembers placement, not execution durability; the daemon-crash
limit in [ADR-0130](./0130-on-disk-pty-journal-is-not-built.md) still applies.

Use the approved package layout and tooling boundaries in the
[desktop architecture](../architecture/desktop.md). Bun follows the repository
pin. Oxc supplies formatting and type-aware linting; native TypeScript 7 checks
types; GPUIX's Solid universal transform compiles JSX. Review and vendor the
selected anti-slop rules. Effect v4 is reserved for real TypeScript asynchronous
services; it does not replace Solid reactivity or Rust transport/recovery.

Ship Apple-silicon macOS first, including independent views. Linux is a separate
platform qualification. Preserve Cockpit. Expose current agent resources and
attention without implementing a speculative Objective/Run coordinator.

## Why

Native frame acquisition preserves cell fidelity, bounded ownership, and the
existing engine thread boundary. Solid can update navigation and details without
reconciling every terminal cell. Runtime view ownership gives every binding the
same independent-presentation semantics and keeps input tied to one process.

Focused-view sizing is understandable in two differently sized windows and
avoids resize oscillation between sibling views. Authoritative readback keeps
the desktop honest when another consumer or server policy wins arbitration.

## Tradeoffs

The native host needs a bounded GPUIX patch and real multi-window proof. Source
pins and packaging must keep the Solid adapter, native addon, and GPUI revision
matched. View-local rendering has a measurable memory and CPU cost; the runtime
must retain compatible default-view behavior for existing consumers.

Observer views may crop output, and changing the controlling view can reflow
the real application. Closing a view does not stop its process, so termination
must be explicit and discoverable. macOS qualification does not establish Linux
input, accessibility, or graphics support.

## Alternatives

**Port Cockpit's implementation.** Rejected: its native host and presentation
ownership are the very boundary being replaced, while its experiment remains
useful independently.

**Render cells through Solid or a web terminal.** Rejected: a second
JavaScript-side grid/engine adds a hot-path copy and another interpretation of
the terminal. The existing native runtime already publishes the required grid.

**Clone clients or spawn duplicate terminals.** Rejected: a client clone shares
presentation; a new process is not another view of the same execution.

**Ship one placement per terminal.** Rejected for the first release. A temporary
development restriction cannot satisfy the independent-view acceptance gate.

**Build agent orchestration into the desktop.** Rejected: existing resources,
events, and approvals provide useful agent context without inventing durable
work authority in a client.
