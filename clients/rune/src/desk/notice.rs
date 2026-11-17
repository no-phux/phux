//! Words for the user: what kind of thing a notice says, how long it
//! stays, and the word a tip offers to press.
//!
//! Notices are toasts in a kit [`ToastStack`]: the newest in front, older
//! ones peeking behind it, and timers that pause while the pointer holds
//! the stack open or the phone app is away. The stack lives here; the
//! toast region draws [`Desk::toasts`] and steps it with
//! [`Desk::toast_input`]. `Desk::notice` mirrors the front toast for the
//! readers that show one line (the inbox footer).

use std::time::Duration;

use phux_client_core::note::{Note, NoteLevel};
use phux_protocol::ids::ResourceId;
use rune::Tx;
use rune::machine::Effects;
use rune_kit::toast_stack::{
    ToastEffect, ToastEntry, ToastEvent, ToastPage, ToastStack, ToastStackState, ToastTimer,
};

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
}

/// What the toast region tells the stack. The stack reads the window's
/// clock itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastInput {
    /// The pointer came onto the stack or moved over it: it expands.
    PointerOver,
    /// The pointer left the stack: it collapses.
    PointerLeft,
}

impl ToastInput {
    const fn event(self, now: f64) -> ToastEvent<Notice> {
        match self {
            Self::PointerOver => ToastEvent::PointerOver { now },
            Self::PointerLeft => ToastEvent::PointerLeft { now },
        }
    }
}

/// The window's toasts: the kit stack.
#[derive(Debug, Default)]
pub struct Toasts {
    stack: ToastStack<Notice>,
}

impl Toasts {
    /// Oldest first; the last is in front.
    pub fn entries(&self) -> &[ToastEntry<Notice>] {
        self.stack.entries()
    }

    /// Collapsed, expanded, or held by a press.
    pub fn state(&self) -> ToastStackState {
        self.stack.stack()
    }

    /// Every toast lays out at its own height.
    pub fn expanded(&self) -> bool {
        self.state().expanded()
    }

    /// The toast in front.
    pub fn front(&self) -> Option<&ToastEntry<Notice>> {
        self.stack.front()
    }

    /// The app-clock time the next timer comes due.
    pub fn next_due(&self) -> Option<f64> {
        self.stack.next_due()
    }

    pub fn is_empty(&self) -> bool {
        self.entries().is_empty()
    }

    fn ids(&self, pick: impl Fn(&Notice) -> bool) -> Vec<u64> {
        let picked = self.entries().iter().filter(|e| pick(&e.options));
        picked.map(|e| e.id).collect()
    }
}

