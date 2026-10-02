---
audience: agents, contributors
stability: evolving
last-reviewed: 2026-10-02
---

# Native presentation acknowledgement

**TL;DR.** Unknown-delivery recovery clears only after Metal reports that the
exact drawable was presented on a still-visible window. Paint, `on_next_frame`,
and command-buffer completion do not mint `Presented`.

The product contract lives in [desktop architecture](../../../docs/architecture/desktop.md#frames-wakes-and-input).

## Ticket and lifetime

`Surface` belongs to one terminal element. `Surface::acquire` takes the GPUI
window, client handle, terminal ID and view ID. Its ticket retains the actual
`Arc<GridFrame>`, connection epoch, exact `ProjectionFence`, window identity
and a weak surface allocation; it does not hold a `Client` shutdown lease.

One `Client::with_control` turn proves engine membership, attached status,
readiness and publication `(stream, bootstrap, sequence)` agreement before
capturing frame and fence. Membership, readiness, the replica position and the
frame come from a single owner turn, `EngineHandle::present_view`, which first
catches up a deferred projection, so they cannot disagree and validating costs
one owner round trip per terminal per draw. Painting hands that proven frame
and epoch to input (`TerminalInput::presented_at`) without asking again; input
itself still revalidates live. `ControlPlane::input_ready` is unusable here because
it is correctly false while delivery is fenced. Recovery capture also clears
predictive echo and reacquires the publication, since predictions change cells
without changing the replica sequence.

`Surface::invalidate` replaces the allocation. The terminal calls it before
applying `clientHandle`, `terminalId` or `viewId` props, on destroy, and when
the element moves to another window. Node IDs never establish identity.

Only a receipt carrying the same window, allocation and exact frame reaches
acknowledgement. In one control-owner turn the completion path rechecks the
handle, root liveness, connection, engine/view, publication identity, tail
position and the conditional delivery fence. A scrolled-back view cannot clear
the fence. Lookup leases are released on GPUI's background executor so a last
`Client` drop never joins the runtime driver on the paint thread.

## Platform receipt

`toolchain/patches/0005-drawable-presented.patch` is applied inside the pinned
Zed checkout by source bootstrap.

1. `NativeVisibility` reads AppKit `isVisible`, `!isMiniaturized` and occlusion
   `Visible`. Headless and test windows stay `Unavailable`.
2. `Window::on_drawable_presented` registers paint-time callbacks bound to that
   submission's scene serial and drawable id.
3. Metal `addPresentedHandler` reports that drawable and whether
   `presentedTime` is nonzero, hopping to the main queue. A missing drawable,
   failed render, replaced scene, closed window or offscreen capture drops the
   callback.
4. The terminal schedules recovery only for a successful, unclipped paint of
   the ticket's frame; the callback rechecks visibility and bounds, then
   `Ticket::acknowledge` revalidates on the control owner.

## Verification boundary

`tests/native/presentation.rs` (included by `src/presentation.rs`) uses the real
runtime, engine, bootstrap frames and delivery correlations to cover
acquisition, cancellation, connection/view/window replacement, newer fences,
publication replacement, scrollback, multiview recovery and competing input.
Synthetic receipt correlation is not GPU acceptance: a visible-window harness
for hidden, minimized, offscreen, failed-draw and cancelled-callback cases is
still required.
