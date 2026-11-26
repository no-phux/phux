//! [`Pending`]: the question the confirm bar holds.

use phux_protocol::ids::{ResourceId, SessionKey};

use super::{Desk, NoticeKind, resources};

/// What waits on the user's Return or Escape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    /// Explicit consent to start a supervised read-only mirror producer.
    Mirror(resources::Source, String),
    /// A line-breaking paste, and the terminal it was meant for.
    Paste(ResourceId, String),
    /// A draft containing line breaks that may execute as separate commands.
    SendDraft(super::phone::composer::Owner),
    /// Closing a terminal that is running a command.
    Close(ResourceId),
    /// Closing every terminal of a tab.
    CloseTab(Vec<ResourceId>),
    /// Ending a session and everything in it.
    CloseSession(SessionKey, String),
    /// A program's clipboard read (OSC 52).
    ClipboardRead(ResourceId, phux_client_core::engine::ClipboardSelection),
    /// A pane's request to open a link here.
    OpenUrl(Box<super::open_url::Asked>),
    /// A program's clipboard write (OSC 52) and the text it would set.
    ClipboardWrite(ResourceId, String),
    /// A pane's request to sign with a forwarded ssh key, in words. The
    /// question itself is the connection's.
    SshSign(String),
}

impl Desk {
    pub fn confirm_pending(&mut self) {
        match self.pending.take() {
            Some(Pending::Mirror(source, target)) => self.start_mirror(source, &target),
            Some(Pending::Paste(terminal, text)) => self.send_paste(&terminal, &text),
            Some(Pending::SendDraft(owner)) => self.confirm_composer(&owner),
            // Answered through `confirm_pending_tx`, which can reach the
            // clipboard and the browser.
            Some(
                Pending::ClipboardRead(..)
                | Pending::ClipboardWrite(..)
                | Pending::OpenUrl(_)
                | Pending::SshSign(_),
            ) => {}
            Some(Pending::Close(terminal)) => self.close_now(&terminal),
            Some(Pending::CloseTab(terminals)) => {
                let terminals = terminals
                    .into_iter()
                    .filter(|id| !self.is_document(id))
                    .collect();
                self.request_terminal_close(terminals);
            }
            Some(Pending::CloseSession(key, _)) => {
                if let Some(client) = &self.connection.client
                    && let Err(error) = client.kill_session(key)
                {
                    self.say(NoticeKind::Error, format!("Session not closed: {error}"));
                }
            }
            None => {}
        }
    }

    pub fn cancel_pending(&mut self) {
        self.decline_open_url();
        self.pending = None;
        self.cancel_raw_relaunch();
    }
}
