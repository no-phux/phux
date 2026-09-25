---
audience: agents, contributors
stability: evolving
last-reviewed: 2026-09-23
---

# Native presentation acknowledgement foundation

**TL;DR.** `src/presentation.rs` acquires an authoritative recovery ticket and
implements conditional revalidation. Production cannot construct its `Presented`
receipt yet: the pinned GPUI API exposes neither native visibility nor a
successful drawable-presentation notification. Do not connect acknowledgement to
canvas paint or `on_next_frame`. The terminal's Unknown-delivery fence must remain
until the platform evidence exists.

The product contract lives in [desktop architecture](../../../docs/architecture/desktop.md#frames-wakes-and-input).

## Ticket and lifetime

`Surface` belongs to one native terminal custom element. `Surface::acquire` takes
the real GPUI window, qualified client handle, terminal ID and view ID. Its ticket
retains the actual `Arc<GridFrame>`, connection epoch, exact `ProjectionFence`,
window identity and weak surface allocation. A non-reused registry handle names
the client; tickets deliberately do not retain a `Client` shutdown lease.

One `Client::with_control` turn proves current engine membership, attached status,
engine readiness and publication `(stream, bootstrap, sequence)` agreement before
capturing the frame and fence. `ControlPlane::input_ready` cannot be used here:
it is correctly false while delivery is fenced. Recovery capture also clears
predictive echo through the existing engine operation and reacquires its actual
publication, because predictions change cells without changing replica sequence.

`Surface::invalidate` replaces the allocation rather than incrementing a wrapping
counter. Call it **before applying target props**, and on destroy. Moving the
same object to another actual GPUI window also replaces that allocation. A new
root gets a new `Surface`, even if its retained node number is reused. Neither
node IDs nor the fixture painter's global observation registry establish identity.

Only a receipt carrying the same window, weak allocation and exact frame can
reach acknowledgement. In one control-owner turn the completion path rechecks
the registry handle, root liveness, connection, current engine/view, actual
publication identity, tail position and conditional delivery fence. A replacement
publication is rejected even if its sequence is unchanged. Any qualifying view
can clear its terminal's fence, but a scrolled-back view cannot.

The registry can close concurrently with a lookup. Both short-lived lookup leases
are released on GPUI's background executor, after leaving the control owner, so
a last `Client` drop cannot join the runtime driver on the paint thread. No
listener, event drain, transport, JS acknowledgement, or JS cell export is added.

## Integration into the parent-owned terminal surface

Add `mod presentation` at the host crate root and `presentation: Surface` to the
terminal element. Initialize it with `Surface::default()`. In `render`, acquire a
ticket using the actual `window` and `cx`, then pass `ticket.frame()` to
`paint::prepare`. Capture the ticket alongside the paint closure. If acquisition
fails, use the typed rejection as the existing paint error text.

In `set_prop`, invalidate before changing `clientHandle`, `terminalId` or `viewId`.
Invalidating on an equal value is conservative and safe. In `destroy`, invalidate
before releasing UI objects. The callback owns only the ticket's weak lifetime.

After `prepared.paint`, a successful observation must identify the same frame,
have no error, and cover the terminal's actual visible bounds. Only then may the
future platform seam register that ticket for the current scene's presentation.
Dropping the ticket on failed/empty/clipped paint or destruction does nothing.
Do not construct a `Presented` value in `terminal.rs`; its private fields are an
intentional fail-closed boundary. Until the following seam lands, capture and
paint are usable but no production recovery acknowledgement is enabled.

Native unit tests need a dev dependency on the existing workspace
`phux-protocol` crate. The tests are included from
`tests/native/presentation.rs` by `src/presentation.rs`. The integration parent
owns the manifest and module wiring.

## What the pinned GPUI source actually guarantees

Paths below are relative to `toolchain/gpuix/zed/crates`:

| Source | Observation |
| --- | --- |
| `gpui/src/window.rs:1624-1653` | Pending `on_next_frame` callbacks run before the next draw/present. They carry no previous drawable success result. |
| `gpui/src/window.rs:2401-2438` | `on_next_frame` queues work; test support can run it with `simulate_next_frame` without presentation. |
| `gpui/src/window.rs:3085-3100` | `present` calls `PlatformWindow::draw` without a success receipt. |
| `gpui/src/platform.rs:821-875` | `PlatformWindow` exposes activity, not visibility/minimization; `draw` returns `()`. |
| `gpui_macos/src/window.rs:1906-1908` | AppKit delegates draw to the Metal renderer without returning a result. |
| `gpui_macos/src/window.rs:2706-2719` | Native occlusion state is available internally, but only starts/stops the display link. |
| `gpui_apple/src/metal_renderer.rs:447-487` | Missing drawable or render failure logs and returns; success only schedules presentation. |
| `gpui_apple/src/metal_renderer.rs:521-529` | A command-buffer completion handler recycles buffers; it does not report drawable presentation. |
| `gpui/src/platform.rs:1013-1037` | Screenshot/offscreen rendering explicitly does not present to the display. |

Window activity is not visibility: an unfocused window may be visible. Sleeping
until another frame, recording a successful text paint, or receiving GPU command
completion is not proof that a drawable reached the visible native window.

## Bounded missing platform seam

Implement this inside the pinned GPUI/Metal source, then expose it through GPUI's
normal Rust API; no application-specific API belongs in the GPUIX registry:

1. Expose native visibility from AppKit's real window: `isVisible`,
   `!isMiniaturized`, and visible occlusion state. Headless/test windows return
   unavailable, never visible. Query on the UI thread, at submission and completion.
2. Associate callbacks registered during successful paint with the **exact scene
   serial and drawable** submitted by `Window::present`. Reject failed draws,
   missing drawables, replaced scenes, closed windows and offscreen rendering.
3. Bridge Metal drawable presentation notification back to GPUI's UI executor.
   A command-buffer completion callback alone is insufficient; use the drawable's
   presented callback/time and preserve failure information. Never wait for GPU
   completion or join a driver on the UI thread.
4. The native host checks the live weak root, successful observation, visible
   unclipped terminal bounds, exact frame and matching window before minting
   `Presented`. `Ticket::acknowledge` then performs its serialized revalidation.

This requires coordinated edits to GPUI core, its AppKit window and Metal
renderer. A host-only timer or `on_next_frame` wrapper cannot provide this evidence.

## Verification boundary

The embedded tests use the real runtime, engine, bootstrap frames, publications
and Unknown-delivery correlations. They cover acquisition/cancellation,
connection replacement, view removal, root/target/window replacement, newer
same-connection fences, publication replacement, scrollback, multiview recovery,
receipt identity and serialized competing input. They exercise the private
conditional-clear core directly; synthetic receipt correlation is explicitly
not a GPU acceptance test.

No visible-window presentation success is claimed. Before enabling recovery,
extend the existing native child-process harness with visible, hidden, minimized,
offscreen, failed-draw and cancelled-callback cases, bounded artifact deadlines
and guaranteed child reaping. A genuine visible drawable must clear the fence;
all other cases must leave it. This remaining test is blocked on the platform
receipt seam rather than replaced with a boolean fixture acknowledgement.
