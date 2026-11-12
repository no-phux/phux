//! Answering a pane's OSC 52 clipboard read
//! (`docs/program-sequences.md#clipboard-osc-52`).

use phux_client_core::engine::ClipboardSelection;
use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::input::paste::ClipboardData;
use phux_protocol::wire::frame::MAX_CLIPBOARD_TEXT;

use super::{ControlPlane, Event, FrameKind, ResourceId, Status};

impl ControlPlane {
    /// Whether the server takes `CLIPBOARD_REPLY`. A host that sees false
    /// leaves reads unanswered and never asks its user.
    #[must_use]
    pub fn clipboard_replies_supported(&self) -> bool {
        self.server
            .as_ref()
            .is_some_and(|server| server.has_ext(ServerFeatureExt::ClipboardRead))
    }

    /// A replica saw an OSC 52 read: surface it only from the focused
    /// terminal of an attached session, and only when it can be answered.
    pub(super) fn note_clipboard_read(
        &mut self,
        terminal_id: ResourceId,
        selection: ClipboardSelection,
    ) {
        if self.status == Status::Attached
            && self.clipboard_focus.as_ref() == Some(&terminal_id)
            && self.clipboard_replies_supported()
        {
            self.push_event(Event::ClipboardRead {
                terminal_id,
                selection,
            });
        }
    }

    /// Answer `terminal_id`'s OSC 52 read of `selection` with `text`, the
    /// user's clipboard, once they allowed it. False, and nothing is sent,
    /// when the server does not take replies, the session is not attached,
    /// input to the terminal is fenced, or the text is over 1 MiB.
    pub fn send_clipboard_reply(
        &mut self,
        terminal_id: &ResourceId,
        selection: ClipboardSelection,
        text: String,
    ) -> bool {
        if !self.clipboard_replies_supported()
            || self.status != Status::Attached
            || self.delivery_fenced(terminal_id)
            || text.len() > MAX_CLIPBOARD_TEXT
        {
            return false;
        }
        self.queue_frame(&FrameKind::ClipboardReply {
            terminal_id: terminal_id.clone(),
            selection: selection.byte(),
            text: ClipboardData(text),
        });
        true
    }
}
