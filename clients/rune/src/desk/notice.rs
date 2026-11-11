//! Words for the user: what kind of thing a notice says, how long it
//! stays, and the word a tip offers to press.

use std::time::Duration;

use rune::Tx;

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
    /// Put a line in front of the user.
    pub fn say(&mut self, kind: NoticeKind, text: impl Into<String>) {
        self.post(kind, text.into(), None);
    }

    /// A tip with a way to act on it: `label` is the button, `action` what
    /// pressing it does. Pressing dismisses the tip.
    pub fn offer(&mut self, text: impl Into<String>, label: &'static str, action: fn(&mut Desk)) {
        self.post(NoticeKind::Tip, text.into(), Some((label, action)));
    }

    /// The pressed offer's action, then the toast goes.
    pub fn take_offer(&mut self) {
        let Some((_, action)) = self.notice.take().and_then(|notice| notice.action) else {
            return;
        };
        action(self);
    }

    /// Once per install, on the first connection: point a newcomer at the
    /// keys worth learning first. The palette lists them; `Keyboard
    /// Shortcuts: Essentials` reopens the list any time.
    pub(crate) fn offer_tour(&mut self) {
        // The tour teaches chords; a phone has the accessory bar instead.
        if self.phone.is_some() || self.prefs.get::<bool>("tour_offered").unwrap_or(false) {
            return;
        }
        let _ = self.prefs.set("tour_offered", &true);
        self.offer(
            "New here? A few keys do most of the work.",
            "Show me",
            Self::show_essentials,
        );
    }

    fn post(&mut self, kind: NoticeKind, text: String, action: Option<Offer>) {
        let expires = self.clock + kind.lasts();
        self.notice = Some(Notice {
            kind,
            text,
            action,
            expires,
        });
        // This may come after the frame's `expire_notice`, in a handler or
        // later in `poll`: another poll sets its timer.
        if let Some(waker) = &self.waker {
            waker();
        }
    }

    pub fn dismiss_notice(&mut self) {
        self.notice = None;
    }

    /// Take down a notice whose time is up, and wake for the one still
    /// showing. "Uploading" stays for as long as that's true.
    pub(super) fn expire_notice(&mut self, tx: &mut Tx) -> bool {
        let Some(notice) = &self.notice else {
            return false;
        };
        if notice.kind == NoticeKind::Info && !self.uploads.is_empty() {
            return false;
        }
        let left = notice.expires - self.clock;
        if left > 0.0 {
            tx.wake_after(Duration::from_secs_f64(left));
            return false;
        }
        self.notice = None;
        true
    }
}
