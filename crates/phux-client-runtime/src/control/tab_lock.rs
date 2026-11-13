//! `ARRANGEMENT_LOCK` (behind `features_ext.TAB_LOCK`): lock or unlock one
//! tab of a session's arrangement. A locked tab refuses every close until it
//! is unlocked; the outcome arrives as [`super::Event::OrganizationOperation`].

use phux_client_core::org_ops::SubmitError;
use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::{OperationId, SessionKey};
use phux_protocol::wire::frame::Command;

use super::{ControlPlane, mint_operation};

impl ControlPlane {
    /// Whether the connected server takes [`Self::lock_group`].
    #[must_use]
    pub fn tab_lock_supported(&self) -> bool {
        self.handshake_ready
            && self
                .server
                .as_ref()
                .is_some_and(|server| server.has_ext(ServerFeatureExt::TabLock))
    }

    /// Lock (`locked`) or unlock the group `group` of `session`'s
    /// arrangement. The last writer wins.
    ///
    /// # Errors
    ///
    /// [`SubmitError::Unsupported`] unless the server advertises `TAB_LOCK`
    /// and the replica holds the session's arrangement; the journal's errors.
    pub fn lock_group(
        &mut self,
        session: SessionKey,
        group: [u8; 16],
        locked: bool,
    ) -> Result<OperationId, SubmitError> {
        let arrangement = self
            .organization
            .session(&session)
            .and_then(|record| record.arrangement)
            .filter(|_| self.tab_lock_supported())
            .ok_or(SubmitError::Unsupported)?;
        self.submit_organization(Command::ArrangementLock {
            operation: mint_operation(),
            arrangement,
            group,
            locked,
        })
    }
}
