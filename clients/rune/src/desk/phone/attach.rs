//! A photo, video or file from a phone. UIKit owns the pickers and the
//! bytes; each pick goes the way a pasted image does: uploaded to the
//! pane's host, its path typed at the prompt, for the agent there to read.

use phux_client_runtime::control::{ControlPlane, Status};
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
    incarnation: Vec<u8>,
}

fn resource_key(control: &ControlPlane, terminal: &ResourceId) -> Option<ResourceKey> {
    control
        .organization()
        .resource_by_live(terminal)
        .map(|record| record.key)
}

impl Attaching {
    /// A slow native load may finish during recovery. Never hand unverified
    /// bytes to the ordinary replayable transfer queue.
    fn upload(
        &self,
        control: &mut ControlPlane,
        extension: &str,
        data: Vec<u8>,
    ) -> Result<u64, &'static str> {
        if control.status() != Status::Attached {
            return Err("Not attached: not connected.");
        }
        if !control
            .server()
            .is_some_and(|server| server.id == self.incarnation)
        {
            return Err("Not attached: this computer's server restarted.");
        }
        let listed = control.topology().is_some_and(|topology| {
            topology
                .panes
                .iter()
                .any(|pane| pane.terminal_id == self.terminal)
        });
        if !listed || resource_key(control, &self.terminal) != self.key {
            return Err("Not attached: that terminal closed.");
        }
        Ok(control.put_file(self.terminal.clone(), extension.to_owned(), data))
    }
}

impl Desk {
    /// The paperclip: ask the native host for its attachment menu.
    pub(in crate::desk) fn attach_from_phone(&mut self) {
        if let Some(native) = &mut self.native {
            native.paste_requested = false;
        }
        let captured = self.pastes_allowed() && self.begin_attaching();
        if let Some(native) = &mut self.native {
            native.attach_requested = captured;
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
        let Some((key, incarnation)) = self.attaching_origin(&terminal) else {
            self.say(NoticeKind::Error, "Not attached: not connected.");
            return false;
        };
        let Some(native) = &mut self.native else {
            return false;
        };
        native.attachments += 1;
        native.attaching.truncate(ANSWERABLE - 1);
        native.attaching.push_front(Attaching {
            id: native.attachments,
            terminal,
            key,
            server: self.link.server,
            incarnation,
        });
        true
    }

    fn attaching_origin(&self, terminal: &ResourceId) -> Option<(Option<ResourceKey>, Vec<u8>)> {
        let client = self.connection.client.as_ref()?;
        client.with_control(|control| {
            if control.status() != Status::Attached
                || control.connection_epoch() != self.connection.view_epoch
            {
                return None;
            }
            let server = control.server()?;
            if server.id.is_empty()
                || Some(server.id.as_slice()) != self.connection.view_server.as_deref()
            {
                return None;
            }
            Some((resource_key(control, terminal), server.id.clone()))
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
        if asked.server != self.link.server {
            return self.say(
                NoticeKind::Error,
                "Not attached: this phone moved to another computer.",
            );
        }
        if !self.pastes_allowed() || self.is_document(&asked.terminal) {
            return self.say(
                NoticeKind::Error,
                "Not attached: another field or question has the keyboard.",
            );
        }
        let Some(client) = &self.connection.client else {
            return self.say(NoticeKind::Error, "Not attached: not connected.");
        };
        // Validation and enqueue share one control lock; a driver reconnect
        // cannot slip between them. Wake for immediate refusal receipts too.
        let upload = client.with_control(|control| asked.upload(control, extension, data));
        client.wake();
        match upload {
            Err(why) => self.say(NoticeKind::Error, why),
            Ok(transfer) => self.track_upload(&asked.terminal, extension, transfer),
        }
    }
}

#[cfg(test)]
mod tests;
