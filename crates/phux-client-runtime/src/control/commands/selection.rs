//! Lookup-only attachment and durable-key recovery; creation is a separate intent.

use super::super::{AttachTarget, ControlError, ControlPlane, FrameKind, Status};

pub(super) const CREATION_IS_NOT_ATTACH: &str =
    "attach does not create a session; submit create_session or ensure_session and select its exact committed key";

impl ControlPlane {
    /// Queue only an attach intent; a legacy create target is not a hidden ensure.
    pub(in crate::control) fn queue_session_attach(&mut self, target: AttachTarget) {
        if matches!(target, AttachTarget::CreateIfMissing { .. }) {
            self.attach_target = None;
            self.error = Some(CREATION_IS_NOT_ATTACH.into());
            self.set_status(Status::Failed);
            return;
        }
        let attach_id = self.next_request_id();
        self.active_attach_id = Some(attach_id);
        self.viewport.pending = false;
        self.queue_frame(&FrameKind::Attach {
            attach_id, target, viewport: self.viewport.desired,
            request_scrollback: true, scrollback_limit_lines: self.options.scrollback_lines,
            role_policy: self.options.attach_role,
        });
    }

    /// A restarted peer may reuse numeric ids and names. Recover only the exact
    /// durable key in the same authority, after that connection seeds its replica.
    pub(in crate::control) fn requalify_attach_target(&mut self, server_id: &[u8]) -> Result<(), ControlError> {
        if !self.options.automatic_lifecycle {
            return Ok(());
        }
        if matches!(self.attach_target, Some(AttachTarget::CreateIfMissing { .. })) {
            return Err(ControlError::Refused(CREATION_IS_NOT_ATTACH.into()));
        }
        let Some(AttachTarget::ById(id)) = &self.attach_target else { return Ok(()); };
        if self.server.as_ref().is_none_or(|previous| previous.id == server_id) {
            return Ok(());
        }
        let key = self.topology.as_ref().and_then(|topology| topology.session(id.get()))
            .and_then(|session| session.key)
            .or_else(|| self.organization.session_by_live(*id).map(|session| session.key));
        let Some((authority, key)) = self.organization.authority().zip(key) else {
            return Err(ControlError::Refused("cannot recover the previous session without its durable authority and key".into()));
        };
        self.org.pending_attach = Some((authority, key));
        self.attach_target = None;
        Ok(())
    }

    pub(in crate::control) fn resume_keyed_attach(&mut self) {
        if !self.handshake_ready || !self.options.automatic_lifecycle || !self.org.ready || self.status.is_terminal() {
            return;
        }
        let Some((authority, key)) = self.org.pending_attach.take() else { return; };
        let id = (self.organization.authority() == Some(authority))
            .then(|| self.organization.session(&key).and_then(|session| session.live)).flatten();
        let Some(id) = id else {
            self.attach_target = None;
            self.error = Some(format!("cannot recover session {key}: its exact authority/key is unavailable"));
            self.set_status(Status::Failed);
            return;
        };
        let target = AttachTarget::ById(id);
        self.attach_target = Some(target.clone());
        self.queue_session_attach(target);
    }
}

#[cfg(test)]
#[path = "selection_tests.rs"]
mod tests;
