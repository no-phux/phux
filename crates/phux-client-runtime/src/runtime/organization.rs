//! The organization surface of a runtime client: the replica and the typed
//! organization commands, serialized by the control owner. Each call
//! delegates to the matching [`crate::control::ControlPlane`] method;
//! outcomes arrive as [`crate::control::Event::OrganizationOperation`].

use phux_client_core::layout::ArrangementDoc;
use phux_client_core::org_ops::SubmitError;
use phux_protocol::ids::{ArrangementKey, OperationId, ResourceKey, SessionKey};
use phux_protocol::wire::frame::{Command, Committed, SessionLifetime, TerminateMode};

use super::Client;
use crate::control::{
    ArrangementSubmitError, ControlPlane, InboxView, IntakeView, OrganizationReplica,
};

impl Client {
    /// A copy of the organization replica.
    #[must_use]
    pub fn organization(&self) -> OrganizationReplica {
        self.inner.with(|control| control.organization().clone())
    }

    /// Legacy alias for [`Self::intake`].
    #[must_use]
    pub fn inbox(&self) -> InboxView {
        self.intake()
    }

    /// The owner-local Intake, distinct from attention notifications.
    #[must_use]
    pub fn intake(&self) -> IntakeView {
        self.inner
            .with(|control| control.organization().intake_view())
    }

    /// Reseed the replica; `None` without `ORGANIZATION`.
    #[must_use]
    pub fn refresh_organization(&self) -> Option<u32> {
        self.inner.with(ControlPlane::refresh_organization)
    }

    /// Read one arrangement into the replica; `None` without
    /// `ORGANIZATION`.
    #[must_use]
    pub fn fetch_arrangement(&self, arrangement: ArrangementKey) -> Option<u32> {
        self.inner
            .with(|control| control.fetch_arrangement(arrangement))
    }

    /// Submit any mutating organization command; see
    /// [`ControlPlane::submit_organization`].
    ///
    /// # Errors
    ///
    /// [`SubmitError`].
    pub fn submit_organization(&self, command: Command) -> Result<OperationId, SubmitError> {
        self.inner
            .with(|control| control.submit_organization(command))
    }

    /// Kill a whole session server-side; see [`ControlPlane::kill_session`].
    ///
    /// # Errors
    ///
    /// [`SubmitError`].
    pub fn kill_session(&self, session: SessionKey) -> Result<OperationId, SubmitError> {
        self.inner.with(|control| control.kill_session(session))
    }

    /// Rename a session.
    ///
    /// # Errors
    ///
    /// [`SubmitError`].
    pub fn rename_session(
        &self,
        session: SessionKey,
        name: &str,
    ) -> Result<OperationId, SubmitError> {
        self.inner
            .with(|control| control.rename_session(session, name))
    }

    /// Link a session into a named project, or clear its link; see
    /// [`ControlPlane::set_session_project`].
    ///
    /// # Errors
    ///
    /// [`SubmitError`].
    pub fn set_session_project(
        &self,
        session: SessionKey,
        project: Option<&str>,
    ) -> Result<OperationId, SubmitError> {
        self.inner
            .with(|control| control.set_session_project(session, project))
    }

    /// Make a session persistent or ephemeral.
    ///
    /// # Errors
    ///
    /// [`SubmitError`].
    pub fn set_session_lifetime(
        &self,
        session: SessionKey,
        lifetime: SessionLifetime,
    ) -> Result<OperationId, SubmitError> {
        self.inner
            .with(|control| control.set_session_lifetime(session, lifetime))
    }

    /// Delete an empty session.
    ///
    /// # Errors
    ///
    /// [`SubmitError`].
    pub fn delete_session(&self, session: SessionKey) -> Result<OperationId, SubmitError> {
        self.inner.with(|control| control.delete_session(session))
    }

    /// Legacy alias for [`Self::intake_resolve`].
    ///
    /// # Errors
    ///
    /// [`SubmitError`].
    pub fn inbox_resolve(&self, session: SessionKey) -> Result<OperationId, SubmitError> {
        self.intake_resolve(session)
    }

    /// Clear a session's Intake mark; members and execution remain unchanged.
    ///
    /// # Errors
    ///
    /// [`SubmitError`].
    pub fn intake_resolve(&self, session: SessionKey) -> Result<OperationId, SubmitError> {
        self.inner.with(|control| control.intake_resolve(session))
    }

    /// Move one resource into `to`.
    ///
    /// # Errors
    ///
    /// [`SubmitError`].
    pub fn move_resource(
        &self,
        resource: ResourceKey,
        to: SessionKey,
    ) -> Result<OperationId, SubmitError> {
        self.inner
            .with(|control| control.move_resource(resource, to))
    }

    /// Move many resources into `to`, one atomic batch per chunk.
    ///
    /// # Errors
    ///
    /// [`SubmitError`].
    pub fn move_resources(
        &self,
        resources: &[ResourceKey],
        to: SessionKey,
    ) -> Result<Vec<OperationId>, SubmitError> {
        self.inner
            .with(|control| control.move_resources(resources, to))
    }

    /// Undo a committed organization edit by its receipt.
    ///
    /// # Errors
    ///
    /// [`SubmitError`].
    pub fn undo(&self, receipt: &Committed) -> Result<OperationId, SubmitError> {
        self.inner.with(|control| control.undo(receipt))
    }

    /// Remove an inactive resource.
    ///
    /// # Errors
    ///
    /// [`SubmitError`].
    pub fn remove_resource(&self, resource: ResourceKey) -> Result<OperationId, SubmitError> {
        self.inner.with(|control| control.remove_resource(resource))
    }

    /// Relaunch an inactive resource.
    ///
    /// # Errors
    ///
    /// [`SubmitError`].
    pub fn relaunch_resource(&self, resource: ResourceKey) -> Result<OperationId, SubmitError> {
        self.inner
            .with(|control| control.relaunch_resource(resource))
    }

    /// Terminate a resource's current activation.
    ///
    /// # Errors
    ///
    /// [`SubmitError`].
    pub fn terminate_resource(
        &self,
        resource: ResourceKey,
        mode: TerminateMode,
    ) -> Result<OperationId, SubmitError> {
        self.inner
            .with(|control| control.terminate_resource(resource, mode))
    }

    /// Replace a session's arrangement with a handle-space document.
    ///
    /// # Errors
    ///
    /// [`ArrangementSubmitError`].
    pub fn replace_arrangement(
        &self,
        session: SessionKey,
        document: &ArrangementDoc,
    ) -> Result<OperationId, ArrangementSubmitError> {
        self.inner
            .with(|control| control.replace_arrangement(session, document))
    }
}
