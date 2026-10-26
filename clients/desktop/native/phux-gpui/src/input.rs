//! Native terminal input. The painter supplies identity and geometry; every
//! action upgrades the view's `Weak<Client>` and does not take a listener.

mod clipboard;
mod composition;
mod fit;
mod handler;
mod keys;
mod link;
mod pointer;
mod typing;

#[cfg(test)]
mod delivery;
#[cfg(test)]
mod tests;

pub use clipboard::{HeldPaste, copy_too_large, paste_needs_confirmation};
use fit::{FitProposal, PendingFit, next_fit_token};
pub use keys::event as key_event;
pub(crate) use link::{LinkAdmit, admit_path, admit_url};

use composition::Composition;
use gpui::{
    self, App, Bounds, ClipboardItem, Context, EntityInputHandler, EventEmitter, FocusHandle,
    KeyDownEvent, KeyUpEvent, Pixels, Point, UTF16Selection, Window, WindowId,
};
use phux_client_runtime::control::{ControlPlane, TerminalResizeOutcome};
use phux_client_runtime::{Client, ViewId, publication::GridFrame};
use phux_protocol::{
    ResourceId,
    input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey},
};
use std::{
    collections::BTreeMap,
    ops::Range,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// Geometry already computed by the painter, in logical window pixels.
#[derive(Clone, Copy, Debug)]
pub struct InputMetrics {
    pub bounds: Bounds<Pixels>,
    pub cell_width: Pixels,
    pub line_height: Pixels,
    /// Window scale the painter snapped this grid to.
    pub scale: f32,
    pub cursor_bounds: Bounds<Pixels>,
    /// How far the painter drew the grid below its whole-row position:
    /// zero, or negative while a smooth scroll sits between rows.
    pub shift: Pixels,
}

/// Connection and replica a painted frame was admitted under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    epoch: u64,
    stream: u64,
    bootstrap: u64,
}

impl Identity {
    fn of(epoch: u64, frame: &GridFrame) -> Self {
        Self {
            epoch,
            stream: frame.stream_id,
            bootstrap: frame.bootstrap_id,
        }
    }
}

/// Whether a painted frame may arm this bound input.
#[derive(Debug, PartialEq, Eq)]
enum Admit {
    Accept(Identity),
    WrongTerminal,
    Replaced,
}

fn admit_presentation(
    terminal: &ResourceId,
    previous: Option<Identity>,
    epoch: u64,
    frame: &GridFrame,
) -> Admit {
    if &frame.terminal_id != terminal {
        return Admit::WrongTerminal;
    }
    let identity = Identity::of(epoch, frame);
    if previous.is_some_and(|previous| previous != identity) {
        Admit::Replaced
    } else {
        Admit::Accept(identity)
    }
}

fn upgrade_client(client: &Weak<Client>) -> Result<Arc<Client>, InputError> {
    client.upgrade().ok_or(InputError::StaleHandle)
}

/// A paste or open the terminal refused, for the shell to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputNotice {
    /// Notification title.
    pub title: &'static str,
    /// Notification body.
    pub body: String,
}

/// A clipboard result the shell has to finish. File bytes are not pasted here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// Show a notice. Nothing was pasted.
    Notice(InputNotice),
    /// Upload these local bytes, then paste only the server path.
    Upload(crate::attach::UploadBatch),
    /// A paste [`paste_needs_confirmation`] flags, held until the person
    /// confirms it. Nothing was pasted; [`TerminalInput::paste_confirmed`]
    /// sends it.
    ConfirmPaste(HeldPaste),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputError {
    StaleHandle,
    WrongFocus,
    StalePresentation,
    NotReady,
    InvalidRange,
    /// The selection is too large for the clipboard. Nothing was copied.
    TooLarge,
    Engine(String),
}

/// Result is local enqueue/refusal, never an assertion of server delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyDisposition {
    Platform,
    Consumed,
}

impl EventEmitter<InputEvent> for TerminalInput {}

