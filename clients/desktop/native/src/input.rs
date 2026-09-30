//! Native terminal input. The painter supplies identity and geometry; every
//! action resolves the existing FFI client registry again, without a listener.

mod composition;
mod keys;
mod link;
mod pointer;

use composition::Composition;
use gpuix_native::native_extensions::gpui::{
    self, App, Bounds, ClipboardItem, Context, EntityInputHandler, FocusHandle, KeyDownEvent,
    KeyUpEvent, Pixels, Point, UTF16Selection, Window, WindowId,
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
    time::{Duration, Instant},
};

/// How long a pane's capacity must hold still before it becomes a PTY resize.
/// Layout settling, window drags and split drags produce bursts of sizes;
/// every intermediate resize makes a shell redraw its prompt into history.
/// A settling proposal requests frames, so the final size always goes out.
const FIT_SETTLE: Duration = Duration::from_millis(60);

/// Geometry already computed by the painter, in logical window pixels.
#[derive(Clone, Copy, Debug)]
pub struct InputMetrics {
    pub bounds: Bounds<Pixels>,
    pub cell_width: Pixels,
    pub line_height: Pixels,
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

pub struct TerminalInput {
    handle: String,
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
    scroll_remainder: f32,
    option_as_alt: bool,
    last_error: Option<InputError>,
    /// The size this view last proposed.
    fitted: Option<(u16, u16)>,
    /// A capacity waiting to hold still for `FIT_SETTLE`.
    settling: Option<((u16, u16), Instant)>,
    fit_active: bool,
    #[cfg(feature = "input-fixture")]
    test_window_active: bool,
}

impl TerminalInput {
    pub fn new(
        handle: String,
        view: ViewId,
        terminal: ResourceId,
        focus: FocusHandle,
        window: &Window,
    ) -> Self {
        Self {
            handle,
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
            scroll_remainder: 0.0,
            option_as_alt: false,
            last_error: None,
            fitted: None,
            settling: None,
            fit_active: false,
            #[cfg(feature = "input-fixture")]
            test_window_active: false,
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }
    pub fn set_option_as_alt(&mut self, enabled: bool) {
        self.option_as_alt = enabled;
    }
    pub fn last_error(&self) -> Option<&InputError> {
        self.last_error.as_ref()
    }
    pub fn preedit(&self) -> (&str, Range<usize>) {
        (&self.composition.text, self.composition.selected.clone())
    }

    #[cfg(feature = "input-fixture")]
    pub fn simulate_window_activation(&mut self, active: bool) {
        self.test_window_active = active;
        if !active {
            self.cancel();
        }
    }

    fn window_active(&self, window: &Window) -> bool {
        #[cfg(feature = "input-fixture")]
        if self.test_window_active {
            return true;
        }
        window.is_window_active()
    }

    /// Call only for a frame actually being painted. This does NOT acknowledge
    /// an unknown-delivery fence. That authority belongs to the presenter.
    pub fn presented(
        &mut self,
        frame: &GridFrame,
        metrics: InputMetrics,
    ) -> Result<(), InputError> {
        let client = self.client()?;
        let identity = client.with_control(|control| {
            let current = self.live_frame(control)?;
            let identity = Identity::of(control.connection_epoch(), frame);
            if frame.terminal_id != self.terminal
                || identity != Identity::of(control.connection_epoch(), &current)
            {
                return Err(InputError::StalePresentation);
            }
            Ok(identity)
        })?;
        if self.identity.is_some_and(|previous| previous != identity) {
            self.cancel();
            self.metrics = None;
            self.fitted = None;
            return Err(InputError::StalePresentation);
        }
        self.identity = Some(identity);
        self.metrics = Some(metrics);
        self.dimensions = (frame.cols, frame.rows);
        Ok(())
    }

    pub fn cancel(&mut self) {
        self.composition.clear();
        self.pending_key = None;
        self.pressed.clear();
        self.gesture = 0;
        self.reported_button = None;
        self.scroll_remainder = 0.0;
    }

    /// Propose this pane's cell capacity as the terminal's PTY size while this
    /// view owns the terminal's geometry (desktop architecture, "Geometry").
    /// The shell names one owner per terminal: its only view, or the view the
    /// user focused last. Other views crop or leave space. Proposals are
    /// edge-triggered: an unchanged capacity sends nothing, so views cannot
    /// oscillate. Gaining ownership or re-activating the window forgets the
    /// last proposal, so the owner reclaims a size another client changed.
    pub fn fit(&mut self, owner: bool, window: &Window, cx: &mut Context<Self>) {
        let active = self.window_active(window);
        let reactivated = active && !self.fit_active;
        self.fit_active = active;
        let Some(metrics) = self.metrics else {
            return;
        };
        if !owner || reactivated {
            self.fitted = None;
        }
        if !owner {
            return;
        }
        let Some(capacity) = capacity(metrics) else {
            return;
        };
        // Already proposed, or (first look) already the authoritative size, as
        // for a terminal spawned at its predicted geometry.
        if self.fitted == Some(capacity) || (self.fitted.is_none() && capacity == self.dimensions) {
            self.fitted = Some(capacity);
            self.settling = None;
            return;
        }
        if self
            .settling
            .is_some_and(|(pending, _)| pending == capacity)
        {
            return;
        }
        // A timer, not the next frame, sends the settled size: a background or
        // occluded window may not paint again, and the resize must still go out.
        self.settling = Some((capacity, Instant::now()));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIT_SETTLE).await;
            let _ = this.update(cx, |state, _| state.flush_fit());
        })
        .detach();
    }

    /// Send the pending capacity once it has held still for `FIT_SETTLE`. A
    /// newer capacity restarts the clock and its own timer sends it.
    fn flush_fit(&mut self) {
        let Some((capacity, since)) = self.settling else {
            return;
        };
        if since.elapsed() < FIT_SETTLE {
            return;
        }
        let Ok(client) = self.client() else {
            return;
        };
        let (cols, rows) = capacity;
        let outcome = client.with_control(|control| {
            control.resize_terminal(&self.terminal, cols.into(), rows.into())
        });
        if outcome == TerminalResizeOutcome::Queued {
            self.fitted = Some(capacity);
        }
        self.settling = None;
    }

    fn client(&self) -> Result<Client, InputError> {
        phux_client_ffi::napi::initialize()
            .client(&self.handle)
            .map_err(|_| InputError::StaleHandle)
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
        cx: &App,
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
            return Ok(KeyDisposition::Platform);
        }
        self.with_current(true, |control| {
            queued(control.send_key(&self.terminal, event.clone()))
        })?;
        self.pressed.insert(event.key, event);
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
            self.pressed.insert(event.key, event);
            return Ok(());
        }
        self.with_current(true, |control| {
            queued(control.send_text(&self.terminal, &committed))
        })
    }

    /// The sole FFI event owner receives InputDelivery for this correlation.
    /// Neither the native adapter nor its caller may auto-retry Unknown.
    pub fn paste_text(&mut self, text: &str, window: &Window, cx: &App) -> Result<u64, InputError> {
        let focused = self.focused(window, cx);
        self.paste(text, focused)
    }

    /// The shell's Paste command, addressed to this terminal by placement.
    /// Keyboard focus may sit in an overlay over it, such as the find bar,
    /// whose own Command-V pastes into the field instead.
    pub fn paste_requested(&mut self, text: &str) -> Result<u64, InputError> {
        self.paste(text, true)
    }

    fn paste(&mut self, text: &str, focused: bool) -> Result<u64, InputError> {
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
                let delivery = control.apply_paste(&self.terminal, text);
                (delivery, !had_events && control.has_events())
            })
        });
        // Refusal only publishes a receipt; it cannot enqueue a paste after a
        // reconnect. Admission and successful enqueue above share one lock.
        let (delivery, resolved) = delivery.unwrap_or_else(|error| {
            (
                client.refuse_acknowledged_input(&format!("native paste refused: {error:?}")),
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
        if let Some(text) = item.text() {
            let result = self.paste_text(&text, window, cx).map(|_| ());
            self.record(result);
        }
        cx.notify();
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

#[cfg(test)]
mod tests {
    use super::{InputMetrics, capacity};
    use gpuix_native::native_extensions::gpui::{Bounds, point, px, size};

    fn metrics(width: f32, height: f32) -> InputMetrics {
        let bounds = Bounds::new(point(px(10.), px(20.)), size(px(width), px(height)));
        InputMetrics {
            bounds,
            cell_width: px(8.),
            line_height: px(17.),
            cursor_bounds: bounds,
        }
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
}
