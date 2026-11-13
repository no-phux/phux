//! Server-held approvals (ADR-0128).

use super::{Client, ControlPlane, lock};

impl Client {
    /// Project the server's held approvals on every connection; see
    /// [`ControlPlane::watch_approvals`].
    pub fn watch_approvals(&self) {
        self.inner.with(ControlPlane::watch_approvals);
    }

    /// Re-read pending approvals on the live server; never repeats decisions.
    pub fn refresh_approvals(&self) {
        self.inner.with(ControlPlane::refresh_approvals);
    }

    /// The pending approvals, oldest first.
    #[must_use]
    pub fn approvals(&self) -> Vec<crate::approvals::Approval> {
        lock(&self.inner.control).approvals()
    }

    /// Whether the negotiated server holds actions for approval.
    #[must_use]
    pub fn approvals_supported(&self) -> bool {
        lock(&self.inner.control).approvals_supported()
    }

    /// Decide one held action; see [`ControlPlane::decide_approval`].
    #[must_use]
    pub fn decide_approval(
        &self,
        id: phux_protocol::ids::ApprovalId,
        decision: crate::approvals::Decision,
    ) -> bool {
        self.inner
            .with(|control| control.decide_approval(id, decision))
    }
}
