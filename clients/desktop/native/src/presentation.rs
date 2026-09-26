//! Native-only recovery tickets. A paint submission is not a presentation receipt.

use std::rc::{Rc, Weak};
use std::sync::Arc;

use gpuix_native::native_extensions::gpui::{App, Window};
use phux_client_runtime::{
    Client, ViewId,
    control::{ControlPlane, ProjectionFence, Status},
    publication::GridFrame,
};

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

/// Owned by one native terminal element, never by a pending closure.
#[derive(Default)]
pub(crate) struct Surface {
    lifetime: Rc<()>,
    window: Option<u64>,
}

impl Surface {
    /// Call before changing any target prop, and on destroy/root replacement.
    /// Replacing the allocation avoids wrapping epochs or reused element IDs.
    pub(crate) fn invalidate(&mut self) {
        self.lifetime = Rc::new(());
    }

    pub(crate) fn acquire(
        &mut self,
        handle: &str,
        terminal: &str,
        view: ViewId,
        window: &Window,
        cx: &App,
    ) -> Result<Ticket, Rejection> {
        let window_id = window.window_handle().window_id().as_u64();
        self.bind_window(window_id);
        with_registered(handle, cx, |control| {
            Ticket::capture(handle, terminal, view, window_id, &self.lifetime, control)
        })
    }

    fn bind_window(&mut self, window: u64) {
        if self.window != Some(window) {
            self.invalidate();
            self.window = Some(window);
        }
    }
}

/// Holds the exact immutable frame used by the painter, without a runtime lease
/// whose last drop might join the driver on the UI thread.
pub(crate) struct Ticket {
    handle: String,
    view: ViewId,
    window: u64,
    lifetime: Weak<()>,
    connection_epoch: u64,
    fence: Option<ProjectionFence>,
    frame: Arc<GridFrame>,
}

impl Ticket {
    fn capture(
        handle: &str,
        terminal: &str,
        view: ViewId,
        window: u64,
        lifetime: &Rc<()>,
        control: &ControlPlane,
    ) -> Result<Self, Rejection> {
        let frame = current_frame(control, view)?;
        if phux_client_ffi::projection::id::encode(&frame.terminal_id) != terminal {
            return Err(Rejection::WrongTerminal);
        }
        let frame = recovery_frame(control, view, frame)?;
        Ok(Self {
            handle: handle.to_owned(),
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

    /// Not callable in production until GPUI supplies a real presentation
    /// receipt. In particular, neither canvas paint nor on_next_frame can mint
    /// Presented. See ../PRESENTATION.md for the bounded platform seam.
    pub(crate) fn acknowledge(self, presented: Presented, cx: &App) -> Result<(), Rejection> {
        self.check_receipt(&presented)?;
        with_registered(&self.handle, cx, |control| {
            self.acknowledge_current(control)
        })
    }

    fn check_receipt(&self, presented: &Presented) -> Result<(), Rejection> {
        if self.window != presented.window {
            return Err(Rejection::WrongWindow);
        }
        if !Weak::ptr_eq(&self.lifetime, &presented.lifetime) {
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

/// No production constructor exists. The platform must eventually mint this
/// only for a successfully presented drawable in a visible, non-minimized native
/// window, correlated with the successful terminal paint and its live root.
/// Keeping the fields private makes the missing platform evidence fail closed.
pub(crate) struct Presented {
    window: u64,
    lifetime: Weak<()>,
    frame: Arc<GridFrame>,
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
    let replica = engine
        .view_replica_info(view)
        .map_err(|_| Rejection::StaleView)?;
    let frame = control
        .publication()
        .acquire_view(view)
        .ok_or(Rejection::Unavailable)?;
    if !engine.input_ready(&frame.terminal_id) {
        return Err(Rejection::Unavailable);
    }
    let published = (frame.stream_id, frame.bootstrap_id, frame.last_seq);
    let authoritative = (replica.stream_id, replica.bootstrap_id, replica.last_seq);
    if published != authoritative {
        return Err(Rejection::StalePublication);
    }
    Ok(frame)
}

fn with_registered<T>(
    handle: &str,
    cx: &App,
    operation: impl FnOnce(&mut ControlPlane) -> Result<T, Rejection>,
) -> Result<T, Rejection> {
    let registry = phux_client_ffi::napi::initialize();
    let client = registry
        .client(handle)
        .map_err(|_| Rejection::StaleHandle)?;
    let mut current = None;
    let result = client.with_control(|control| {
        // Registry handles are globally non-reused and connect only once. The
        // second lookup validates disposal inside the same owner turn as ack.
        current = Some(
            registry
                .client(handle)
                .map_err(|_| Rejection::StaleHandle)?,
        );
        operation(control)
    });
    release_off_thread(client, current, cx);
    result
}

fn release_off_thread(client: Client, current: Option<Client>, cx: &App) {
    // Registry close can race this lease. A final Client drop can join its
    // driver, so even short-lived lookup leases must leave the UI thread.
    cx.background_executor()
        .spawn(async move { drop((client, current)) })
        .detach();
}

#[cfg(test)]
#[path = "../../tests/native/presentation.rs"]
mod tests;
