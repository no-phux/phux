---
audience: agents, contributors
stability: evolving
last-reviewed: 2026-09-26
---

# Native presentation acknowledgement

**TL;DR.** Unknown-delivery recovery clears only after Metal reports that the
exact drawable was presented on a still-visible window. Paint, `on_next_frame`,
and command-buffer completion do not mint `Presented`.

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
have no error, and cover the terminal's actual visible bounds. Only then does
`schedule_recovery` register that ticket with `Window::on_drawable_presented`.
Dropping the ticket on failed, empty, or clipped paint does nothing.
Do not construct a `Presented` value in `terminal.rs`; its private fields stay
inside the presentation module.

Native unit tests need a dev dependency on the existing workspace
`phux-protocol` crate. The tests are included from
`tests/native/presentation.rs` by `src/presentation.rs`. The integration parent
owns the manifest and module wiring.

## Platform receipt

`toolchain/patches/0005-drawable-presented.patch` is applied inside the pinned
Zed checkout by source bootstrap. It is not a GPUIX registry API.

1. `NativeVisibility` reads AppKit `isVisible`, `!isMiniaturized`, and occlusion
   `Visible`. Headless and test windows stay `Unavailable`.
2. `Window::on_drawable_presented` registers paint-time callbacks. `present`
   binds them to that submission's scene serial and drawable id.
3. Metal `addPresentedHandler` reports that drawable's id and whether
   `presentedTime` is nonzero. The handler hops to the main queue and does not
   wait for GPU completion. A missing drawable, failed render, replaced scene,
   closed window, or offscreen capture drops the callback.
4. The terminal schedules recovery only for a successful, unclipped paint of the
   ticket's frame. The callback rechecks visibility and bounds, then
   `Ticket::acknowledge` revalidates on the control owner.

## Verification boundary

The embedded tests use the real runtime, engine, bootstrap frames, publications
and Unknown-delivery correlations. They cover acquisition/cancellation,
connection replacement, view removal, root/target/window replacement, newer
same-connection fences, publication replacement, scrollback, multiview recovery,
receipt identity and serialized competing input. They exercise the private
conditional-clear core directly; synthetic receipt correlation is explicitly
not a GPU acceptance test.

Recovery is wired to that receipt. A visible-window child-process harness for
hidden, minimized, offscreen, failed-draw, and cancelled-callback cases is still
the acceptance proof that a genuine drawable clears the fence and every other
case leaves it. Synthetic receipt correlation is not that proof.
