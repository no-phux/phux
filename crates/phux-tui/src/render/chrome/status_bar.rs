//! Status-bar chrome layer.
//!
//! [`phux_config::widget::StatusBar`] composes the widget row;
//! [`StatusBarPainter`] lays it into a `cols × 1` ratatui buffer and emits
//! raw VT (CUP + per-cell SGR + grapheme), ending in an SGR reset.
//! The caller restores the focused pane's cursor afterwards (ADR-0020). The
//! bar defaults to [`Position::Top`] (`docs/consumers/tui.md` §8.5); the pane
//! content rect shifts to match.

use std::time::SystemTime;

use phux_config::widget::{Cell as WidgetCell, StatusBar, WidgetContext, WindowInfo};

use crate::render::theme::Theme;

/// Where the status bar lives in the outer terminal. Defaults to
/// [`Self::Top`] per `docs/consumers/tui.md` §8.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Position {
    /// One row at the very bottom of the outer terminal.
    Bottom,
    /// One row at the very top of the outer terminal. Surfaced by the
    /// `[status] position = "top"` config key.
    #[default]
    Top,
}

impl From<phux_config::StatusPosition> for Position {
    /// Map the `[status] position` config value onto the render enum.
    /// The mapping lives at this boundary so `phux-config`
    /// stays free of render types (ADR-0020).
    fn from(pos: phux_config::StatusPosition) -> Self {
        match pos {
            phux_config::StatusPosition::Bottom => Self::Bottom,
            phux_config::StatusPosition::Top => Self::Top,
        }
    }
}

/// Inputs for one frame of the bar, borrowed across the ratatui boundary.
#[derive(Debug, Clone, Copy)]
pub struct StatusBarContext<'a> {
    /// Wall-clock time the bar is rendering at.
    pub now: SystemTime,
    /// Current session name (`""` if not in a session).
    pub session_name: &'a str,
    /// Configured prefix chord.
    pub prefix: &'a str,
    /// The TUI's windows in display order (active one flagged), consumed
    /// by the `windows` widget. Empty ⇒ no window bar. TUI-side data fed
    /// into the widget pipeline via [`Self::as_widget`].
    pub windows: &'a [WindowInfo],
    /// The focused pane's live working directory (`""` when
    /// unknown), consumed by the `cwd` widget. Injected by the painter
    /// from driver-fed state, like `windows`.
    pub cwd: &'a str,
    /// The focused pane's last known command exit code
    /// (OSC-133 `command_finished`), consumed by the `exit` widget.
    pub last_exit: Option<i32>,
}

impl<'a> StatusBarContext<'a> {
    /// Convert to the lower-level [`WidgetContext`] expected by
    /// [`StatusBar::render`].
    #[must_use]
    pub const fn as_widget(&self) -> WidgetContext<'a> {
        WidgetContext {
            now: self.now,
            session_name: self.session_name,
            prefix: self.prefix,
            windows: self.windows,
            cwd: self.cwd,
            last_exit: self.last_exit,
            // Placeholder: `StatusBar::render` stamps the real row width
            // (the one place that knows it) before any widget sees this.
            cols: 0,
        }
    }
}

/// Build a [`StatusBarContext`] for one render pass. Window list, cwd, and
/// last exit are injected by the painter.
#[must_use]
pub const fn make_context(session_name: &str, now: SystemTime) -> StatusBarContext<'_> {
    StatusBarContext {
        now,
        session_name,
        prefix: "C-a",
        windows: &[],
        cwd: "",
        last_exit: None,
    }
}

mod cells;
mod notice;
mod paint;

pub use notice::{NOTICE_TTL, Notice, NoticeSeverity};

/// Columns the bar yields at each viewport edge to a docked sidebar, so its
/// tabs never paint under the strip. [`Self::NONE`] is a full-width bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BarInset {
    /// Columns yielded at the left edge.
    pub left: u16,
    /// Columns yielded at the right edge.
    pub right: u16,
}

impl BarInset {
    /// The full-width bar: no sidebar docked, nothing yielded.
    pub const NONE: Self = Self { left: 0, right: 0 };

    /// The bar's origin column and width in a `cols`-wide viewport
    /// (saturating: an oversized inset is a zero-width, no-op bar).
    #[must_use]
    pub const fn span(self, cols: u16) -> (u16, u16) {
        // `Ord::min` is not const for u16.
        let x = if self.left < cols { self.left } else { cols };
        let width = cols.saturating_sub(self.left).saturating_sub(self.right);
        (x, width)
    }
}

/// Whether a bar paint may reuse its last composed strip. The painter sees
/// its own inputs (each setter invalidates) but not the clock, `exec` caches,
/// or the session name, so the caller decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ComposePolicy {
    /// Run the widget pipeline. Every trigger that can have moved an input the
    /// painter does not own uses this: the 1 s poll tick, a chrome repaint, a
    /// full-frame redraw, and every standalone caller.
    Always,
    /// Reuse the cached strip unless a setter invalidated it; for pane-output
    /// paints, which change no bar input.
    WhenDirty,
}

