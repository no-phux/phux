//! Words for the user: what kind of thing a notice says, how long it
//! stays, and the word a tip offers to press.

use super::Desk;

/// What kind of thing a notice says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    Info,
    Error,
    /// Something worth knowing, with a way to act on it; stays until read.
    Tip,
}

impl NoticeKind {
    /// Seconds on screen; a failure stays long enough to be read twice.
    pub(super) const fn lasts(self) -> f64 {
        match self {
            Self::Info => 6.0,
            Self::Error => 12.0,
            Self::Tip => 30.0,
        }
    }
}

/// A word to press on a tip, and what pressing it does.
pub type Offer = (&'static str, fn(&mut Desk));

/// Words for the user, in a toast and the inbox's footer.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub kind: NoticeKind,
    pub text: String,
    /// A word to press and what it does; the toast shows it as a button.
    pub action: Option<Offer>,
    /// When it goes, on the app clock.
    pub(super) expires: f64,
}

impl Desk {
    /// The pressed offer's action, then the toast goes.
    pub fn take_offer(&mut self) {
        let Some((_, action)) = self.notice.take().and_then(|notice| notice.action) else {
            return;
        };
        action(self);
    }

    pub fn dismiss_notice(&mut self) {
        self.notice = None;
    }
}
