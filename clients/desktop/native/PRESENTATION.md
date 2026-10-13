---
audience: agents, contributors
stability: evolving
last-reviewed: 2026-10-13
---

# Native presentation acknowledgement

**TL;DR.** Unknown-delivery recovery clears only after the platform reports
that the exact drawable was presented on a still-visible window. Paint,
`on_next_frame`, and command-buffer completion do not mint `Presented`.

The product contract lives in [desktop architecture](../../../docs/architecture/desktop.md#frames-wakes-and-input).

## Ticket and lifetime

`Surface` belongs to one terminal view. `Surface::acquire` borrows the view's
`Arc<Client>` and takes the terminal id, view id and window. It does not clone
the client, so the paint path cannot be the drop that joins the runtime driver.
The ticket retains the actual `Arc<GridFrame>`, connection epoch, exact
`ProjectionFence`, window identity, a weak surface allocation and
`Weak<Client>`. It does not hold a strong client.

One `Client::with_control` turn proves engine membership, attached status,
readiness and publication `(stream, bootstrap, sequence)` agreement before
capturing frame and fence. Membership, readiness, the replica position and the
frame come from a single owner turn, `EngineHandle::present_view`. Painting
hands that proven frame and epoch to input (`TerminalInput::presented_at`)
without asking again; input itself still revalidates live.
`ControlPlane::input_ready` is unusable here because it is correctly false
while delivery is fenced. Recovery capture also clears predictive echo and
reacquires the publication, since predictions change cells without changing
the replica sequence.

`Surface::invalidate` replaces the allocation when the view moves to another
window. Dropping the view drops the allocation, so a pending receipt cannot
acknowledge a destroyed surface. Node ids never establish identity.

Only a receipt carrying the same window, allocation and exact frame reaches
acknowledgement. The callback upgrades `Weak<Client>`. If the client is gone
the receipt is `StaleHandle` and does not resurrect the runtime. The upgraded
`Arc` is dropped on GPUI's background executor so a last `Client` drop never
joins the driver on the UI thread. In one control-owner turn the completion
path rechecks root liveness, connection, engine/view, publication identity,
tail position and the conditional delivery fence. A scrolled-back view cannot
clear the fence.

## Platform receipt

The parent framework patch preserves `Window::on_drawable_presented` and
`Window::native_visibility` on gpui-pre 0.3.8. Paint-time callbacks are bound
to that submission's scene. The terminal schedules recovery only for a
successful, unclipped paint of the ticket's frame; the callback rechecks
visibility and bounds, then `Ticket::acknowledge` revalidates on the control
owner.

## Verification boundary

`tests/native/presentation.rs` (included by `src/presentation.rs`) uses the
real runtime, engine, bootstrap frames and delivery correlations to cover
acquisition, cancellation, connection/view/window replacement, newer fences,
publication replacement, scrollback, multiview recovery, a wrong terminal id,
and that a pending ticket does not retain `Arc<Client>`. Synthetic receipt
correlation is not GPU acceptance: a visible-window harness for hidden,
minimized, offscreen, failed-draw and cancelled-callback cases is still
required.
