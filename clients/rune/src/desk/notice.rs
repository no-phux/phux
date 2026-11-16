//! Words for the user: what kind of thing a notice says, how long it
//! stays, and the word a tip offers to press.

use std::time::Duration;

use phux_client_core::note::{Note, NoteLevel};
use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::Desk;
use crate::alerts::{Alert, Target};

/// Set once the "New here?" tip has been shown, or onboarding taught its keys.
pub(super) const TOUR_OFFERED: &str = "tour_offered";

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
        if self.phone.is_some() || self.prefs.get::<bool>(TOUR_OFFERED).unwrap_or(false) {
            return;
        }
        let _ = self.prefs.set(TOUR_OFFERED, &true);
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

    /// A program posted a note for its pane (`phux notify`): a toast, and a
    /// system notification when a `warn` or `error` note's pane is out of
    /// sight.
    pub(super) fn note_posted(&mut self, terminal: &ResourceId, note: &Note) -> bool {
        // The main window speaks for both; the quick terminal would repeat it.
        if self.quick.is_some() {
            return false;
        }
        let pane = self
            .connection
            .topology
            .as_ref()
            .and_then(|t| t.pane(terminal));
        let who = pane.map_or_else(|| terminal.to_string(), crate::palette::pane_label);
        let session = pane.map(|p| p.session_name.clone()).unwrap_or_default();
        let text = match &note.body {
            Some(body) => format!("{who}: {} — {body}", note.title),
            None => format!("{who}: {}", note.title),
        };
        let kind = match note.level {
            NoteLevel::Error => NoticeKind::Error,
            NoteLevel::Warn | NoteLevel::Info => NoticeKind::Info,
        };
        self.say(kind, text);
        let seen = self.focused && self.on_screen().contains(terminal);
        if note.level != NoteLevel::Info && !seen && self.settings.notifications {
            self.inbox.alerts.post_now(Alert {
                // One per pane, so a newer note replaces the last.
                id: format!("{terminal}:note"),
                title: format!("{who}: {}", note.title),
                body: note.body.clone().unwrap_or_default(),
                target: Target::Terminal {
                    id: terminal.clone(),
                    session,
                },
            });
        }
        true
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
        if notice.kind == NoticeKind::Info && !self.connection.uploads.is_empty() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::Resolved;

    #[test]
    fn a_note_toasts_naming_its_pane_and_an_error_reads_as_one() {
        let mut desk = Desk::new(&Resolved::offline("notes"));
        desk.settings.notifications = false;
        let note = Note {
            level: NoteLevel::Error,
            title: "build failed".to_owned(),
            body: Some("3 errors".to_owned()),
            at: 1,
        };
        assert!(desk.note_posted(&ResourceId::local(3), &note));
        let notice = desk.notice.as_ref().expect("a toast");
        assert_eq!(notice.kind, NoticeKind::Error);
        assert!(
            notice.text.ends_with(": build failed — 3 errors"),
            "{}",
            notice.text
        );
    }
}
