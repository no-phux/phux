//! A photo, video or file from a phone. UIKit owns the pickers and the
//! bytes; each pick goes the way a pasted image does: uploaded to the
//! pane's host, its path typed at the prompt, for the agent there to read.

use phux_protocol::ids::{ResourceId, ResourceKey};

use super::super::{Desk, NoticeKind};

/// How many requests can still be answered: a photo may be loading from
/// iCloud while the next one, or a paste, is asked for.
const ANSWERABLE: usize = 8;

/// The pane an attachment is for, chosen when it was asked for: a picker can
/// stay open while the keyboard, the connection or the machine moves on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attaching {
    /// Names this request to the host, so a late answer still goes to the
    /// pane it was asked for.
    pub id: u64,
    terminal: ResourceId,
    /// What the pane is across reconnects, where the server says.
    key: Option<ResourceKey>,
    server: u64,
}

impl Desk {
    /// The paperclip: ask the native host for its attachment menu.
    pub(in crate::desk) fn attach_from_phone(&mut self) {
        if !self.pastes_allowed() {
            return;
        }
        if self.begin_attaching()
            && let Some(native) = &mut self.native
        {
            native.attach_requested = true;
        }
    }

    /// Note the pane with the keyboard as where the host's bytes will go.
    pub(in crate::desk) fn begin_attaching(&mut self) -> bool {
        let Some(terminal) = self
            .keyboard()
            .filter(|(terminal, view)| view.is_some() && !self.is_document(terminal))
            .map(|(terminal, _)| terminal.clone())
        else {
            self.say(NoticeKind::Error, "No terminal to attach to.");
            return false;
        };
        let (key, server) = (self.resource_key(&terminal), self.link.server);
        let Some(native) = &mut self.native else {
            return false;
        };
        native.attachments += 1;
        native.attaching.truncate(ANSWERABLE - 1);
        native.attaching.push_front(Attaching {
            id: native.attachments,
            terminal,
            key,
            server,
        });
        true
    }

    fn resource_key(&self, terminal: &ResourceId) -> Option<ResourceKey> {
        let client = self.connection.client.as_ref()?;
        client.with_control(|control| {
            let record = control.organization().resource_by_live(terminal)?;
            Some(record.key)
        })
    }

    /// The host's answer to request `id`: one picked file. `None`: it
    /// couldn't be read. Several answers may follow one request.
    pub fn attach_from_host(&mut self, id: u64, extension: &str, data: Option<Vec<u8>>) {
        let Some(data) = data.filter(|data| !data.is_empty()) else {
            return self.say(
                NoticeKind::Error,
                "Not attached: it couldn't be read, or is over 48 MB.",
            );
        };
        let asked = self.native.as_ref().and_then(|native| {
            let mut asked = native.attaching.iter();
            asked.find(|asked| asked.id == id).cloned()
        });
        let Some(asked) = asked else {
            return self.say(
                NoticeKind::Error,
                "Not attached: too much was asked for since.",
            );
        };
        let refusal = if asked.server != self.link.server {
            Some("Not attached: this phone moved to another computer.")
        } else if !self.pastes_allowed() {
            Some("Not attached: another field or question has the keyboard.")
        } else if self.connection.client.is_none() {
            Some("Not attached: not connected.")
        } else if self.connection.topology.is_some()
            && (!self.lists(&asked.terminal) || self.resource_key(&asked.terminal) != asked.key)
        {
            Some("Not attached: that terminal closed.")
        } else {
            None
        };
        match refusal {
            Some(why) => self.say(NoticeKind::Error, why),
            None => self.upload(&asked.terminal, extension, data),
        }
    }
}

#[cfg(test)]
mod tests;
