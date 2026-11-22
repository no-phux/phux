//! [`FrameKind::encode`]'s dispatch: every variant routed to its generated
//! `field::<frame>::encode` or to the family writer that owns its field
//! order.

use crate::wire::encode::Encoder;
use crate::wire::field;

use super::FrameKind;

impl FrameKind {
    /// Write the field-tagged payload of `self`; the type byte is already out.
    ///
    /// Exhaustive, so a new variant does not compile until it is routed;
    /// each family writer then matches only the variants routed to it.
    pub(super) fn encode_payload(&self, enc: &mut Encoder<'_>) {
        match self {
            Self::Hello { .. }
            | Self::HelloOk { .. }
            | Self::Ping { .. }
            | Self::Pong { .. }
            | Self::ResourceOutput { .. }
            | Self::Detach
            | Self::Detached { .. }
            | Self::InputTerminalReply { .. } => self.encode_connection_payload(enc),
            Self::Attach { .. } => field::attach::encode(self, enc),
            Self::InputKey { .. } => field::input_key::encode(self, enc),
            Self::InputMouse { .. } => field::input_mouse::encode(self, enc),
            Self::InputFocus { .. } => field::input_focus::encode(self, enc),
            Self::InputPaste { .. } => field::input_paste::encode(self, enc),
            Self::ClipboardReply { .. } => field::clipboard_reply::encode(self, enc),
            Self::FrameAck { .. } => field::frame_ack::encode(self, enc),
            Self::ViewportResize { .. } => field::viewport_resize::encode(self, enc),
            Self::ViewportColors { .. } => field::viewport_colors::encode(self, enc),
            Self::Attached { .. } => field::attached::encode(self, enc),
            Self::AttachReady { .. } => field::attach_ready::encode(self, enc),
            Self::BootstrapBegin { .. }
            | Self::BootstrapChunk { .. }
            | Self::BootstrapReady { .. }
            | Self::HistoryRequest { .. }
            | Self::HistoryPage { .. }
            | Self::BootstrapTombstone { .. }
            | Self::HistoryTombstone { .. }
            | Self::HistoryRejected { .. } => self.encode_stream_payload(enc),
            Self::Bell { .. } => field::bell::encode(self, enc),
            Self::Error { .. } => field::error::encode(self, enc),
            Self::GetMetadata { .. } | Self::DeleteMetadata { .. } => {
                field::get_metadata::encode(self, enc);
            }
            Self::SetMetadata { .. } => field::set_metadata::encode(self, enc),
            Self::ListMetadata { .. } => field::list_metadata::encode(self, enc),
            Self::SubscribeMetadata { .. } => field::subscribe_metadata::encode(self, enc),
            Self::MetadataChanged { .. } => field::metadata_changed::encode(self, enc),
            Self::MetadataValue { .. } => field::metadata_value::encode(self, enc),
            Self::MetadataKeys { .. } => field::metadata_keys::encode(self, enc),
            Self::ListDirectory { .. }
            | Self::DirectoryListing { .. }
            | Self::PathQuery { .. }
            | Self::PathResults { .. } => self.encode_metadata_payload(enc),
            Self::SpawnResource { .. }
            | Self::ResourceSpawned { .. }
            | Self::ResourceClosed { .. }
            | Self::ResizeTerminal { .. }
            | Self::Command { .. }
            | Self::Event { .. } => self.encode_resource_payload(enc),
            Self::MoveResource { .. } => field::move_terminal::encode(self, enc),
            Self::ResourceMoved { .. } => field::terminal_moved::encode(self, enc),
            Self::CommandResult { .. } => field::command_result::encode(self, enc),
            Self::SubscribeEvents { .. } => field::subscribe_events::encode(self, enc),
        }
    }
}