/// The key two copies of one notice share, so a repeat pulses in place.
fn key_of(notice: &Notice) -> String {
    format!("{:?}:{}", notice.kind, notice.text)
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

    /// The window's toasts, for the toast region.
    pub const fn toasts(&self) -> &Toasts {
        &self.toasts
    }

    /// The toast region heard the pointer.
    pub fn toast_input(&mut self, input: ToastInput, tx: &mut Tx) {
        let effects = self.toasts.stack.step(input.event(self.clock));
        self.toast_effects(effects, Some(tx));
    }

    /// Toast `id`'s offer: the toast goes, then its action runs.
    pub fn take_offer_of(&mut self, id: u64) {
        let action = (self.toasts.entries().iter())
            .find(|e| e.id == id)
            .and_then(|e| e.options.action);
        self.dismiss_toast(id);
        if let Some((_, action)) = action {
            action(self);
        }
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
        let options = Notice { kind, text, action };
        let effects = self.toasts.stack.step(ToastEvent::Shown {
            key: key_of(&options),
            options,
            secs: Some(kind.lasts()),
            now: self.clock,
        });
        self.toast_effects(effects, None);
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

    /// The front toast goes; the one behind it comes forward.
    pub fn dismiss_notice(&mut self) {
        if let Some(id) = self.toasts.front().map(|e| e.id) {
            self.dismiss_toast(id);
        }
    }

    /// Toast `id` goes (its close button).
    pub fn dismiss_toast(&mut self, id: u64) {
        let now = self.clock;
        let effects = (self.toasts.stack).step(ToastEvent::Dismissed { id: Some(id), now });
        self.toast_effects(effects, None);
    }

    /// Every toast goes.
    pub fn clear_notices(&mut self) {
        let now = self.clock;
        let effects = self
            .toasts
            .stack
            .step(ToastEvent::Dismissed { id: None, now });
        self.toast_effects(effects, None);
    }

    /// Every toast that `pick` names goes. True when one did.
    pub(super) fn dismiss_notices(&mut self, pick: impl Fn(&Notice) -> bool) -> bool {
        let ids = self.toasts.ids(pick);
        for id in &ids {
            self.dismiss_toast(*id);
        }
        !ids.is_empty()
    }

    /// The window shows or is away (a phone app in the background): no
    /// toast's time runs while it is away.
    pub(super) fn toasts_shown(&mut self, shown: bool) {
        let page = if shown {
            ToastPage::Shown
        } else {
            ToastPage::Hidden
        };
        let now = self.clock;
        let effects = self.toasts.stack.step(ToastEvent::Page { page, now });
        self.toast_effects(effects, None);
    }

    /// Take down the toasts whose time is up, and wake for the next one.
    /// "Uploading" stays for as long as that's true.
    pub(super) fn expire_notice(&mut self, tx: &mut Tx) -> bool {
        let released = self.hold_for_uploads();
        let now = self.clock;
        let effects = self.toasts.stack.step(ToastEvent::Tick { now });
        self.toast_effects(effects, Some(tx)) | released
    }

    /// An info toast whose time is up waits, untimed, while an upload is on
    /// its way, and goes once none is. True when one went.
    fn hold_for_uploads(&mut self) -> bool {
        let uploading = !self.connection.uploads.is_empty();
        let now = self.clock;
        for entry in self.toasts.stack.entries_mut() {
            let due = matches!(entry.timer, ToastTimer::Running { due } if now + 1e-3 >= due);
            if uploading && due && entry.options.kind == NoticeKind::Info {
                entry.timer = ToastTimer::Forever;
            }
        }
        !uploading && self.release_held()
    }

    /// Info toasts are always timed, so an untimed one is one an upload
    /// held: it goes.
    fn release_held(&mut self) -> bool {
        let held = |e: &&ToastEntry<Notice>| {
            e.options.kind == NoticeKind::Info && e.timer == ToastTimer::Forever
        };
        let ids: Vec<u64> = self
            .toasts
            .entries()
            .iter()
            .filter(held)
            .map(|e| e.id)
            .collect();
        for id in &ids {
            self.dismiss_toast(*id);
        }
        !ids.is_empty()
    }

    /// What a step asks for: a wake for the next timer (a repeat's pulse
    /// is not drawn).
    /// True when a toast closed on its own.
    fn toast_effects(&mut self, effects: Effects<ToastEffect>, mut tx: Option<&mut Tx>) -> bool {
        let mut closed = false;
        for effect in effects {
            match effect {
                ToastEffect::Pulse(_) => {}
                ToastEffect::Closed(_) => closed = true,
                ToastEffect::WakeAt(due) => {
                    if let Some(tx) = tx.as_deref_mut() {
                        tx.wake_after(Duration::from_secs_f64((due - self.clock).max(0.0)));
                    }
                }
            }
        }
        self.notice = self.toasts.front().map(|e| e.options.clone());
        closed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::Resolved;

    fn desk() -> Desk {
        Desk::new(&Resolved::offline("notes"))
    }

    fn texts(desk: &Desk) -> Vec<&str> {
        let entries = desk.toasts().entries().iter();
        entries.map(|e| e.options.text.as_str()).collect()
    }

    fn tick(desk: &mut Desk, clock: f64) -> bool {
        desk.clock = clock;
        desk.expire_notice(&mut Tx::default())
    }

    #[test]
    fn a_note_toasts_naming_its_pane_and_an_error_reads_as_one() {
        let mut desk = desk();
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

    #[test]
    fn a_toast_closes_when_its_time_is_up_and_wakes_for_it() {
        let mut desk = desk();
        desk.clock = 10.0;
        desk.say(NoticeKind::Info, "Copied");
        assert_eq!(desk.toasts().next_due(), Some(16.0), "six seconds");
        assert!(!tick(&mut desk, 15.9), "not yet");
        assert_eq!(
            desk.notice.as_ref().map(|n| n.text.as_str()),
            Some("Copied")
        );
        assert!(tick(&mut desk, 16.0), "its time is up");
        assert_eq!(desk.notice, None);
        assert!(desk.toasts().is_empty());
        assert_eq!(desk.toasts().next_due(), None, "nothing to wake for");
    }

    #[test]
    fn toasts_stack_newest_in_front_and_each_keeps_its_own_time() {
        let mut desk = desk();
        desk.say(NoticeKind::Error, "Rename failed");
        desk.clock = 1.0;
        desk.say(NoticeKind::Info, "Copied");
        assert_eq!(texts(&desk), ["Rename failed", "Copied"]);
        assert_eq!(
            desk.notice.as_ref().map(|n| n.text.as_str()),
            Some("Copied")
        );
        assert!(tick(&mut desk, 7.0), "the info toast goes at 7");
        assert_eq!(texts(&desk), ["Rename failed"], "the error stays to 12");
        assert!(tick(&mut desk, 12.0));
        assert!(desk.toasts().is_empty());
    }

    #[test]
    fn a_repeat_of_the_front_toast_stays_in_place_and_restarts_its_time() {
        let mut desk = desk();
        desk.say(NoticeKind::Info, "Copied");
        desk.clock = 4.0;
        desk.say(NoticeKind::Info, "Copied");
        assert_eq!(texts(&desk), ["Copied"], "not a second copy");
        assert_eq!(desk.toasts().next_due(), Some(10.0), "six from the repeat");
    }

    #[test]
    fn the_pointer_over_the_stack_pauses_every_timer_until_it_leaves() {
        let mut desk = desk();
        desk.say(NoticeKind::Info, "Copied");
        desk.clock = 2.0;
        desk.toast_input(ToastInput::PointerOver, &mut Tx::default());
        assert!(desk.toasts().expanded());
        assert_eq!(desk.toasts().next_due(), None, "paused");
        assert!(!tick(&mut desk, 30.0), "held open, it stays");
        desk.toast_input(ToastInput::PointerLeft, &mut Tx::default());
        assert_eq!(desk.toasts().next_due(), Some(34.0), "the four it had left");
        assert!(tick(&mut desk, 34.0));
    }

    #[test]
    fn an_away_phone_app_stops_the_clock() {
        let mut desk = desk();
        desk.say(NoticeKind::Info, "Copied");
        desk.clock = 1.0;
        desk.toasts_shown(false);
        assert!(!tick(&mut desk, 60.0), "no time runs while away");
        desk.toasts_shown(true);
        assert_eq!(desk.toasts().next_due(), Some(65.0));
    }

    #[test]
    fn an_offer_is_taken_from_its_own_toast_and_that_toast_goes() {
        fn opened(desk: &mut Desk) {
            desk.settings_open = true;
        }
        let mut desk = desk();
        desk.offer("New here?", "Show me", opened);
        let id = desk.toasts().front().map(|e| e.id).expect("a tip");
        desk.say(NoticeKind::Info, "Copied");
        desk.take_offer_of(id);
        assert!(desk.settings_open, "the tip's action ran");
        assert_eq!(texts(&desk), ["Copied"], "only the tip went");
    }

    #[test]
    fn dismissing_the_front_toast_brings_the_next_forward() {
        let mut desk = desk();
        desk.say(NoticeKind::Error, "first");
        desk.say(NoticeKind::Error, "second");
        desk.dismiss_notice();
        assert_eq!(desk.notice.as_ref().map(|n| n.text.as_str()), Some("first"));
        desk.clear_notices();
        assert_eq!(desk.notice, None);
    }

    #[test]
    fn an_info_toast_waits_out_an_upload_then_goes() {
        let mut desk = desk();
        desk.connection
            .uploads
            .push((2, ResourceId::local(1), None));
        desk.say(NoticeKind::Info, "Uploading image…");
        desk.say(NoticeKind::Error, "Rename failed");
        assert!(tick(&mut desk, 12.0), "the error's time is up");
        assert_eq!(texts(&desk), ["Uploading image…"], "uploading stays");
        assert!(!tick(&mut desk, 100.0));
        desk.connection.uploads.clear();
        assert!(tick(&mut desk, 100.1), "the upload is over");
        assert!(desk.toasts().is_empty());
    }
}
