//! Words the native host heard for one session (the iOS Action Button's
//! "Dictate to Phux"): shown in that session, then sent as the composer
//! sends, one acknowledged paste and Enter. The session may not be on
//! screen yet, or the computer not attached, so the words wait until it is.
//! They are never typed into whatever else happens to be showing.
use std::time::{Duration, Instant};

use rune::TextEdit;

use super::super::{Desk, NoticeKind};

/// How long dictated words wait for their session before they are given up.
const WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct Dictation {
    session: String,
    text: String,
    since: Instant,
    /// The session has been asked to show.
    shown: bool,
}

impl Desk {
    /// Send `text` to session `session`'s focused terminal once it shows.
    pub fn dictate(&mut self, session: &str, text: &str) {
        let (session, text) = (session.trim(), text.trim());
        if session.is_empty() || text.is_empty() {
            return;
        }
        let Some(phone) = self.phone.as_mut() else {
            return;
        };
        phone.dictation = Some(Dictation {
            session: session.to_owned(),
            text: text.to_owned(),
            since: Instant::now(),
            shown: false,
        });
        self.follow_dictation();
    }

    /// Move waiting dictation on: show its session, then send. True while it
    /// still waits.
    pub(in crate::desk) fn follow_dictation(&mut self) -> bool {
        let Some(dictation) = self.phone.as_mut().and_then(|p| p.dictation.take()) else {
            return false;
        };
        if dictation.since.elapsed() > WAIT {
            let why = format!(
                "Dictation not sent: session {:?} didn't take it in time. It said: {}",
                dictation.session, dictation.text
            );
            self.say(NoticeKind::Error, why);
            return false;
        }
        let Some(waiting) = self.dictation_step(dictation) else {
            return false;
        };
        if let Some(phone) = self.phone.as_mut() {
            phone.dictation = Some(waiting);
        }
        true
    }

    /// One step: `Some` to keep waiting.
    fn dictation_step(&mut self, mut dictation: Dictation) -> Option<Dictation> {
        let Some(topology) = self.connection.topology.as_ref() else {
            return Some(dictation);
        };
        if !topology
            .sessions
            .iter()
            .any(|s| s.name == dictation.session)
        {
            let why = format!(
                "Dictation not sent: there is no session {:?}. Make one, or name another for the \
                 Action Button in Shortcuts.",
                dictation.session
            );
            self.say(NoticeKind::Error, why);
            return None;
        }
        let showing = self.connection.shown.as_ref().is_some_and(|shown| {
            topology
                .pane(&shown.resource)
                .is_some_and(|p| p.session_name == dictation.session)
        });
        if !dictation.shown {
            // The pages the phone puts over its terminal; the words are for it.
            dictation.shown = true;
            self.close_inbox();
            self.close_phone_navigation();
            self.close_settings();
            if !showing {
                self.show_session(&dictation.session);
            }
        }
        if !showing {
            return Some(dictation);
        }
        let Some(owner) = self.composer_owner() else {
            return Some(dictation);
        };
        let ready = self.connection.client.as_ref().is_some_and(|client| {
            client.with_control(|control| control.input_ready(owner.terminal()))
        });
        if !ready {
            return Some(dictation);
        }
        self.deliver_dictation(&owner, &dictation.text);
        None
    }

    /// Into `owner`'s composer and sent, or added to a draft already there.
    fn deliver_dictation(&mut self, owner: &super::composer::Owner, text: &str) {
        let Some(phone) = self.phone.as_mut() else {
            return;
        };
        let draft = phone.composer.draft_entry(owner);
        let focused = draft.edit.focused;
        // A draft of the user's own there is theirs: the words join it, to be
        // read and sent by hand, and are never sent along with it unseen. One
        // still on its way keeps its own text; the words wait as the next.
        let own = draft.edit.text.trim_end();
        let kept = if draft.sending.is_some() {
            Some(text.to_owned())
        } else if own.is_empty() {
            None
        } else {
            Some(format!("{own} {text}"))
        };
        draft.edit = TextEdit::new(kept.as_deref().unwrap_or(text));
        draft.edit.focused = focused;
        if kept.is_some() {
            return self.say(
                NoticeKind::Info,
                "Dictation added to your draft. Send it when it reads right.",
            );
        }
        // Lines after the first would each run: those ask first, as typed ones do.
        self.submit_composer(owner, !text.contains(['\n', '\r']));
    }
}
