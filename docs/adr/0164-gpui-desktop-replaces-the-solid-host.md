---
audience: contributors, agents
stability: stable
last-reviewed: 2026-10-13
---

# 0164 — GPUI desktop replaces the Solid host

**TL;DR.** The desktop is a Rust GPUI and GPUI Kit application that links the
client runtime in-process. The shipping app is that executable. There is no
Solid shell, GPUIX host, JavaScript bundle, or NAPI addon. Independent views,
focused-writable geometry, close-detaches, and the separate installed CLI
remain. Apple-silicon macOS is still the first release target.

Status: Accepted
Date: 2026-10-13

## Context

[ADR-0139](./0139-solid-desktop-over-native-runtime-views.md) built the desktop
as a Solid shell over a patched GPUIX native host, with an optional NAPI
encoder sharing one client registry. That host needed a bounded GPUIX patch,
a matched Solid adapter, and a JavaScript bridge that was not allowed to own
terminal cells. The runtime, grid publication, and presentation fence were
already Rust.

## Decision

Replace that host with `clients/desktop/native`. The executable is
`phux-desktop`. It imports `gpui` as `gpui-pre` 0.3.8, `gpui_platform` as
`gpui-pre-platform` 0.3.8, and `gpui_kit` 0.7.1. A parent-managed pinned GPUI
snapshot patch supplies the drawable-presented callback. Paint and
`on_next_frame` do not clear a presentation fence.

The process owns connection, workspace, terminal entities, settings, and
platform menus. It does not keep a second reconnect or input-delivery state
machine beside `phux-client-runtime`. `Phux.app` runs that binary plus fonts
and an icon. It does not ship a JS bundle or a `.node` addon.

The installed `phux` CLI stays a separate prerequisite. Dock launch resolves
it and never replaces it. Isolated qualification passes `--phux` explicitly
and refuses an installed binary and a production socket.

Independent views, one focused size owner, zero-viewport attach, and
close-detaches are unchanged. Closing a view does not end execution. Explicit
Terminate Terminal does. Restoration remembers placement, not process
survival. Cockpit remains a separate client. Linux is a later qualification.
Projects/worktree grouping, remote hosts, and approvals are not claimed by
this decision.

Bun may run script tooling. It is not the production host.

## Why

The painter was already forbidden to serialize cells through JavaScript. A
Solid shell plus a patched custom-element registry added a second GPUI type
universe and a packaging constraint without owning the terminal. Linking the
runtime in one GPUI process removes that boundary and keeps the
drawable-presented acknowledgement on the thread that painted the frame.

## Tradeoffs

The Solid reactive shell and its JSX gates go away. GPUI Kit and a patched
GPUI snapshot are now the UI dependency. macOS qualification still does not
establish Linux input, accessibility, or graphics. View-local rendering still
costs memory and CPU. Observer panes may crop, and changing the size owner
can reflow the real application.

## Alternatives

**Keep the GPUIX Solid host.** Rejected: the patch, addon, and JS bridge were
the cost this cutover removes.

**Render cells in a web view.** Rejected: a second grid interpretation on the
hot path.

**Embed the NAPI encoder in the Rust binary.** Rejected: the process can call
the runtime directly. [ADR-0135](./0135-one-binding-crate.md) still owns C and
UniFFI encoders; this app is not a binding.
