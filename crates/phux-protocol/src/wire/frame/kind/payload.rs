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
            | Self::Attach { .. }
            | Self::Detach
            | Self::Detached { .. }
            | Self::InputKey { .. }
            | Self::InputMouse { .. }
            | Self::InputFocus { .. }
            | Self::InputPaste { .. }
            | Self::InputTerminalReply { .. }
            | Self::ClipboardReply { .. }
            | Self::FrameAck { .. }
            | Self::ViewportResize { .. }
            | Self::Attached { .. }
            | Self::AttachReady { .. } => self.encode_connection_payload(enc),
            Self::BootstrapBegin { .. }
            | Self::BootstrapChunk { .. }
            | Self::BootstrapReady { .. }
            | Self::HistoryRequest { .. }
            | Self::HistoryPage { .. }
            | Self::BootstrapTombstone { .. }
            | Self::HistoryTombstone { .. }
            | Self::HistoryRejected { .. }
            | Self::Bell { .. }
            | Self::Error { .. } => self.encode_stream_payload(enc),
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
            | Self::MoveResource { .. }
            | Self::ResourceMoved { .. }
            | Self::ResourceClosed { .. }
            | Self::ResizeTerminal { .. }
            | Self::Command { .. }
            | Self::CommandResult { .. }
            | Self::SubscribeEvents { .. }
            | Self::Event { .. } => self.encode_resource_payload(enc),
        }
    }
}
