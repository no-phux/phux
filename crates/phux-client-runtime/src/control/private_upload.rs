//! Private images belong to one observed execution on one live connection.
//! Ordinary file transfers deliberately retain their reconnect replay policy.

use phux_protocol::ids::{
    ActivationId, AuthorityId, ResourceId, ResourceKey, ServerInstance, SessionKey,
};
use phux_protocol::wire::frame::ExecState;

use super::{ControlPlane, FrameKind, PasteEvent, PasteTrust, ServerFeature, Status};

/// An opaque, connection-bound destination for a user-selected private image.
///
/// Missing organization/execution evidence and satellite destinations refuse:
/// the hub's connection epoch cannot establish a satellite's continuity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrivateUploadDestination {
    epoch: u64,
    server: Vec<u8>,
    authority: AuthorityId,
    resource: ResourceKey,
    session: SessionKey,
    activation: ActivationId,
    instance: ServerInstance,
    pub(super) terminal: ResourceId,
}

impl ControlPlane {
    /// Capture the original identity before asking the user for private bytes.
    #[must_use]
    pub fn private_upload_destination(
        &self,
        terminal: &ResourceId,
    ) -> Option<PrivateUploadDestination> {
        if !self.private_upload_connection_verified(terminal) {
            return None;
        }
        let record = self.organization.resource_by_live(terminal)?;
        let activation = record.activation?;
        if !record.kind.is_terminal()
            || activation.state != ExecState::Running
            || activation.persistence_pending
        {
            return None;
        }
        Some(PrivateUploadDestination {
            epoch: self.connection_epoch,
            server: self
                .server
                .as_ref()
                .filter(|server| !server.id.is_empty())?
                .id
                .clone(),
            authority: self.organization.authority()?,
            resource: record.key,
            session: record.session,
            activation: activation.id,
            instance: activation.instance,
            terminal: terminal.clone(),
        })
    }

    fn private_upload_connection_verified(&self, terminal: &ResourceId) -> bool {
        terminal.host().is_none()
            && self.status == Status::Attached
            && self.accepts_inbound(self.connection_epoch)
            && self.organization.is_live()
            && self.server_has(ServerFeature::FileUpload)
    }
    /// Check under the same control lock as admission and the outbound drain.
    #[must_use]
    pub fn private_upload_destination_is_current(
        &self,
        destination: &PrivateUploadDestination,
    ) -> bool {
        self.private_upload_destination(&destination.terminal)
            .as_ref()
            == Some(destination)
    }

    /// Private-image transfers never queue offline or survive a reconnect.
    ///
    /// The destination is checked again before every chunk, its outbound
    /// drain, and its receipt.
    pub fn put_private_image(
        &mut self,
        destination: &PrivateUploadDestination,
        png: Vec<u8>,
    ) -> u64 {
        if !self.private_upload_destination_is_current(destination) {
            return self.refuse_file_upload("private image destination is no longer verified");
        }
        self.queue_file_upload(
            destination.terminal.clone(),
            "png".to_owned(),
            png,
            Some(destination.clone()),
        )
    }

    /// Type the private path once, without putting it in the reconnect input
    /// journal.
    ///
    /// As with raw paste, delivery after a lost socket is uncertain;
    /// the client must not automatically repeat this user intent.
    pub fn paste_private_image_path(
        &mut self,
        destination: &PrivateUploadDestination,
        path: &str,
    ) -> bool {
        if !self.private_upload_destination_is_current(destination)
            || !self.input_ready(&destination.terminal)
            || self.input_replay.must_order_after(&destination.terminal)
            || path.len() > super::input::MAX_RAW_PASTE
        {
            return false;
        }
        self.queue_private_frame(
            &FrameKind::InputPaste {
                terminal_id: destination.terminal.clone(),
                event: PasteEvent {
                    trust: PasteTrust::Trusted,
                    data: path.as_bytes().to_vec(),
                },
            },
            destination.clone(),
            None,
        );
        true
    }
}

#[cfg(test)]
mod tests;