/// VT painter for a composed [`StatusBar`], caching the last row so an
/// unchanged repaint is a no-op.
pub struct StatusBarPainter {
    bar: StatusBar,
    position: Position,
    /// Last painted strip with its `(x, width)` span (a sidebar toggle moves
    /// an otherwise identical row); also what hit tests read. `None` forces a
    /// paint.
    last_row: Option<(u16, u16, Vec<WidgetCell>)>,
    /// Last (cols, rows) we painted into. Different dims invalidate
    /// `last_row` and force a fresh paint.
    last_viewport: Option<(u16, u16)>,
    /// The window list fed to the `windows` widget. Updated
    /// by the driver from the `ArrangementDoc` and injected into the render
    /// context inside [`Self::paint`]; a change invalidates the cache.
    windows: Vec<WindowInfo>,
    /// A fixed config-error line painted instead of the widgets.
    error: Option<String>,
    /// The notice slot and its expiry; never set under the error line or on
    /// an empty bar.
    notice: Option<(Notice, std::time::Instant)>,
    /// ADR-0033 supervisory badge overlaid right-aligned on the widget row.
    supervisory: Option<String>,
    /// Agent-attention hint overlaid just left of the badge.
    attention: Option<String>,
    /// The theme every bar colour resolves through: `surface` is the row's
    /// bed (the sidebar's material), `attention` the hint chip, `accent` the
    /// prefix chip and supervisory badge, and `theme.<slot>` widget colours
    /// name any slot.
    theme: Theme,
    prefix: String,
    /// The resolver is waiting at the prefix: the chip at the right edge
    /// says so the instant the chord lands, before which-key's delay.
    prefix_pending: bool,
    /// The active key mode: its name rides the same right-edge chip while
    /// the prefix is not pending, so a user always sees the mode is on.
    key_mode: Option<String>,
    /// The focused pane's live cwd for the `cwd` widget (`None` renders
    /// nothing).
    focused_cwd: Option<String>,
    /// The focused pane's last known command exit code, fed
    /// by the driver from `command_finished` events. `None` ⇒ unknown.
    last_exit: Option<i32>,
    /// Window index under a live tab drag, painted as the insertion
    /// marker. `None` when no tab drag is over this strip.
    drop_at: Option<usize>,
}

impl std::fmt::Debug for StatusBarPainter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatusBarPainter")
            .field("bar", &self.bar)
            .field("position", &self.position)
            .field(
                "last_row.len",
                &self.last_row.as_ref().map(|(_, _, r)| r.len()),
            )
            .field("last_viewport", &self.last_viewport)
            .field("windows.len", &self.windows.len())
            .field("error", &self.error)
            .field("notice", &self.notice)
            .field("supervisory", &self.supervisory)
            .field("attention", &self.attention)
            .field("theme", &self.theme)
            .field("prefix", &self.prefix)
            .field("prefix_pending", &self.prefix_pending)
            .field("key_mode", &self.key_mode)
            .field("focused_cwd", &self.focused_cwd)
            .field("last_exit", &self.last_exit)
            .field("drop_at", &self.drop_at)
            .finish()
    }
}

impl StatusBarPainter {
    /// Build a painter from an already-composed [`StatusBar`].
    #[must_use]
    pub fn new(bar: StatusBar, position: Position) -> Self {
        Self {
            bar,
            position,
            last_row: None,
            last_viewport: None,
            windows: Vec::new(),
            error: None,
            notice: None,
            supervisory: None,
            attention: None,
            theme: Theme::default(),
            prefix: "C-a".to_owned(),
            prefix_pending: false,
            key_mode: None,
            focused_cwd: None,
            last_exit: None,
            drop_at: None,
        }
    }

    /// A painter showing a fixed error line instead of widgets, used when the
    /// config fails to load: it is never empty and always polls, so the
    /// diagnostic (pointing at `phux config check`) stays on screen.
    #[must_use]
    pub fn error_line(message: impl Into<String>) -> Self {
        Self {
            bar: StatusBar::empty(),
            position: Position::default(),
            last_row: None,
            last_viewport: None,
            windows: Vec::new(),
            error: Some(message.into()),
            notice: None,
            supervisory: None,
            attention: None,
            theme: Theme::default(),
            prefix: "C-a".to_owned(),
            prefix_pending: false,
            key_mode: None,
            focused_cwd: None,
            last_exit: None,
            drop_at: None,
        }
    }

    /// Whether this painter shows the config-error line (a lesser diagnostic
    /// must not replace it).
    #[must_use]
    pub const fn is_error_line(&self) -> bool {
        self.error.is_some()
    }

    /// Which row this painter reserves ([`Position::Bottom`] or
    /// [`Position::Top`]). The paint/layout helpers read this so the pane
    /// content rect and the bar row agree on the reservation.
    #[must_use]
    pub const fn position(&self) -> Position {
        self.position
    }