pub struct TerminalInput {
    client: Weak<Client>,
    view: ViewId,
    terminal: ResourceId,
    window: WindowId,
    focus: FocusHandle,
    identity: Option<Identity>,
    metrics: Option<InputMetrics>,
    dimensions: (u16, u16),
    composition: Composition,
    pending_key: Option<KeyEvent>,
    pressed: BTreeMap<PhysicalKey, KeyEvent>,
    gesture: u64,
    /// The cell and fifth of a cell the last selection drag landed on.
    drag_cell: Option<(u16, u16, u8)>,
    /// Where a selection drag is held past the grid, while it autoscrolls.
    autoscroll: Option<(Point<Pixels>, gpui::Modifiers)>,
    /// Which autoscroll timer is current; an older one stops.
    autoscroll_run: u64,
    reported_button: Option<phux_protocol::input::mouse::MouseButton>,
    /// OSC 8 and row URLs for the frame last painted, plus a Command-click
    /// captured from that frame.
    painted_links: link::PaintedLinks,
    /// The cell the last motion event already delivered. A later move inside
    /// that cell is not a new event.
    last_motion_cell: Option<(u16, u16)>,
    /// Whole wheel rows not yet reported to a mouse-reporting program.
    scroll_remainder: f32,
    /// The view offset the last local scroll landed on, and the part of a row
    /// above it the painter scrolls past.
    scroll_fraction: Option<(u64, f32)>,
    /// Whether the program reported the wheel at a replica's
    /// `(stream, bootstrap, sequence)`.
    wheel_mode: Option<((u64, u64, u64), bool)>,
    option_as_alt: bool,
    /// Registry host this window attaches to. Absent is the local server.
    /// A satellite terminal is remote even when this is absent.
    window_remote: Option<String>,
    last_error: Option<InputError>,
    /// The size and cell pitch this view last proposed.
    fitted: Option<FitProposal>,
    /// Cell pitch last stored on the local replica. An unchanged paint does
    /// not touch the engine, so it does not republish.
    applied_cell: Option<(u16, u16)>,
    /// A proposal waiting to hold still for `FIT_SETTLE`.
    settling: Option<PendingFit>,
    /// Distinguishes this input from a later one bound to the same view.
    fit_token: u64,
    fit_active: bool,
    /// Output sequence of the last frame painted, and the key-to-paint probe.
    painted_seq: u64,
    key_probe: crate::perf::KeyProbe,
}

