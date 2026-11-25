//! OSC 52 clipboard reads: `defaults.clipboard-read` denies, allows, or asks
//! ([`ClipboardReadOverlay`]). Only the focused pane of a focused window may
//! read, one at a time, prompting at most once per [`PROMPT_COOLDOWN`]; the
//! host's answer goes back as `CLIPBOARD_REPLY` only if that pane still has
//! focus.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use phux_client_core::engine::ClipboardSelection;
use phux_config::ClipboardRead;
use phux_protocol::ResourceId;
use phux_protocol::input::key::{KeyAction, KeyEvent, PhysicalKey};
use phux_protocol::input::mouse::MouseEvent;
use phux_protocol::wire::frame::MAX_CLIPBOARD_TEXT;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratcn::runtime::{DeclareCtx, Event, EventResult, FocusState, Ratcn};
use ratcn::{Button, Dialog};

use crate::render::kit::{self, Surface};
use crate::render::overlay::widgets::centered_panel;
use crate::render::overlay::{OverlayCommand, RenderOverlay};
use crate::render::{ChromeBreakpoints, Theme};

/// The shortest time between two prompts from one pane.
pub(in crate::attach) const PROMPT_COOLDOWN: Duration = Duration::from_secs(3);

/// How long the host terminal has to answer before the read is dropped.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// How long an unanswered prompt holds off other reads, in case it was
/// closed without an answer.
const ASK_TIMEOUT: Duration = Duration::from_secs(60);

/// The read state the driver and the prompt share.
pub(in crate::attach) type SharedClipboardReads = Rc<RefCell<ClipboardReads>>;

/// What the driver does with one read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::attach) enum ReadAction {
    /// Drop it without asking anyone.
    Drop,
    /// Raise the prompt.
    Ask,
    /// Allowed: [`ClipboardReads::take_query`] has the host query.
    Query,
}

/// The person's answer to the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::attach) enum Choice {
    AllowOnce,
    AllowPane,
    Deny,
}

/// Where the one read in flight stands.
#[derive(Debug, Default)]
pub(in crate::attach) enum ReadPhase {
    /// No read in flight.
    #[default]
    Idle,
    /// A prompt is up, since when.
    Asking(Instant),
    /// Allowed, not yet asked of the host terminal.
    Approved(ResourceId, ClipboardSelection),
    /// Asked of the host terminal, and when.
    Awaiting(ResourceId, ClipboardSelection, Instant),
}

impl ReadPhase {
    /// Whether this phase holds off another read at `now`. A prompt or a
    /// host query that outlived its timeout no longer does.
    fn in_flight(&self, now: Instant) -> bool {
        match self {
            Self::Idle => false,
            Self::Asking(at) => now.duration_since(*at) < ASK_TIMEOUT,
            Self::Approved(..) => true,
            Self::Awaiting(_, _, at) => now.duration_since(*at) < REPLY_TIMEOUT,
        }
    }

    /// Leave `Approved` for `Idle`, handing back the read; any other phase
    /// stays as it is.
    fn take_approved(&mut self) -> Option<(ResourceId, ClipboardSelection)> {
        match std::mem::take(self) {
            Self::Approved(pane, selection) => Some((pane, selection)),
            other => {
                *self = other;
                None
            }
        }
    }

    /// Leave `Awaiting` for `Idle`, handing back the read; any other phase
    /// stays as it is.
    fn take_awaiting(&mut self) -> Option<(ResourceId, ClipboardSelection, Instant)> {
        match std::mem::take(self) {
            Self::Awaiting(pane, selection, at) => Some((pane, selection, at)),
            other => {
                *self = other;
                None
            }
        }
    }
}

#[derive(Debug, Default)]
pub(in crate::attach) struct ClipboardReads {
    /// Whether the server takes `CLIPBOARD_REPLY`; without it nothing is
    /// ever asked.
    supported: bool,
    /// The host terminal's window has focus, as its focus reports say
    /// (assumed when it sends none).
    window_focused: bool,
    /// Panes allowed for the rest of this attach.
    granted: HashSet<ResourceId>,
    /// When each pane last prompted.
    prompted: HashMap<ResourceId, Instant>,
    /// The read in flight, if any.
    phase: ReadPhase,
}

impl ClipboardReads {
    pub(in crate::attach) fn shared(supported: bool) -> SharedClipboardReads {
        Rc::new(RefCell::new(Self {
            supported,
            window_focused: true,
            ..Self::default()
        }))
    }

    /// Fold a host terminal focus report.
    pub(in crate::attach) const fn set_window_focused(&mut self, focused: bool) {
        self.window_focused = focused;
    }

