//! The per-pane unseen mark and how a view reports looking
//! (`docs/scripting.md#unseen-marks`).

/// Terminal-scoped key holding what happened in the pane since anyone last
/// looked ([`PaneUnseen`]).
///
/// UTF-8 `seen`, `output`, `bell`, or `finished`. Absent says nothing: the
/// pane is new, or an older server keeps no marks. Server-written: a
/// `SET_METADATA` is refused, and a `DELETE_METADATA` sets `seen`.
pub const RESOURCE_UNSEEN_KEY: &str = "phux.unseen/v1";

/// Terminal-scoped key reporting that the pane came into (`true`) or left
/// (`false`) the writing view.
///
/// Applied to the writing attachment, not stored or broadcast. Coming into
/// view sets [`RESOURCE_UNSEEN_KEY`] to `seen`, and nothing marks the pane
/// while any view shows it. The report ends with the attachment, so a client
/// reports again after each `ATTACHED`. An older server stores the value and
/// ignores it.
pub const RESOURCE_IN_VIEW_KEY: &str = "phux.in-view/v1";

/// A pane's [`RESOURCE_UNSEEN_KEY`] mark, weakest first: a weaker one never
/// replaces a stronger one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PaneUnseen {
    /// Nothing: someone looked.
    Seen,
    /// It printed something.
    Output,
    /// It rang its bell.
    Bell,
    /// Its agent finished.
    Finished,
}

impl PaneUnseen {
    /// The stored word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Seen => "seen",
            Self::Output => "output",
            Self::Bell => "bell",
            Self::Finished => "finished",
        }
    }

    /// The mark a stored value names; `None` for anything else.
    #[must_use]
    pub fn decode(value: &[u8]) -> Option<Self> {
        [Self::Seen, Self::Output, Self::Bell, Self::Finished]
            .into_iter()
            .find(|mark| mark.as_str().as_bytes() == value)
    }
}
