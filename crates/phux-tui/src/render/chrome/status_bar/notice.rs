//! Transient notices: the chip a lifecycle event shows on the bar.

use std::time::Duration;

use ratatui::style::{Modifier, Style};

use crate::render::theme::Theme;

/// How long a transient [`Notice`] stays on the bar (expiry rides the 1 s
/// status tick).
pub const NOTICE_TTL: Duration = Duration::from_secs(7);

/// A [`Notice`]'s chip style: `Info` rides the selection pair, `Warn` is
/// bold selection ink on the theme's `error` slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeSeverity {
    /// Informational lifecycle event (e.g. an input-lease handover).
    Info,
    /// Something is degraded and the user should know (e.g. a federation
    /// satellite became unreachable).
    Warn,
}

impl NoticeSeverity {
    /// Full-row style for a notice of this severity.
    pub(super) fn style(self, theme: &Theme) -> Style {
        match self {
            Self::Info => Style::default()
                .fg(theme.selection_fg)
                .bg(theme.selection_bg),
            Self::Warn => Style::default()
                .fg(theme.selection_fg)
                .bg(theme.error)
                .add_modifier(Modifier::BOLD),
        }
    }
}

/// A transient, self-expiring status-bar message (input handovers, degraded
/// federation, pane exits).
///
/// One newest-wins slot restarting the
/// [`NOTICE_TTL`] clock, painted as a right-aligned chip over the bar; the
/// persistent error line outranks it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Render severity; see [`NoticeSeverity`].
    pub severity: NoticeSeverity,
    /// The message painted across the bar row (truncated to the span).
    pub text: String,
}

impl Notice {
    /// An [`NoticeSeverity::Info`] notice.
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            severity: NoticeSeverity::Info,
            text: text.into(),
        }
    }

    /// A [`NoticeSeverity::Warn`] notice.
    pub fn warn(text: impl Into<String>) -> Self {
        Self {
            severity: NoticeSeverity::Warn,
            text: text.into(),
        }
    }
}