    /// Decide `pane`'s read. `focused` is true when it is this client's
    /// focused pane; the window must have focus too.
    pub(in crate::attach) fn on_read(
        &mut self,
        pane: &ResourceId,
        selection: ClipboardSelection,
        focused: bool,
        policy: ClipboardRead,
        now: Instant,
    ) -> ReadAction {
        let in_flight = self.phase.in_flight(now);
        let focused = focused && self.window_focused;
        if !self.supported || !focused || policy == ClipboardRead::Deny || in_flight {
            return ReadAction::Drop;
        }
        if policy == ClipboardRead::Allow || self.granted.contains(pane) {
            self.phase = ReadPhase::Approved(pane.clone(), selection);
            return ReadAction::Query;
        }
        if self
            .prompted
            .get(pane)
            .is_some_and(|at| now.duration_since(*at) < PROMPT_COOLDOWN)
        {
            return ReadAction::Drop;
        }
        self.prompted.insert(pane.clone(), now);
        self.phase = ReadPhase::Asking(now);
        ReadAction::Ask
    }

    /// Record the person's answer for `pane`. A pane grant always sticks,
    /// but only an open prompt's answer starts a read: a prompt whose phase
    /// a later read already replaced must not overwrite it.
    pub(in crate::attach) fn decide(
        &mut self,
        pane: &ResourceId,
        selection: ClipboardSelection,
        choice: Choice,
    ) {
        if choice == Choice::AllowPane {
            self.granted.insert(pane.clone());
        }
        if !matches!(self.phase, ReadPhase::Asking(_)) {
            return;
        }
        self.phase = match choice {
            Choice::Deny => ReadPhase::Idle,
            Choice::AllowOnce | Choice::AllowPane => ReadPhase::Approved(pane.clone(), selection),
        };
    }

    /// The OSC 52 query to write to the host terminal for an allowed read.
    pub(in crate::attach) fn take_query(&mut self, now: Instant) -> Option<Vec<u8>> {
        let (pane, selection) = self.phase.take_approved()?;
        let mut query = b"\x1b]52;".to_vec();
        query.push(selection.byte());
        query.extend_from_slice(b";?\x1b\\");
        self.phase = ReadPhase::Awaiting(pane, selection, now);
        Some(query)
    }

    /// The host terminal answered with `base64`: the reply for the pane that
    /// asked, if it still has focus, the answer came in time, and it is
    /// UTF-8 text within the cap.
    pub(in crate::attach) fn take_reply(
        &mut self,
        base64: &[u8],
        focused: Option<&ResourceId>,
        now: Instant,
    ) -> Option<(ResourceId, ClipboardSelection, String)> {
        let (pane, selection, at) = self.phase.take_awaiting()?;
        if focused != Some(&pane) || now.duration_since(at) >= REPLY_TIMEOUT {
            return None;
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(base64)
            .ok()?;
        let text = String::from_utf8(bytes).ok()?;
        (text.len() <= MAX_CLIPBOARD_TEXT).then_some((pane, selection, text))
    }
}

/// How long a new prompt ignores the keys that allow, so a word being typed
/// or an Enter already on its way can't answer a read the person never saw.
pub(in crate::attach) const PROMPT_ARMING: Duration = Duration::from_millis(500);

/// The `ask` prompt, a ratcn [`Dialog`] with three buttons. Enter presses
/// the focused one (focus starts on `Allow once`), `y` allows once, `a`
/// allows this pane for the rest of the attach, Esc or `n` denies. Allowing,
/// by key or by click, waits out [`PROMPT_ARMING`].
pub(in crate::attach) struct ClipboardReadOverlay {
    pane: ResourceId,
    shown: Instant,
    selection: ClipboardSelection,
    reads: SharedClipboardReads,
    message: String,
    theme: Theme,
    breakpoints: ChromeBreakpoints,
    model: Model,
    ui: Surface<Model, Msg>,
    /// The viewport of the last paint, which input is laid out against.
    outer: Cell<Rect>,
}

/// What the prompt's ratcn surface tracks between frames: which button has
/// focus.
#[derive(Debug)]
struct Model {
    focus: FocusState,
}

/// What the dialog's buttons and focus traversal ask for.
#[derive(Debug, Clone)]
enum Msg {
    Focus(FocusState),
    Answer(Choice),
}

const DIALOG: &str = "clipboard-read";
const DENY: &str = "deny";
const PANE: &str = "pane";
const ONCE: &str = "once";

/// Rows the dialog adds around its message: border and padding above and
/// below, the gap row, and the button row.
const DIALOG_CHROME_ROWS: u16 = 2 + 2 + 1 + 1;

/// The narrowest dialog that fits all three buttons on one row: `Deny`,
/// `Allow pane`, `Allow once` (label plus four cells each, two apart),
/// inside the border and padding.
const MIN_WIDTH: u16 = 8 + 14 + 14 + 2 * 2 + 4;

impl ClipboardReadOverlay {
    pub(in crate::attach) fn new(
        pane: ResourceId,
        selection: ClipboardSelection,
        label: &str,
        reads: SharedClipboardReads,
        theme: &Theme,
    ) -> Self {
        let what = match selection {
            ClipboardSelection::Clipboard => "clipboard",
            ClipboardSelection::Primary => "primary selection",
        };
        let runtime = Ratcn::new().focus(|m: &Model| &m.focus, Msg::Focus);
        Self {
            pane,
            shown: Instant::now(),
            selection,
            reads,
            message: format!("{label} wants to read your {what}"),
            theme: *theme,
            breakpoints: ChromeBreakpoints::default(),
            model: Model {
                focus: FocusState::intent([DIALOG, ONCE]),
            },
            ui: Surface::new(runtime),
            outer: Cell::new(Rect::new(0, 0, 80, 24)),
        }
    }