    /// Set the configured prefix chord exposed to prefix-aware widgets.
    pub fn set_prefix(&mut self, prefix: impl Into<String>) {
        let prefix = prefix.into();
        if self.prefix != prefix {
            self.prefix = prefix;
            self.invalidate();
        }
    }

    /// Update the `windows` widget's list; true when it changed. A change
    /// invalidates the row cache.
    pub fn set_windows(&mut self, windows: Vec<WindowInfo>) -> bool {
        if self.windows == windows {
            return false;
        }
        self.windows = windows;
        self.invalidate();
        true
    }

    /// Point (or clear) the tab-drag insertion marker at window `drop_at`;
    /// true when it changed. No-op on the error line.
    pub fn set_drop_index(&mut self, drop_at: Option<usize>) -> bool {
        if self.error.is_some() || self.drop_at == drop_at {
            return false;
        }
        self.drop_at = drop_at;
        self.invalidate();
        true
    }

    /// Set (or clear, with `None`) the focused pane's live
    /// working directory rendered by the `cwd` widget. Returns `true` if
    /// the value actually changed; a change invalidates the row cache.
    pub fn set_focused_cwd(&mut self, cwd: Option<String>) -> bool {
        if self.focused_cwd == cwd {
            return false;
        }
        self.focused_cwd = cwd;
        self.invalidate();
        true
    }

    /// Set (or clear, with `None`) the focused pane's last
    /// command exit code rendered by the `exit` widget. Returns `true` if
    /// the value actually changed; a change invalidates the row cache.
    pub fn set_last_exit(&mut self, last_exit: Option<i32>) -> bool {
        if self.last_exit == last_exit {
            return false;
        }
        self.last_exit = last_exit;
        self.invalidate();
        true
    }

    /// Show `notice` for [`NOTICE_TTL`] from `now`, replacing any current
    /// one; true when accepted. Refused (logged instead) under the error line,
    /// or on an empty bar, which reserves no row to paint it on
    /// (`docs/consumers/tui.md` §8.7).
    pub fn set_notice(&mut self, notice: Notice, now: std::time::Instant) -> bool {
        if self.error.is_some() {
            tracing::info!(
                severity = ?notice.severity,
                text = %notice.text,
                "status-bar notice suppressed under the persistent error line",
            );
            return false;
        }
        if self.bar.is_empty() {
            tracing::info!(
                severity = ?notice.severity,
                text = %notice.text,
                "status-bar notice dropped: no bar row is reserved (empty [status] config)",
            );
            return false;
        }
        self.notice = Some((notice, now + NOTICE_TTL));
        self.invalidate();
        true
    }

    /// Drop the notice once its deadline passes (from the status tick); true
    /// when cleared.
    pub fn clear_expired_notice(&mut self, now: std::time::Instant) -> bool {
        match &self.notice {
            Some((_, deadline)) if now >= *deadline => {
                self.notice = None;
                self.invalidate();
                true
            }
            _ => false,
        }
    }

    /// Whether `expected` currently owns the transient notice slot.
    pub(crate) fn notice_is(&self, expected: &str) -> bool {
        self.notice
            .as_ref()
            .is_some_and(|(notice, _)| notice.text == expected)
    }

    /// ADR-0033: set or clear the supervisory badge; true when it changed. A
    /// no-op under the error line, where the badge cannot show.
    pub fn set_supervisory(&mut self, badge: Option<String>) -> bool {
        if self.error.is_some() || self.supervisory == badge {
            return false;
        }
        self.supervisory = badge;
        self.invalidate();
        true
    }

    /// Set or clear the attention hint; same contract as the badge.
    pub fn set_attention(&mut self, hint: Option<String>) -> bool {
        if self.error.is_some() || self.attention == hint {
            return false;
        }
        self.attention = hint;
        self.invalidate();
        true
    }

    /// Adopt `theme` for every bar colour; a change invalidates the row
    /// cache. The driver calls this at attach and on each config reload.
    pub fn set_theme(&mut self, theme: &Theme) {
        if self.theme != *theme {
            self.theme = *theme;
            self.invalidate();
        }
    }

    /// Show or clear the prefix chip; true when it changed, so the caller
    /// repaints the row. Nothing under the error line.
    pub fn set_prefix_pending(&mut self, pending: bool) -> bool {
        if self.error.is_some() || self.prefix_pending == pending {
            return false;
        }
        self.prefix_pending = pending;
        self.invalidate();
        true
    }

    /// Show the active key mode's name on the key chip, or clear it; true
    /// when it changed. Nothing under the error line.
    pub fn set_key_mode(&mut self, mode: Option<&str>) -> bool {
        if self.error.is_some() || self.key_mode.as_deref() == mode {
            return false;
        }
        self.key_mode = mode.map(str::to_owned);
        self.invalidate();
        true
    }

    /// Whether the prefix chip is up, for driver tests.
    #[cfg(test)]
    #[must_use]
    pub const fn prefix_pending_for_test(&self) -> bool {
        self.prefix_pending
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests;
