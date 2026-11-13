//! OSC 52 clipboard reads (`docs/program-sequences.md#clipboard-osc-52`).
//!
//! `defaults.clipboard-read` decides: `deny` drops every read, `allow`
//! answers, `ask` raises [`ClipboardReadOverlay`] (allow once, allow for this
//! pane, deny). Only the focused pane of a focused window may read, one read
//! is in flight at a time, and a pane may prompt at most once per
//! [`PROMPT_COOLDOWN`]. An allowed read asks the host terminal for its
//! clipboard with OSC 52, the way writes reach it; its answer goes back to
//! the asking pane as `CLIPBOARD_REPLY`, only if that pane still has focus.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use phux_client_core::engine::ClipboardSelection;
use phux_config::ClipboardRead;
use phux_protocol::ResourceId;
use phux_protocol::input::key::{KeyAction, KeyEvent, PhysicalKey};
use phux_protocol::wire::frame::MAX_CLIPBOARD_TEXT;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;

use crate::render::overlay::widgets::{Modal, centered_panel};
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
    /// A prompt is up, since when.
    asking: Option<Instant>,
    /// Allowed, not yet asked of the host terminal.
    approved: Option<(ResourceId, ClipboardSelection)>,
    /// Asked of the host terminal, and when.
    awaiting: Option<(ResourceId, ClipboardSelection, Instant)>,
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
        let in_flight = self
            .asking
            .is_some_and(|at| now.duration_since(at) < ASK_TIMEOUT)
            || self.approved.is_some()
            || self
                .awaiting
                .as_ref()
                .is_some_and(|(_, _, at)| now.duration_since(*at) < REPLY_TIMEOUT);
        let focused = focused && self.window_focused;
        if !self.supported || !focused || policy == ClipboardRead::Deny || in_flight {
            return ReadAction::Drop;
        }
        if policy == ClipboardRead::Allow || self.granted.contains(pane) {
            self.approved = Some((pane.clone(), selection));
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
        self.asking = Some(now);
        ReadAction::Ask
    }

    /// Record the person's answer for `pane`.
    pub(in crate::attach) fn decide(
        &mut self,
        pane: &ResourceId,
        selection: ClipboardSelection,
        choice: Choice,
    ) {
        self.asking = None;
        match choice {
            Choice::Deny => {}
            Choice::AllowPane => {
                self.granted.insert(pane.clone());
                self.approved = Some((pane.clone(), selection));
            }
            Choice::AllowOnce => self.approved = Some((pane.clone(), selection)),
        }
    }

    /// The OSC 52 query to write to the host terminal for an allowed read.
    pub(in crate::attach) fn take_query(&mut self, now: Instant) -> Option<Vec<u8>> {
        let (pane, selection) = self.approved.take()?;
        let mut query = b"\x1b]52;".to_vec();
        query.push(selection.byte());
        query.extend_from_slice(b";?\x1b\\");
        self.awaiting = Some((pane, selection, now));
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
        let (pane, selection, at) = self.awaiting.take()?;
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

/// The `ask` prompt. Enter or `y` allows once, `a` allows this pane for the
/// rest of the attach, Esc or `n` denies. Allowing waits out [`PROMPT_ARMING`].
pub(in crate::attach) struct ClipboardReadOverlay {
    pane: ResourceId,
    shown: Instant,
    selection: ClipboardSelection,
    reads: SharedClipboardReads,
    message: String,
    theme: Theme,
    breakpoints: ChromeBreakpoints,
}

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
        Self {
            pane,
            shown: Instant::now(),
            selection,
            reads,
            message: format!("{label} wants to read your {what}"),
            theme: *theme,
            breakpoints: ChromeBreakpoints::default(),
        }
    }

    fn modal_area(outer: Rect, bp: ChromeBreakpoints) -> Rect {
        let wide = centered_panel(outer, 5, 20, 3, bp);
        let h = 3.min(outer.height);
        let y = outer.y + (outer.height.saturating_sub(h)) / 2;
        Rect::new(wide.x, y, wide.width, h)
    }

    fn decide(&self, choice: Choice) -> OverlayCommand {
        self.reads
            .borrow_mut()
            .decide(&self.pane, self.selection, choice);
        OverlayCommand::Dismiss
    }
}

impl RenderOverlay for ClipboardReadOverlay {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let text = Style::default().fg(self.theme.text);
        let lines = vec![Line::styled(self.message.clone(), text)];
        Modal::new(&self.theme, "Clipboard read?".to_owned(), lines)
            .footer("", "⏎ once · a this pane · esc deny")
            .render_into(Self::modal_area(area, self.breakpoints), buf);
    }

    fn bounds(&self, area: Rect) -> Option<Rect> {
        Some(Self::modal_area(area, self.breakpoints))
    }

    fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.breakpoints = bp;
    }

    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        if key.action != KeyAction::Press {
            return OverlayCommand::Stay;
        }
        let armed = self.shown.elapsed() >= PROMPT_ARMING;
        match key.key {
            PhysicalKey::Escape | PhysicalKey::N => self.decide(Choice::Deny),
            PhysicalKey::Enter | PhysicalKey::Y if armed => self.decide(Choice::AllowOnce),
            PhysicalKey::A if armed => self.decide(Choice::AllowPane),
            _ => OverlayCommand::Stay,
        }
    }
}

#[cfg(test)]
mod tests;