    /// A half-width dialog (at least [`MIN_WIDTH`]) exactly tall enough for
    /// the message wrapped to its inner width, centered.
    fn modal_area(&self, outer: Rect) -> Rect {
        let min_h = DIALOG_CHROME_ROWS + 1;
        let wide = centered_panel(outer, 5, MIN_WIDTH, min_h, self.breakpoints);
        let inner = wide.width.saturating_sub(4);
        let rows = ratcn::geometry::wrapped_height(&self.message, inner)
            .saturating_add(DIALOG_CHROME_ROWS);
        let h = rows.min(outer.height);
        let y = outer.y + (outer.height.saturating_sub(h)) / 2;
        Rect::new(wide.x, y, wide.width, h)
    }

    /// Declare the dialog filling `area` (the modal rect).
    fn declare(&self, area: Rect) -> impl FnMut(&mut DeclareCtx<'_, Model, Msg>) + use<> {
        let message = self.message.clone();
        move |ctx| {
            let answer = |choice| move || Msg::Answer(choice);
            let deny = Button::new("Deny")
                .secondary()
                .on_press(answer(Choice::Deny));
            let pane = Button::new("Allow pane")
                .secondary()
                .on_press(answer(Choice::AllowPane));
            let once = Button::new("Allow once").on_press(answer(Choice::AllowOnce));
            let dialog = Dialog::new()
                .title("Clipboard read?")
                .description(message.clone())
                .outer_width(area.width)
                .outer_height(area.height)
                .on_dismiss(answer(Choice::Deny))
                .action(DENY, deny)
                .action(PANE, pane)
                .action(ONCE, once);
            ctx.component(DIALOG, dialog, area);
        }
    }

    /// Route one ratcn event and fold its message into a command.
    fn route(&mut self, event: Event) -> OverlayCommand {
        let area = self.modal_area(self.outer.get());
        let declare = self.declare(area);
        let theme = kit::theme(&self.theme);
        match self.ui.route(event, &self.model, &theme, declare) {
            EventResult::Emit(Msg::Focus(focus)) => {
                self.model.focus = focus;
                OverlayCommand::Stay
            }
            EventResult::Emit(Msg::Answer(choice)) => self.answer(choice),
            EventResult::Consumed | EventResult::Ignored => OverlayCommand::Stay,
        }
    }

    /// Record `choice` and close, unless it allows before the prompt armed.
    fn answer(&self, choice: Choice) -> OverlayCommand {
        let armed = self.shown.elapsed() >= PROMPT_ARMING;
        if choice != Choice::Deny && !armed {
            return OverlayCommand::Stay;
        }
        self.reads
            .borrow_mut()
            .decide(&self.pane, self.selection, choice);
        OverlayCommand::Dismiss
    }
}

impl RenderOverlay for ClipboardReadOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.outer.set(area);
        let modal = self.modal_area(area);
        let theme = kit::theme(&self.theme);
        self.ui
            .paint(buf, modal, &self.model, &theme, self.declare(modal));
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        Some(self.modal_area(area))
    }

    fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.breakpoints = bp;
        self.ui.invalidate();
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = *theme;
    }

    fn on_viewport_resize(&mut self, _pane_cols: u16, _pane_rows: u16) {
        self.ui.invalidate();
    }

    fn asks(&self) -> bool {
        true
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        if key.action != KeyAction::Press {
            return OverlayCommand::Stay;
        }
        // The letter shortcuts answer whichever button has focus.
        match key.key {
            PhysicalKey::N => return self.answer(Choice::Deny),
            PhysicalKey::Y => return self.answer(Choice::AllowOnce),
            PhysicalKey::A => return self.answer(Choice::AllowPane),
            _ => {}
        }
        kit::key(key).map_or(OverlayCommand::Stay, |event| self.route(event))
    }

    fn handle_mouse(&mut self, mouse: &MouseEvent) -> OverlayCommand {
        kit::mouse(mouse).map_or(OverlayCommand::Stay, |event| self.route(event))
    }
}

#[cfg(test)]
mod tests;
