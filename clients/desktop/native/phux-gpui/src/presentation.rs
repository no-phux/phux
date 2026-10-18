//! Native-only recovery tickets. A paint submission is not a presentation receipt.
//!
//! The terminal holds `Arc<Client>`. A ticket and its drawable-presented
//! callback hold only `Weak<Client>`. Upgrading that weak reference for the
//! acknowledgement, then dropping the upgrade off the UI thread, is the only
//! lease a receipt takes. The paint path borrows the entity's `Arc` and never
//! drops a `Client`.

use std::cell::RefCell;
use std::rc::{Rc, Weak as RcWeak};
use std::sync::{Arc, Weak};

use gpui::{self, App, Window};
use phux_client_runtime::{
    Client, ViewId,
    control::{ControlPlane, ProjectionFence, Status},
    publication::{GridFrame, TerminalPublication},
};
use phux_protocol::ResourceId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rejection {
    StaleHandle,
    Detached,
    Unavailable,
    StaleView,
    WrongTerminal,
    StalePublication,
    StaleSurface,
    WrongWindow,
    NotAtTail,
    NoFence,
    StaleFence,
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

/// Owned by one native terminal view, never by a pending closure.
#[derive(Default)]
pub(crate) struct Surface {
    lifetime: Rc<()>,
    window: Option<u64>,
    /// The last frame proven current for this connection. A later draw reuses
    /// it without an owner-thread turn while the publication is still that
    /// exact frame. A new publication, reconnect, detach, or prediction fence
    /// takes the full check.
    validated: RefCell<Option<ValidatedFrame>>,
}

struct ValidatedFrame {
    epoch: u64,
    frame: Arc<GridFrame>,
    slot: TerminalPublication,
}

impl Surface {
    /// Call when the view is destroyed or rebound. Replacing the allocation
    /// avoids wrapping epochs or reused element ids.
    pub(crate) fn invalidate(&mut self) {
        self.lifetime = Rc::new(());
        self.validated = RefCell::new(None);
    }

    /// Borrow `client`. This does not clone it, so the paint path cannot be
    /// the drop that joins the runtime driver.
    pub(crate) fn acquire(
        &mut self,
        client: &Arc<Client>,
        terminal: &ResourceId,
        view: ViewId,
        window: &Window,
    ) -> Result<Ticket, Rejection> {
        let window_id = window.window_handle().window_id().as_u64();
        self.bind_window(window_id);
        let weak = Arc::downgrade(client);
        client.with_control(|control| {
            Ticket::capture(
                weak,
                terminal,
                view,
                window_id,
                &self.lifetime,
                &self.validated,
                control,
            )
        })
    }

    fn bind_window(&mut self, window: u64) {
        if self.window != Some(window) {
            self.invalidate();
            self.window = Some(window);
        }
    }
}

/// Holds the exact immutable frame used by the painter. The client is weak:
/// a pending receipt must not keep the runtime alive, and must not drop the
/// last `Client` on the UI thread.
pub(crate) struct Ticket {
    client: Weak<Client>,
    view: ViewId,
    window: u64,
    lifetime: RcWeak<()>,
    connection_epoch: u64,
    fence: Option<ProjectionFence>,
    frame: Arc<GridFrame>,
}

impl Ticket {
    fn capture(
        client: Weak<Client>,
        terminal: &ResourceId,
        view: ViewId,
        window: u64,
        lifetime: &Rc<()>,
        validated: &RefCell<Option<ValidatedFrame>>,
        control: &ControlPlane,
    ) -> Result<Self, Rejection> {
        let frame = if let Some(frame) = reused_frame(control, terminal, &validated.borrow()) {
            frame
        } else {
            let frame = current_frame(control, view)?;
            if frame.terminal_id != *terminal {
                return Err(Rejection::WrongTerminal);
            }
            let frame = recovery_frame(control, view, frame)?;
            remember_frame(control, view, &frame, &mut validated.borrow_mut());
            frame
        };
        Ok(Self {
            client,
            view,
            window,
            lifetime: Rc::downgrade(lifetime),
            connection_epoch: control.connection_epoch(),
            fence: control.projection_fence(&frame.terminal_id),
            frame,
        })
    }

    pub(crate) fn frame(&self) -> Arc<GridFrame> {
        Arc::clone(&self.frame)
    }

    /// The connection epoch [`Self::frame`] was proven current under.
    pub(crate) const fn connection_epoch(&self) -> u64 {
        self.connection_epoch
    }

    /// Revalidate this ticket against a drawable-presented receipt. Canvas paint
    /// and `on_next_frame` cannot mint [`Presented`].
    pub(crate) fn acknowledge(self, presented: Presented, cx: &App) -> Result<(), Rejection> {
        self.check_receipt(&presented)?;
        let client = self.client.upgrade().ok_or(Rejection::StaleHandle)?;
        let result = client.with_control(|control| self.acknowledge_current(control));
        release_off_thread(client, cx);
        result
    }

    fn check_receipt(&self, presented: &Presented) -> Result<(), Rejection> {
        if self.window != presented.window {
            return Err(Rejection::WrongWindow);
        }
        if !RcWeak::ptr_eq(&self.lifetime, &presented.lifetime) {
            return Err(Rejection::StaleSurface);
        }
        if !Arc::ptr_eq(&self.frame, &presented.frame) {
            return Err(Rejection::StalePublication);
        }
        Ok(())
    }

    fn acknowledge_current(&self, control: &mut ControlPlane) -> Result<(), Rejection> {
        // Hold the upgraded root through revalidation and the conditional clear.
        // Rc confines destruction, target changes and this callback to the UI thread.
        let _lifetime = self.lifetime.upgrade().ok_or(Rejection::StaleSurface)?;
        let fence = self.fence.ok_or(Rejection::NoFence)?;
        if control.connection_epoch() != self.connection_epoch {
            return Err(Rejection::StaleFence);
        }
        let current = current_frame(control, self.view)?;
        if !Arc::ptr_eq(&current, &self.frame) {
            return Err(Rejection::StalePublication);
        }
        if !self.frame.scrollbar.at_tail() {
            return Err(Rejection::NotAtTail);
        }
        if !control.acknowledge_projection_if(&self.frame.terminal_id, fence) {
            return Err(Rejection::StaleFence);
        }
        Ok(())
    }
}

/// Minted only from [`schedule_recovery`], after GPUI reports that the exact
/// drawable was presented on a still-visible window.
pub(crate) struct Presented {
    window: u64,
    lifetime: RcWeak<()>,
    frame: Arc<GridFrame>,
}

pub(crate) fn schedule_recovery(
    ticket: Ticket,
    painted: &Arc<GridFrame>,
    bounds: gpui::Bounds<gpui::Pixels>,
    window: &mut Window,
    _cx: &mut App,
) {
    if ticket.fence.is_none() || !Arc::ptr_eq(painted, &ticket.frame) {
        return;
    }
    if !visible_unclipped(bounds, window) {
        return;
    }
    let frame = Arc::clone(painted);
    let lifetime = ticket.lifetime.clone();
    let expected_window = ticket.window;
    crate::platform::on_presented(window, move |window, cx| {
        if window.window_handle().window_id().as_u64() != expected_window {
            return;
        }
        if !visible_unclipped(bounds, window) {
            return;
        }
        let presented = Presented {
            window: expected_window,
            lifetime,
            frame,
        };
        // A rejected receipt leaves the fence set. There is no second acknowledgement path.
        let _ = ticket.acknowledge(presented, cx);
    });
}

fn visible_unclipped(bounds: gpui::Bounds<gpui::Pixels>, window: &Window) -> bool {
    crate::platform::visible(window) && bounds_cover(window.content_mask().bounds, bounds)
}

fn bounds_cover(mask: gpui::Bounds<gpui::Pixels>, bounds: gpui::Bounds<gpui::Pixels>) -> bool {
    if bounds.size.width <= gpui::px(0.) || bounds.size.height <= gpui::px(0.) {
        return false;
    }
    let hit = mask.intersect(&bounds);
    hit.origin == bounds.origin && hit.size == bounds.size
}

fn reused_frame(
    control: &ControlPlane,
    terminal: &ResourceId,
    validated: &Option<ValidatedFrame>,
) -> Option<Arc<GridFrame>> {
    if control.status() != Status::Attached || control.projection_fence(terminal).is_some() {
        return None;
    }
    let validated = validated.as_ref()?;
    if control.connection_epoch() != validated.epoch || validated.frame.terminal_id != *terminal {
        return None;
    }
    // `acquire` returns without an owner turn when this frame is current. A
    // deferred projection is stale, so it falls through and catches up.
    let frame = validated.slot.acquire()?;
    Arc::ptr_eq(&frame, &validated.frame).then_some(frame)
}

fn remember_frame(
    control: &ControlPlane,
    view: ViewId,
    frame: &Arc<GridFrame>,
    validated: &mut Option<ValidatedFrame>,
) {
    let Some(slot) = control.publication().view_slot(view) else {
        *validated = None;
        return;
    };
    *validated = Some(ValidatedFrame {
        epoch: control.connection_epoch(),
        frame: Arc::clone(frame),
        slot,
    });
}

fn recovery_frame(
    control: &ControlPlane,
    view: ViewId,
    frame: Arc<GridFrame>,
) -> Result<Arc<GridFrame>, Rejection> {
    if control.projection_fence(&frame.terminal_id).is_none() {
        return Ok(frame);
    }
    // Predictions modify publication cells without changing replica sequence.
    // A recovery ticket must render authoritative content, not predictive echo.
    control
        .engine()
        .ok_or(Rejection::Unavailable)?
        .clear_view_predictions(view)
        .map_err(|_| Rejection::StaleView)?;
    current_frame(control, view)
}

fn current_frame(control: &ControlPlane, view: ViewId) -> Result<Arc<GridFrame>, Rejection> {
    if control.status() != Status::Attached {
        return Err(Rejection::Detached);
    }
    let engine = control.engine().ok_or(Rejection::Unavailable)?;
    // A retained publication is not evidence of membership in a replaced engine.
    // One owner turn answers membership, readiness, the replica position and
    // the frame published for it (caught up first if it was deferred): this
    // runs per terminal per window draw, on the UI thread.
    let presented = engine
        .present_view(view)
        .map_err(|_| Rejection::StaleView)?;
    let replica = presented.replica;
    let frame = presented.frame.ok_or(Rejection::Unavailable)?;
    if !replica.input_ready {
        return Err(Rejection::Unavailable);
    }
    let published = (frame.stream_id, frame.bootstrap_id, frame.last_seq);
    let authoritative = (replica.stream_id, replica.bootstrap_id, replica.last_seq);
    if published != authoritative {
        return Err(Rejection::StalePublication);
    }
    Ok(frame)
}

fn release_off_thread(client: Arc<Client>, cx: &App) {
    // The upgraded lease may be the last `Client`. Dropping it here would join
    // the runtime driver on the UI thread, including from a presentation callback.
    cx.background_executor()
        .spawn(async move { drop(client) })
        .detach();
}

#[cfg(test)]
mod tests;