impl TerminalInput {
    pub fn new(
        client: Weak<Client>,
        view: ViewId,
        terminal: ResourceId,
        focus: FocusHandle,
        window: &Window,
    ) -> Self {
        Self {
            client,
            view,
            terminal,
            window: window.window_handle().window_id(),
            focus,
            identity: None,
            metrics: None,
            dimensions: (0, 0),
            composition: Composition::default(),
            pending_key: None,
            pressed: BTreeMap::new(),
            gesture: 0,
            drag_cell: None,
            autoscroll: None,
            autoscroll_run: 0,
            reported_button: None,
            painted_links: link::PaintedLinks::default(),
            last_motion_cell: None,
            scroll_remainder: 0.0,
            scroll_fraction: None,
            wheel_mode: None,
            option_as_alt: false,
            window_remote: None,
            last_error: None,
            fitted: None,
            applied_cell: None,
            settling: None,
            fit_token: next_fit_token(),
            fit_active: false,
            painted_seq: 0,
            key_probe: crate::perf::KeyProbe::default(),
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }
    pub fn set_option_as_alt(&mut self, enabled: bool) {
        self.option_as_alt = enabled;
    }

    /// The registry host this window attaches to. Absent is the local server.
    pub fn set_window_remote(&mut self, remote: Option<String>) {
        self.window_remote = remote;
    }

    fn host(&self) -> crate::attach::TerminalHost {
        crate::attach::terminal_host(self.window_remote.as_deref(), &self.terminal)
    }

    pub fn last_error(&self) -> Option<&InputError> {
        self.last_error.as_ref()
    }
    pub fn preedit(&self) -> (&str, Range<usize>) {
        (&self.composition.text, self.composition.selected.clone())
    }

    /// Call only for a frame actually being painted, which the painter already
    /// proved current under `epoch` in this draw (a presentation ticket), so
    /// painting costs no owner round trip. Input itself still revalidates
    /// live. This does NOT acknowledge an unknown-delivery fence. That
    /// authority belongs to the presenter.
    pub fn presented_at(
        &mut self,
        epoch: u64,
        frame: &Arc<GridFrame>,
        metrics: InputMetrics,
    ) -> Result<(), InputError> {
        match admit_presentation(&self.terminal, self.identity, epoch, frame) {
            Admit::WrongTerminal => Err(InputError::StalePresentation),
            Admit::Replaced => {
                self.cancel();
                self.painted_links.forget();
                self.metrics = None;
                self.fitted = None;
                self.applied_cell = None;
                self.revoke_fit();
                self.key_probe.forget();
                Err(InputError::StalePresentation)
            }
            Admit::Accept(identity) => {
                self.painted_links.present(identity, Arc::clone(frame));
                self.identity = Some(identity);
                self.painted_seq = frame.last_seq;
                self.key_probe.painted(frame.last_seq);
                self.metrics = Some(metrics);
                self.dimensions = (frame.cols, frame.rows);
                Ok(())
            }
        }
    }

    pub fn cancel(&mut self) {
        self.composition.clear();
        self.pending_key = None;
        self.pressed.clear();
        self.gesture = 0;
        self.drag_cell = None;
        self.autoscroll = None;
        self.reported_button = None;
        self.painted_links.cancel();
        self.scroll_remainder = 0.0;
    }

    fn client(&self) -> Result<Arc<Client>, InputError> {
        upgrade_client(&self.client)
    }

    fn focused(&self, window: &Window, _: &App) -> bool {
        window.window_handle().window_id() == self.window
            && self.focus.is_focused(window)
            && window.is_window_active()
    }

    /// Focused in this window and painting the live frame. `input` also
    /// requires a terminal that accepts input.
    fn admit_focused(&self, window: &Window, cx: &App, input: bool) -> Result<(), InputError> {
        if !self.focused(window, cx) {
            return Err(InputError::WrongFocus);
        }
        self.with_current(input, |_| Ok(()))
    }

    fn live_frame(&self, control: &ControlPlane) -> Result<std::sync::Arc<GridFrame>, InputError> {
        let info = control
            .engine()
            .ok_or(InputError::NotReady)?
            .view_replica_info(self.view)
            .map_err(engine_error)?;
        let frame = control
            .publication()
            .acquire_view(self.view)
            .ok_or(InputError::StalePresentation)?;
        if frame.terminal_id != self.terminal
            || (info.stream_id, info.bootstrap_id) != (frame.stream_id, frame.bootstrap_id)
        {
            return Err(InputError::StalePresentation);
        }
        Ok(frame)
    }

    fn validate(&self, control: &ControlPlane, input: bool) -> Result<(), InputError> {
        let frame = self.live_frame(control)?;
        if self.identity != Some(Identity::of(control.connection_epoch(), &frame))
            || self.dimensions != (frame.cols, frame.rows)
        {
            return Err(InputError::StalePresentation);
        }
        if input && !input_ready(control, &self.terminal) {
            return Err(InputError::NotReady);
        }
        Ok(())
    }

    fn with_current<T>(
        &self,
        input: bool,
        operation: impl FnOnce(&mut ControlPlane) -> Result<T, InputError>,
    ) -> Result<T, InputError> {
        let client = self.client()?;
        client.with_control(|control| {
            self.validate(control, input)?;
            operation(control)
        })
    }

    fn record(&mut self, result: Result<(), InputError>) {
        self.last_error = result.err();
        if self.last_error.is_some() {
            self.cancel();
        }
    }
}

impl Drop for TerminalInput {
    fn drop(&mut self) {
        self.release_fit_claim();
    }
}

fn queued(success: bool) -> Result<(), InputError> {
    success.then_some(()).ok_or(InputError::NotReady)
}

/// Native guardians never accept terminal-input fallback, including when the
/// retained controller view is disconnected. The server owns the final gate
/// before a publication reaches this client.
pub fn native_owned(control: &ControlPlane, terminal: &ResourceId) -> bool {
    let Some(topology) = control.topology() else {
        return false;
    };
    let Some(engine) = control.engine() else {
        return false;
    };
    topology.agent_sessions.iter().any(|agent| {
        agent.parent.as_ref() == Some(terminal) && engine.has_native_agent_view(&agent.id)
    })
}

fn input_ready(control: &ControlPlane, terminal: &ResourceId) -> bool {
    if control
        .options()
        .attach_role
        .is_some_and(phux_protocol::wire::frame::RolePolicy::is_viewer)
    {
        return false;
    }
    !native_owned(control, terminal) && control.input_ready(terminal)
}

fn engine_error(error: phux_client_runtime::engine::EngineError) -> InputError {
    InputError::Engine(error.to_string())
}
