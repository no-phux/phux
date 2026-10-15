//! Native terminal input. The painter supplies identity and geometry; every
//! action upgrades the view's `Weak<Client>` and does not take a listener.

mod composition;
mod keys;
mod link;
mod pointer;

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
    input::{
        focus::FocusEvent,
        key::{KeyAction, KeyEvent, ModSet, PhysicalKey},
    },
};
use std::{
    collections::BTreeMap,
    ops::Range,
    sync::{
        Arc, LazyLock, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// How long a pane's capacity must hold still before it becomes a PTY resize.
/// Layout settling, window drags and split drags produce bursts of sizes;
/// every intermediate resize makes a shell redraw its prompt into history.
/// A settling proposal requests frames, so the final size always goes out.
const FIT_SETTLE: Duration = Duration::from_millis(60);

/// Server `DEFAULT_CELL_PX` and the TUI `HOST_CELL_PX_FALLBACK`. Desktop must
/// not import that private constant. A first look at this pitch sends nothing.
const DEFAULT_CELL_PX: (u16, u16) = (8, 16);

/// `(cols, rows)` plus the integer cell pitch reported with them.
type FitProposal = ((u16, u16), (u16, u16));

/// A proposal waiting out [`FIT_SETTLE`], armed for one presentation generation.
#[derive(Clone, Copy)]
struct PendingFit {
    proposal: FitProposal,
    since: Instant,
    identity: Identity,
}

/// Token of the view that last painted as the visible size owner.
///
/// Hiding releases paint state and keeps this input, including a fit timer.
/// The timer delivers only while the token still matches, so a hidden former
/// owner cannot resize over the view that has since taken ownership.
fn fit_claims() -> &'static Mutex<BTreeMap<(usize, ResourceId), u64>> {
    static CLAIMS: LazyLock<Mutex<BTreeMap<(usize, ResourceId), u64>>> =
        LazyLock::new(|| Mutex::new(BTreeMap::new()));
    &CLAIMS
}

fn next_fit_token() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn claim_key(client: &Arc<Client>, terminal: &ResourceId) -> (usize, ResourceId) {
    (Arc::as_ptr(client) as usize, terminal.clone())
}

/// Whether a settled fit may still be delivered. `claimed` is the visible
/// size owner's token and `live` is the generation published now.
fn admit_delayed_fit(
    claimed: Option<u64>,
    token: u64,
    armed: Identity,
    live: Option<Identity>,
) -> bool {
    claimed == Some(token) && live == Some(armed)
}

/// Geometry already computed by the painter, in logical window pixels.
#[derive(Clone, Copy, Debug)]
pub struct InputMetrics {
    pub bounds: Bounds<Pixels>,
    pub cell_width: Pixels,
    pub line_height: Pixels,
    /// Window scale the painter snapped this grid to.
    pub scale: f32,
    pub cursor_bounds: Bounds<Pixels>,
}

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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputError {
    StaleHandle,
    WrongFocus,
    StalePresentation,
    NotReady,
    InvalidRange,
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
    reported_button: Option<phux_protocol::input::mouse::MouseButton>,
    /// OSC 8 and row URLs for the frame last painted, plus a Command-click
    /// captured from that frame.
    painted_links: link::PaintedLinks,
    /// The cell the last motion event already delivered. A later move inside
    /// that cell is not a new event.
    last_motion_cell: Option<(u16, u16)>,
    scroll_remainder: f32,
    option_as_alt: bool,
    /// Registry host this window attaches to. Absent is the local server.
    /// A satellite terminal is remote even when this is absent.
    window_remote: Option<String>,
    last_error: Option<InputError>,
    /// The size and cell pitch this view last proposed.
    fitted: Option<FitProposal>,
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
            reported_button: None,
            painted_links: link::PaintedLinks::default(),
            last_motion_cell: None,
            scroll_remainder: 0.0,
            option_as_alt: false,
            window_remote: None,
            last_error: None,
            fitted: None,
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

    fn window_active(&self, window: &Window) -> bool {
        window.is_window_active()
    }

    /// Call only for a frame actually being painted. This does NOT acknowledge
    /// an unknown-delivery fence. That authority belongs to the presenter.
    pub fn presented(
        &mut self,
        frame: &Arc<GridFrame>,
        metrics: InputMetrics,
    ) -> Result<(), InputError> {
        let client = self.client()?;
        let epoch = client.with_control(|control| {
            let current = self.live_frame(control)?;
            let epoch = control.connection_epoch();
            if Identity::of(epoch, frame) != Identity::of(epoch, &current) {
                return Err(InputError::StalePresentation);
            }
            Ok(epoch)
        })?;
        self.presented_at(epoch, frame, metrics)
    }

    /// [`Self::presented`] for a frame the painter already proved current
    /// under `epoch` in this draw (a presentation ticket), so painting costs
    /// no second owner round trip. Input itself still revalidates live.
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
                self.revoke_fit();
                self.key_probe.forget();
                Err(InputError::StalePresentation)
            }
            Admit::Accept(identity) => {
                self.painted_links.present(
                    link::FrameStamp::new(identity.epoch, frame),
                    Arc::clone(frame),
                );
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
        self.reported_button = None;
        self.painted_links.cancel();
        self.scroll_remainder = 0.0;
    }

    /// Propose this pane's cell capacity and integer cell pitch as the
    /// terminal's PTY geometry while this view owns it (desktop architecture,
    /// "Geometry"). The shell names one owner per terminal: its only view, or
    /// the view the user focused last. Other views crop or leave space.
    /// Proposals are edge-triggered: an unchanged proposal sends nothing, so
    /// views cannot oscillate. Gaining ownership or re-activating the window
    /// forgets the last proposal, so the owner reclaims a size another client
    /// changed. The first look also sends nothing when the grid already matches
    /// and the pitch is the server default of 8×16.
    ///
    /// A timer, not the next frame, sends the settled size: a background or
    /// occluded window may not paint again, and the resize must still go out.
    /// Hiding keeps this input and that timer. Delivery rechecks that this
    /// view is still the visible size owner of the generation that armed the
    /// proposal, so a hidden former owner cannot override the focused view.
    pub fn fit(&mut self, owner: bool, window: &Window, cx: &mut Context<Self>) {
        let active = self.window_active(window);
        let reactivated = active && !self.fit_active;
        self.fit_active = active;
        if !owner {
            self.fitted = None;
            self.revoke_fit();
            return;
        }
        // Claim before deciding whether a resize is needed, so a focused view
        // whose grid already fits still retires a hidden owner's pending fit.
        self.claim_fit();
        if reactivated {
            self.fitted = None;
        }
        let Some(metrics) = self.metrics else {
            return;
        };
        let Some(capacity) = capacity(metrics) else {
            return;
        };
        let cell = cell_px(metrics);
        let Some(proposal) = pending_fit(self.fitted, capacity, cell, self.dimensions) else {
            self.fitted = Some((capacity, cell));
            self.settling = None;
            return;
        };
        if self
            .settling
            .is_some_and(|pending| pending.proposal == proposal)
        {
            return;
        }
        let Some(identity) = self.identity else {
            self.settling = None;
            return;
        };
        self.settling = Some(PendingFit {
            proposal,
            since: Instant::now(),
            identity,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIT_SETTLE).await;
            let _ = this.update(cx, |state, _| state.flush_fit());
        })
        .detach();
    }

    /// Send the pending proposal once it has held still for `FIT_SETTLE`. A
    /// newer proposal restarts the clock and its own timer sends it.
    ///
    /// Admission is the current visible owner and the armed generation.
    /// Nothing here acknowledges an unknown-delivery fence. A viewer role
    /// is refused by the resize itself, which sends nothing.
    fn flush_fit(&mut self) {
        let Some(pending) = self.settling else {
            return;
        };
        if pending.since.elapsed() < FIT_SETTLE {
            return;
        }
        let Ok(client) = self.client() else {
            return;
        };
        let claimed = self.claimed_token(&client);
        let outcome = client.with_control(|control| {
            let live = self
                .live_frame(control)
                .ok()
                .map(|frame| Identity::of(control.connection_epoch(), &frame));
            if !admit_delayed_fit(claimed, self.fit_token, pending.identity, live) {
                return None;
            }
            let ((cols, rows), cell_px) = pending.proposal;
            Some(control.resize_terminal_with_cell(
                &self.terminal,
                cols.into(),
                rows.into(),
                Some(cell_px),
            ))
        });
        if outcome == Some(TerminalResizeOutcome::Queued) {
            self.fitted = Some(pending.proposal);
        }
        self.settling = None;
    }

    fn claim_fit(&self) {
        let Ok(client) = self.client() else {
            return;
        };
        fit_claims()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(claim_key(&client, &self.terminal), self.fit_token);
    }

    fn claimed_token(&self, client: &Arc<Client>) -> Option<u64> {
        fit_claims()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&claim_key(client, &self.terminal))
            .copied()
    }

    /// Drop a pending fit and release visible ownership when this input holds it.
    fn revoke_fit(&mut self) {
        self.settling = None;
        self.release_fit_claim();
    }

    fn release_fit_claim(&mut self) {
        let mut claims = fit_claims()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Ok(client) = self.client() {
            let key = claim_key(&client, &self.terminal);
            if claims.get(&key) == Some(&self.fit_token) {
                claims.remove(&key);
            }
            return;
        }
        claims.retain(|_, token| *token != self.fit_token);
    }

    fn client(&self) -> Result<Arc<Client>, InputError> {
        upgrade_client(&self.client)
    }

    fn focused(&self, window: &Window, _: &App) -> bool {
        window.window_handle().window_id() == self.window
            && self.focus.is_focused(window)
            && self.window_active(window)
    }

    fn current(&self, window: &Window, cx: &App) -> Result<(), InputError> {
        if !self.focused(window, cx) {
            return Err(InputError::WrongFocus);
        }
        self.with_current(false, |_| Ok(()))
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
        if self.identity != Some(Identity::of(control.connection_epoch(), &frame)) {
            return Err(InputError::StalePresentation);
        }
        if self.dimensions != (frame.cols, frame.rows) {
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

    fn ready(&self, window: &Window, cx: &App) -> Result<(), InputError> {
        if !self.focused(window, cx) {
            return Err(InputError::WrongFocus);
        }
        self.with_current(true, |_| Ok(()))
    }

    /// Painter calls on focus/blur and window activation changes. A blur clears
    /// local IME state even when the remote connection has already disappeared.
    pub fn focus_changed(
        &mut self,
        active: bool,
        window: &Window,
        cx: &App,
    ) -> Result<(), InputError> {
        if window.window_handle().window_id() != self.window {
            return Err(InputError::WrongFocus);
        }
        if !active {
            self.cancel();
        }
        if active && !self.focused(window, cx) {
            return Err(InputError::WrongFocus);
        }
        let event = if active {
            FocusEvent::Gained
        } else {
            FocusEvent::Lost
        };
        self.with_current(true, |control| {
            queued(control.send_focus(&self.terminal, event))
        })
    }

    /// Register on the focused terminal's bubbling key-down path. Stop GPUI
    /// propagation only for Consumed. Printable keys reach the OS text handler.
    pub fn key_down(
        &mut self,
        down: &KeyDownEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<KeyDisposition, InputError> {
        self.pending_key = None;
        if down.keystroke.modifiers.platform {
            return Ok(KeyDisposition::Platform);
        }
        if !self.focused(window, cx) {
            return Err(InputError::WrongFocus);
        }
        if !self.composition.text.is_empty() {
            return Ok(KeyDisposition::Platform);
        }
        let action = if down.is_held {
            KeyAction::Repeat
        } else {
            KeyAction::Press
        };
        let mut event = keys::event(&down.keystroke, action);
        event.mods.set(ModSet::CAPS_LOCK, window.capslock().on);
        if keys::uses_text(down, self.option_as_alt) {
            self.with_current(true, |_| Ok(()))?;
            self.pending_key = Some(event);
            self.key_probe.arm(self.painted_seq);
            return Ok(KeyDisposition::Platform);
        }
        self.with_current(true, |control| {
            queued(control.send_key(&self.terminal, event.clone()))
        })?;
        let echoed = self.predict_sent(&event);
        self.key_probe.arm(self.painted_seq);
        self.pressed.insert(event.key, event);
        if echoed {
            cx.notify();
        }
        Ok(KeyDisposition::Consumed)
    }

    pub fn key_up(
        &mut self,
        up: &KeyUpEvent,
        window: &Window,
        cx: &App,
    ) -> Result<KeyDisposition, InputError> {
        self.pending_key = None;
        let released = keys::event(&up.keystroke, KeyAction::Release).key;
        let Some(mut event) = self.pressed.remove(&released) else {
            return Ok(KeyDisposition::Platform);
        };
        if !self.focused(window, cx) {
            return Err(InputError::WrongFocus);
        }
        event.action = KeyAction::Release;
        event.mods = keys::modifiers(up.keystroke.modifiers);
        event.mods.set(ModSet::CAPS_LOCK, window.capslock().on);
        event.consumed_mods = ModSet::empty();
        event.text = None;
        self.with_current(true, |control| {
            queued(control.send_key(&self.terminal, event))
        })?;
        Ok(KeyDisposition::Consumed)
    }

    fn commit(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &Window,
        cx: &App,
    ) -> Result<(), InputError> {
        if !self.focused(window, cx) {
            return Err(InputError::WrongFocus);
        }
        self.composition
            .replace(range, text)
            .ok_or(InputError::InvalidRange)?;
        let committed = std::mem::take(&mut self.composition.text);
        self.composition.clear();
        if let Some(mut event) = self.pending_key.take() {
            event.consumed_mods = event.mods & (ModSet::SHIFT | ModSet::ALT);
            event.text = Some(committed);
            self.with_current(true, |control| {
                queued(control.send_key(&self.terminal, event.clone()))
            })?;
            self.predict_sent(&event);
            self.pressed.insert(event.key, event);
            return Ok(());
        }
        self.with_current(true, |control| {
            queued(control.send_text(&self.terminal, &committed))
        })?;
        self.publish_echo(&committed);
        Ok(())
    }

    /// Paint a local guess for this key before the server can echo it.
    ///
    /// The runtime predictor decides what is safe to draw, including hiding
    /// alternate-screen guesses until the application proves it echoes.
    /// A local socket still predicts: it may be the near end of a tunnel.
    fn predict_sent(&self, event: &KeyEvent) -> bool {
        let Some(text) = echo_guess(event) else {
            return false;
        };
        self.publish_echo(&text)
    }

    fn publish_echo(&self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let Some(client) = self.client.upgrade() else {
            return false;
        };
        let Some(engine) = client.engine() else {
            return false;
        };
        engine
            .predict_view_text(self.view, text.to_owned())
            .unwrap_or(false)
    }

    /// The user's own clipboard paste, trusted (the runtime's
    /// `apply_user_paste`). The sole FFI event owner receives InputDelivery
    /// for a returned correlation; `None` is one atomic paste event too large
    /// for acknowledged input, with no receipt. Neither the native adapter nor
    /// its caller may auto-retry Unknown.
    pub fn paste_text(
        &mut self,
        text: &str,
        window: &Window,
        cx: &App,
    ) -> Result<Option<u64>, InputError> {
        let focused = self.focused(window, cx);
        self.paste(text, focused)
    }

    /// The shell's Paste command, addressed to this terminal by placement.
    /// Keyboard focus may sit in an overlay over it, such as the find bar,
    /// whose own Command-V pastes into the field instead.
    pub fn paste_requested(&mut self, text: &str) -> Result<Option<u64>, InputError> {
        self.paste(text, true)
    }

    fn paste(&mut self, text: &str, focused: bool) -> Result<Option<u64>, InputError> {
        self.cancel();
        let client = self.client()?;
        let delivery = client.with_control(|control| {
            let admission = if focused {
                self.validate(control, true)
            } else {
                Err(InputError::WrongFocus)
            };
            admission.map(|()| {
                let had_events = control.has_events();
                let delivery = control.apply_user_paste(&self.terminal, text);
                (delivery, !had_events && control.has_events())
            })
        });
        // Refusal only publishes a receipt; it cannot enqueue a paste after a
        // reconnect. Admission and successful enqueue above share one lock.
        let (delivery, resolved) = delivery.unwrap_or_else(|error| {
            (
                Some(client.refuse_acknowledged_input(&format!("native paste refused: {error:?}"))),
                false,
            )
        });
        if resolved {
            client.wake();
        }
        Ok(delivery)
    }

    pub fn copy_selection(&self, window: &Window, cx: &mut App) -> Result<(), InputError> {
        if !self.focused(window, cx) {
            return Err(InputError::WrongFocus);
        }
        self.copy_requested(cx)
    }

    /// The shell's Copy command, addressed to this terminal by placement: its
    /// selection is copied wherever keyboard focus is, as with the find bar
    /// open.
    pub fn copy_requested(&self, cx: &mut App) -> Result<(), InputError> {
        let text = self.with_current(false, |control| {
            control
                .engine()
                .ok_or(InputError::NotReady)?
                .selection_text_view_bounded(self.view, 1024 * 1024)
                .map_err(engine_error)
        })?;
        let phux_client_runtime::engine::BoundedSelectionText::Text(text) = text else {
            return Err(InputError::Engine(format!(
                "clipboard copy refused: {text:?}"
            )));
        };
        let text =
            String::from_utf8(text).map_err(|error| InputError::Engine(error.to_string()))?;
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        Ok(())
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

fn input_ready(control: &ControlPlane, terminal: &ResourceId) -> bool {
    if control
        .options()
        .attach_role
        .is_some_and(phux_protocol::wire::frame::RolePolicy::is_viewer)
    {
        return false;
    }
    control.input_ready(terminal)
}

fn engine_error(error: phux_client_runtime::engine::EngineError) -> InputError {
    InputError::Engine(error.to_string())
}

impl EntityInputHandler for TerminalInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        self.current(window, cx).ok()?;
        let (bytes, adjusted) = self.composition.range(range)?;
        *actual = Some(adjusted);
        Some(self.composition.text[bytes].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        self.ready(window, cx).ok()?;
        Some(UTF16Selection {
            range: self.composition.selected.clone(),
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.current(window, cx).ok()?;
        (!self.composition.text.is_empty()).then(|| 0..self.composition.len())
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // insertText commits; unmark alone discards local preedit, never sends it.
        self.composition.clear();
        self.pending_key = None;
        window.invalidate_character_coordinates();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = self.commit(range, text, window, cx);
        self.record(result);
        window.invalidate_character_coordinates();
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_key = None;
        let result = self.ready(window, cx).and_then(|_| {
            self.composition
                .mark(range, text, selected)
                .then_some(())
                .ok_or(InputError::InvalidRange)
        });
        self.record(result);
        window.invalidate_character_coordinates();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.current(window, cx).ok()?;
        self.composition.range(range)?;
        // The painter owns cursor geometry. A candidate window is anchored at
        // that cursor; no guessed width for emoji or shaped preedit is exposed.
        Some(self.metrics?.cursor_bounds)
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        self.current(window, cx).ok()?;
        self.metrics?.cursor_bounds.contains(&point).then_some(0)
    }

    fn set_selected_text_range(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.current(window, cx).is_err() {
            return;
        }
        if let Some((_, range)) = self.composition.range(range) {
            self.composition.selected = range;
            cx.notify();
        }
    }

    fn text_length_utf16(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<usize> {
        self.current(window, cx).ok()?;
        Some(self.composition.len())
    }

    fn accepts_text_input(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.ready(window, cx).is_ok()
    }

    fn paste(&mut self, item: ClipboardItem, window: &mut Window, cx: &mut Context<Self>) {
        match self.clipboard_attach(&item) {
            Some(crate::attach::PathAttach::Paste(text)) => {
                let result = self.paste_text(&text, window, cx).map(|_| ());
                self.record(result);
            }
            Some(crate::attach::PathAttach::Upload(batch)) => {
                cx.emit(InputEvent::Upload(batch));
            }
            Some(crate::attach::PathAttach::Refuse { title, body }) => {
                cx.emit(InputEvent::Notice(InputNotice { title, body }));
            }
            None => {}
        }
        cx.notify();
    }
}

impl TerminalInput {
    fn clipboard_attach(&self, item: &ClipboardItem) -> Option<crate::attach::PathAttach> {
        crate::attach::clipboard_for_host(item, &self.host())
    }

    /// Paste addressed by the shell, not by keyboard focus. A refusal is
    /// returned so the shell can show it without a nested event.
    pub fn paste_command(&mut self, cx: &mut Context<Self>) -> Option<InputNotice> {
        let item = cx.read_from_clipboard()?;
        match self.clipboard_attach(&item) {
            Some(crate::attach::PathAttach::Paste(text)) => {
                let result = self.paste_requested(&text);
                self.record(result.map(|_| ()));
                None
            }
            Some(crate::attach::PathAttach::Upload(batch)) => {
                cx.emit(InputEvent::Upload(batch));
                None
            }
            Some(crate::attach::PathAttach::Refuse { title, body }) => {
                Some(InputNotice { title, body })
            }
            None => None,
        }
    }
}

/// Whole cells that fit the painted bounds. `None` for a collapsed pane.
fn capacity(metrics: InputMetrics) -> Option<(u16, u16)> {
    let cols = (metrics.bounds.size.width / metrics.cell_width).floor();
    let rows = (metrics.bounds.size.height / metrics.line_height).floor();
    let fits = cols >= 2.0 && rows >= 1.0;
    if !fits {
        return None;
    }
    Some((clamp(cols), clamp(rows)))
}

fn clamp(cells: f32) -> u16 {
    cells.min(f32::from(u16::MAX)) as u16
}

fn cell_px(metrics: InputMetrics) -> (u16, u16) {
    crate::terminal::geometry::cell_px(metrics.cell_width, metrics.line_height)
}

/// `None` when this proposal was already sent, or on the first look when the
/// grid matches and the pitch is [`DEFAULT_CELL_PX`]. Any other pitch is
/// reported so mouse pixels and the encoder share one cell. Sending a new
/// pitch tombstones native history once; that is the server's resize contract.
fn pending_fit(
    fitted: Option<FitProposal>,
    capacity: (u16, u16),
    cell_px: (u16, u16),
    dimensions: (u16, u16),
) -> Option<FitProposal> {
    let proposal = (capacity, cell_px);
    if fitted == Some(proposal) {
        return None;
    }
    if fitted.is_none() && capacity == dimensions && cell_px == DEFAULT_CELL_PX {
        return None;
    }
    Some(proposal)
}

/// The text a key should draw locally, if it is a guess the predictor can own.
/// Control and super chords are commands. Alt stays: Option types a character.
fn echo_guess(event: &KeyEvent) -> Option<String> {
    if !echo_action(event.action) || command_chord(event) {
        return None;
    }
    match event.key {
        PhysicalKey::Enter => Some("\r".to_owned()),
        PhysicalKey::Backspace => Some("\u{7f}".to_owned()),
        _ => nonempty_text(event.text.as_deref()),
    }
}

fn echo_action(action: KeyAction) -> bool {
    matches!(action, KeyAction::Press | KeyAction::Repeat)
}

fn command_chord(event: &KeyEvent) -> bool {
    event.mods.intersects(ModSet::CTRL | ModSet::SUPER)
}

fn nonempty_text(text: Option<&str>) -> Option<String> {
    text.filter(|text| !text.is_empty()).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::{
        Admit, Identity, InputError, InputMetrics, admit_delayed_fit, admit_presentation, capacity,
        cell_px, echo_guess, pending_fit, upgrade_client,
    };
    use gpui::{Bounds, point, px, size};
    use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

    fn metrics(width: f32, height: f32) -> InputMetrics {
        let bounds = Bounds::new(point(px(10.), px(20.)), size(px(width), px(height)));
        InputMetrics {
            bounds,
            cell_width: px(8.),
            line_height: px(17.),
            scale: 1.,
            cursor_bounds: bounds,
        }
    }

    #[test]
    fn fit_skips_a_matching_grid_at_the_default_cell() {
        assert_eq!(pending_fit(None, (80, 24), (8, 16), (80, 24)), None);
    }

    #[test]
    fn fit_reports_a_nondefault_cell_and_a_changed_pitch() {
        assert_eq!(
            pending_fit(None, (80, 24), (9, 20), (80, 24)),
            Some(((80, 24), (9, 20)))
        );
        assert_eq!(
            pending_fit(Some(((80, 24), (8, 16))), (80, 24), (9, 20), (80, 24)),
            Some(((80, 24), (9, 20)))
        );
        assert_eq!(
            pending_fit(Some(((80, 24), (9, 20))), (80, 24), (9, 20), (10, 4)),
            None
        );
    }

    #[test]
    fn fit_reports_a_different_capacity_at_the_default_cell() {
        assert_eq!(
            pending_fit(None, (100, 40), (8, 16), (80, 24)),
            Some(((100, 40), (8, 16)))
        );
    }

    #[test]
    fn cell_pitch_rounds_the_painted_cell() {
        let mut sample = metrics(800., 400.);
        sample.cell_width = px(9.5);
        sample.line_height = px(17.3);
        assert_eq!(cell_px(sample), (10, 17));
    }

    #[test]
    fn capacity_counts_whole_cells_only() {
        assert_eq!(capacity(metrics(807., 409.)), Some((100, 24)));
        assert_eq!(capacity(metrics(16., 17.)), Some((2, 1)));
    }

    #[test]
    fn collapsed_or_degenerate_panes_propose_nothing() {
        assert_eq!(capacity(metrics(15., 400.)), None);
        assert_eq!(capacity(metrics(400., 16.)), None);
        assert_eq!(capacity(metrics(f32::NAN, 400.)), None);
    }

    fn grid(terminal: phux_protocol::ResourceId, stream: u64, bootstrap: u64) -> super::GridFrame {
        use phux_client_runtime::publication::{
            Cursor, FrameColors, GridBuffer, GridDamage, Scrollbar,
        };
        super::GridFrame {
            terminal_id: terminal,
            generation: 1,
            stream_id: stream,
            bootstrap_id: bootstrap,
            last_seq: 0,
            cols: 1,
            rows: 1,
            cursor: Cursor::default(),
            scrollbar: Scrollbar::default(),
            colors: FrameColors::default(),
            damage: GridDamage::Full,
            buffer: GridBuffer::default(),
        }
    }

    #[test]
    fn presentation_identity_rejects_the_wrong_terminal_and_a_replaced_replica() {
        let bound = phux_protocol::ResourceId::local(7);
        let frame = grid(bound.clone(), 1, 1);
        let Admit::Accept(first) = admit_presentation(&bound, None, 3, &frame) else {
            panic!("a first presentation must arm");
        };
        assert_eq!(
            admit_presentation(&bound, Some(first), 3, &frame),
            Admit::Accept(first)
        );
        assert_eq!(
            admit_presentation(&phux_protocol::ResourceId::local(8), Some(first), 3, &frame),
            Admit::WrongTerminal
        );
        assert_eq!(
            admit_presentation(&bound, Some(first), 4, &frame),
            Admit::Replaced
        );
        let swapped = grid(bound.clone(), 1, 2);
        assert_eq!(
            admit_presentation(&bound, Some(first), 3, &swapped),
            Admit::Replaced
        );
    }

    #[test]
    fn a_dropped_client_is_a_stale_handle() {
        let client = std::sync::Arc::new(phux_client_runtime::Runtime::embedded(
            phux_client_runtime::control::ControlOptions::default(),
        ));
        let weak = std::sync::Arc::downgrade(&client);
        assert!(upgrade_client(&weak).is_ok());
        drop(client);
        assert_eq!(upgrade_client(&weak).unwrap_err(), InputError::StaleHandle);
    }

    fn press(key: PhysicalKey, mods: ModSet, text: Option<&str>) -> KeyEvent {
        KeyEvent {
            action: KeyAction::Press,
            key,
            mods,
            consumed_mods: ModSet::empty(),
            composing: false,
            text: text.map(str::to_owned),
            unshifted_codepoint: None,
        }
    }

    #[test]
    fn local_echo_guesses_typed_text_and_skips_commands() {
        assert_eq!(
            echo_guess(&press(
                PhysicalKey::Unidentified,
                ModSet::empty(),
                Some("é")
            )),
            Some("é".to_owned())
        );
        assert_eq!(
            echo_guess(&press(PhysicalKey::Enter, ModSet::empty(), None)),
            Some("\r".to_owned())
        );
        assert_eq!(
            echo_guess(&press(PhysicalKey::Backspace, ModSet::SHIFT, None)),
            Some("\u{7f}".to_owned())
        );
        assert_eq!(
            echo_guess(&press(PhysicalKey::C, ModSet::CTRL, Some("c"))),
            None
        );
        assert_eq!(
            echo_guess(&press(PhysicalKey::ArrowLeft, ModSet::empty(), None)),
            None
        );
        let mut release = press(PhysicalKey::Unidentified, ModSet::empty(), Some("a"));
        release.action = KeyAction::Release;
        assert_eq!(echo_guess(&release), None);
    }

    fn generation(epoch: u64) -> Identity {
        Identity {
            epoch,
            stream: 1,
            bootstrap: 1,
        }
    }

    /// Pressing another pane, switching tabs, or zooming a sibling moves the
    /// visible-owner claim. The hidden view's token no longer matches.
    #[test]
    fn press_tab_switch_and_zoom_retire_a_hidden_owners_fit() {
        let armed = generation(1);
        assert!(
            !admit_delayed_fit(Some(2), 1, armed, Some(armed)),
            "the focused writable view holds the claim"
        );
        assert!(
            !admit_delayed_fit(None, 1, armed, Some(armed)),
            "a painted non-owner released the claim"
        );
    }

    /// Blur and occlusion leave the same view as the visible owner.
    #[test]
    fn blur_keeps_the_visible_owners_claim() {
        let armed = generation(1);
        assert!(admit_delayed_fit(Some(7), 7, armed, Some(armed)));
    }

    /// Coming back to that owner, still on the armed generation, may settle.
    #[test]
    fn returning_the_visible_owner_is_admitted() {
        let armed = generation(4);
        assert!(admit_delayed_fit(Some(7), 7, armed, Some(armed)));
    }

    #[test]
    fn reconnect_replaces_the_armed_generation() {
        let armed = generation(1);
        assert!(!admit_delayed_fit(Some(7), 7, armed, Some(generation(2))));
        assert!(!admit_delayed_fit(Some(7), 7, armed, None));
    }
}

/// Drives [`TerminalInput::fit`] and [`TerminalInput::flush_fit`] against an
/// embedded runtime. Press, tab switch, and zoom are one handoff: the newly
/// focused writable view claims ownership while the previous timer is still
/// running, and hiding does not call `fit(false)`.
#[cfg(test)]
mod delivery {
    use super::{FIT_SETTLE, InputMetrics, TerminalInput};
    use gpui::{AppContext, Bounds, Entity, TestAppContext, Window, point, px, size};
    use phux_client_runtime::control::{ControlOptions, ControlPlane};
    use phux_client_runtime::{Client, Runtime, ViewId};
    use phux_protocol::{
        PROTOCOL_VERSION, ResourceId,
        caps::{
            BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities,
            ServerFeature, ServerFeatureSet,
        },
        ids::{BootstrapId, ClientId, SessionId, StreamId, WindowId},
        wire::{
            frame::{AttachTarget, Command, CommandResult, ErrorCode, FrameKind, RolePolicy},
            info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo},
        },
    };
    use std::sync::Arc;
    use std::time::Instant;

    fn terminal() -> ResourceId {
        ResourceId::local(7)
    }

    fn metrics(width: f32, height: f32) -> InputMetrics {
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(width), px(height)));
        InputMetrics {
            bounds,
            cell_width: px(8.),
            line_height: px(17.),
            scale: 1.,
            cursor_bounds: bounds,
        }
    }

    fn session(role: Option<RolePolicy>) -> Arc<Client> {
        let client = Arc::new(Runtime::embedded(ControlOptions {
            attach: Some(AttachTarget::ByName("fit".into())),
            attach_role: role,
            ..ControlOptions::default()
        }));
        open_session(&client, 1);
        client
    }

    fn open_session(client: &Client, generation: u64) {
        client.with_control(ControlPlane::connection_opened);
        let _ = client.take_outbound();
        client
            .feed(FrameKind::HelloOk {
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new()
                    .with_features(ServerFeatureSet::with(&[ServerFeature::AcknowledgedInput])),
                server_id: vec![1; 16],
                selected_profile: BootstrapProfile::SynthesizedVtRaw,
                bootstrap_limits: BootstrapLimits::default(),
            })
            .expect("hello");
        let attach_id = client
            .take_outbound()
            .iter()
            .find_map(|bytes| match FrameKind::decode(bytes).expect("decode").0 {
                FrameKind::Attach { attach_id, .. } => Some(attach_id),
                _ => None,
            })
            .expect("attach request");
        let stream_id = StreamId::new(generation).unwrap();
        let bootstrap_id = BootstrapId::new(generation).unwrap();
        let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal())
            .with_sessions(vec![SessionInfo::new(SessionId::new(1), "fit")])
            .with_windows(vec![WindowInfo::new(
                WindowId::new(1),
                SessionId::new(1),
                "shell",
            )])
            .with_resources(vec![ResourceInfo::new(terminal(), WindowId::new(1), 20, 4)]);
        for frame in [
            FrameKind::Attached {
                attach_id,
                snapshot,
                initial_client_id: ClientId::new(1),
            },
            FrameKind::BootstrapBegin {
                terminal_id: terminal(),
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                cols: 20,
                rows: 4,
                base_seq: 0,
            },
            FrameKind::BootstrapChunk {
                terminal_id: terminal(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: b"ready".to_vec().into(),
            },
            FrameKind::BootstrapReady {
                terminal_id: terminal(),
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
            FrameKind::AttachReady { attach_id },
        ] {
            client.feed(frame).expect("bootstrap");
        }
        let _ = client.take_outbound();
        let _ = client.take_events();
    }

    fn fence(client: &Client) -> Option<phux_client_runtime::control::ProjectionFence> {
        client.with_control(|control| {
            control.apply_line(&terminal(), "command");
            let request_id = control
                .take_outbound()
                .iter()
                .find_map(|bytes| match FrameKind::decode(bytes).expect("decode").0 {
                    FrameKind::Command {
                        request_id,
                        command: Command::ApplyInput { .. },
                    } => Some(request_id),
                    _ => None,
                })
                .expect("apply input");
            control
                .feed(FrameKind::CommandResult {
                    request_id,
                    result: CommandResult::Error {
                        code: ErrorCode::InputDeliveryUnknown,
                        message: "ambiguous fixture delivery".into(),
                    },
                })
                .expect("unknown");
            let _ = control.take_outbound();
            control.projection_fence(&terminal())
        })
    }

    fn resizes(client: &Client) -> Vec<(u16, u16)> {
        client
            .take_outbound()
            .iter()
            .filter_map(|bytes| match FrameKind::decode(bytes).expect("decode").0 {
                FrameKind::ResizeTerminal { cols, rows, .. } => Some((cols, rows)),
                _ => None,
            })
            .collect()
    }

    fn bind(
        client: &Arc<Client>,
        view: ViewId,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> Entity<TerminalInput> {
        let focus = cx.focus_handle();
        let weak = Arc::downgrade(client);
        let terminal = terminal();
        cx.new(move |_cx| TerminalInput::new(weak, view, terminal, focus, window))
    }

    fn arm(
        input: &Entity<TerminalInput>,
        owner: bool,
        width: f32,
        height: f32,
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        input.update(cx, |state, cx| {
            let client = state.client().expect("client");
            let frame = client.acquire_view(state.view).expect("frame");
            let epoch = client.connection_epoch();
            state
                .presented_at(epoch, &frame, metrics(width, height))
                .expect("present");
            state.fit(owner, window, cx);
        });
    }

    fn settle(input: &Entity<TerminalInput>, cx: &mut gpui::App) {
        input.update(cx, |state, _| {
            if let Some(pending) = &mut state.settling {
                pending.since = Instant::now() - FIT_SETTLE;
            }
            state.flush_fit();
        });
    }

    #[gpui::test]
    fn a_hidden_owners_delayed_fit_does_not_override_the_focused_view(cx: &mut TestAppContext) {
        let client = session(None);
        let older = client.create_view(&terminal()).expect("hidden view");
        let focused = client.create_view(&terminal()).expect("focused view");
        cx.add_empty_window().update(|window, cx| {
            // Press, tab switch, and zoom all end here: the newly focused
            // writable view fits, and the previous owner is no longer painted.
            let hidden = bind(&client, older, window, cx);
            let shown = bind(&client, focused, window, cx);
            arm(&hidden, true, 800., 408., window, cx);
            arm(&shown, true, 400., 204., window, cx);
            settle(&hidden, cx);
            assert!(
                resizes(&client).is_empty(),
                "a delayed fit from the hidden owner must not resize"
            );
            settle(&shown, cx);
            assert_eq!(resizes(&client), vec![(50, 12)]);

            // Returning to the first view settles that view's geometry.
            arm(&hidden, true, 800., 408., window, cx);
            settle(&hidden, cx);
            assert_eq!(resizes(&client), vec![(100, 24)]);

            // A later paint that is no longer the owner drops its timer.
            arm(&hidden, true, 560., 272., window, cx);
            arm(&hidden, false, 560., 272., window, cx);
            settle(&hidden, cx);
            assert!(resizes(&client).is_empty());
        });
    }

    #[gpui::test]
    fn blur_of_an_inactive_window_still_settles_the_visible_owner(cx: &mut TestAppContext) {
        let client = session(None);
        let view = client.create_view(&terminal()).expect("view");
        cx.add_empty_window().update(|window, cx| {
            let input = bind(&client, view, window, cx);
            arm(&input, true, 800., 408., window, cx);
            window.blur(cx);
            assert!(
                !window.is_window_active(),
                "occlusion and blur keep the visible owner"
            );
            input.update(cx, |state, _| {
                state.flush_fit();
                assert!(state.settling.is_some(), "the clock has not settled");
            });
            assert!(resizes(&client).is_empty());
            settle(&input, cx);
            assert_eq!(resizes(&client), vec![(100, 24)]);
        });
    }

    #[gpui::test]
    fn a_resize_burst_sends_only_the_settled_proposal(cx: &mut TestAppContext) {
        let client = session(None);
        let view = client.create_view(&terminal()).expect("view");
        cx.add_empty_window().update(|window, cx| {
            let input = bind(&client, view, window, cx);
            arm(&input, true, 800., 408., window, cx);
            arm(&input, true, 640., 340., window, cx);
            input.update(cx, |state, _| state.flush_fit());
            assert!(resizes(&client).is_empty());
            settle(&input, cx);
            assert_eq!(resizes(&client), vec![(80, 20)]);
        });
    }

    #[gpui::test]
    fn reconnect_drops_the_hidden_fit_until_the_new_generation_is_shown(cx: &mut TestAppContext) {
        let client = session(None);
        let view = client.create_view(&terminal()).expect("view");
        cx.add_empty_window().update(|window, cx| {
            let stale = bind(&client, view, window, cx);
            arm(&stale, true, 800., 408., window, cx);
            open_session(&client, 2);
            settle(&stale, cx);
            assert!(
                resizes(&client).is_empty(),
                "the pre-reconnect proposal is a replaced generation"
            );
            let view = client
                .create_view(&terminal())
                .expect("view after reconnect");
            let fresh = bind(&client, view, window, cx);
            arm(&fresh, true, 800., 408., window, cx);
            settle(&fresh, cx);
            assert_eq!(resizes(&client), vec![(100, 24)]);
        });
    }

    #[gpui::test]
    fn a_viewer_is_not_resized(cx: &mut TestAppContext) {
        let client = session(Some(RolePolicy::VIEWER));
        let view = client.create_view(&terminal()).expect("view");
        cx.add_empty_window().update(|window, cx| {
            let input = bind(&client, view, window, cx);
            arm(&input, true, 800., 408., window, cx);
            settle(&input, cx);
            assert!(resizes(&client).is_empty());
        });
    }

    #[gpui::test]
    fn a_settled_fit_does_not_acknowledge_an_unknown_delivery_fence(cx: &mut TestAppContext) {
        let client = session(None);
        let view = client.create_view(&terminal()).expect("view");
        let armed = fence(&client).expect("fence");
        cx.add_empty_window().update(|window, cx| {
            let input = bind(&client, view, window, cx);
            arm(&input, true, 800., 408., window, cx);
            settle(&input, cx);
            assert_eq!(resizes(&client), vec![(100, 24)]);
            let current = client.with_control(|control| control.projection_fence(&terminal()));
            assert_eq!(current, Some(armed));
        });
    }
}
