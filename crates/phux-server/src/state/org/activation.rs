//! Activation reports: an execution started, ended, or was lost.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "shares the parent's namespace"
)]
use super::*;

impl ServerState {
    /// End `resource`'s activation if it is still current, then remove its
    /// record: a reap, a kill, a purge. A no-op during shutdown, which
    /// leaves records for the next start to reconcile.
    pub(crate) fn retire_record(&mut self, resource: ResourceId, outcome: ExitOutcome) {
        if self.org.shutting_down {
            return;
        }
        // The key's removal is already durable. Writing it again would apply
        // against memory that still has the row.
        if self
            .sessions
            .registry
            .resource_key(resource)
            .is_some_and(|key| self.org.retirement_suppressed.contains(&key))
        {
            return;
        }
        let Some(token) = self.activation_token(resource) else {
            if let Some(record) = self.registry().record(resource) {
                self.retire_dormant(record.key);
            }
            return;
        };
        if self.org.terminating.remove(&resource) {
            self.activation_ended(token, outcome, false);
        } else {
            self.retire_activation(token, outcome);
        }
    }

    /// Startup reconciliation (ADR-0162 §5): every activation a previous
    /// run left current or lost, and no engine in this process vouches for,
    /// becomes `Lost` when its process still runs on this boot with the
    /// recorded start time, and `Completed`/`Unobserved` when a recorded
    /// process is gone. A launch with no durable process identity is unresolved:
    /// a crash may have lost the start report. Nothing is relaunched. Idempotent.
    pub fn reconcile(&mut self) -> Reconciled {
        self.reconcile_with(crate::reconcile::boot_id(), crate::reconcile::observe)
    }

    /// [`Self::reconcile`] against a given boot identity and OS probe.
    pub fn reconcile_with(
        &mut self,
        boot: Option<[u8; 16]>,
        observe: impl Fn(i32) -> crate::reconcile::Observed,
    ) -> Reconciled {
        use crate::reconcile::{Verdict, decide};
        let candidates: Vec<_> = self
            .org()
            .resources()
            .filter_map(|r| {
                let a = r.activation.as_ref()?;
                Some((r.key, a.id, a.instance, r.kind, a.state, a.host_proc))
            })
            .collect();
        let mut out = Reconciled::default();
        for (key, activation, instance, kind, state, proc) in candidates {
            let vouched = self
                .sessions
                .registry
                .resource_id(&key)
                .is_some_and(|id| self.resources().handle(id).is_some());
            // A recorded running in-process projection cannot outlive its
            // server. Terminal processes can, even if identity capture failed.
            let verdict = if !vouched
                && kind != phux_core::ResourceKind::Terminal
                && proc.is_none()
                && matches!(state, ExecState::Running | ExecState::Stopping)
            {
                Verdict::Unobserved
            } else {
                decide(state, vouched, proc, boot, &observe)
            };
            match verdict {
                Verdict::Keep => continue,
                Verdict::Lost => {
                    if proc.is_some() {
                        out.lost += 1;
                    } else {
                        out.unresolved += 1;
                    }
                }
                Verdict::Unobserved => {
                    out.unobserved += 1;
                }
            }
            self.reconcile_activation(
                ActivationToken {
                    resource: key,
                    activation,
                    instance,
                },
                verdict,
            );
        }
        out
    }

    /// Cold start: each Terminal record gets back the `@N` its pane last had
    /// when it next launches, so ids held by scripts and agents keep naming
    /// the same pane across a restart and `phux restore`.
    pub fn reserve_pane_ids(&mut self) {
        let saved: Vec<(ResourceId, u32)> = self
            .org()
            .resources()
            .filter_map(|r| match r.launch.as_ref()? {
                LaunchSpec::Terminal(launch) => {
                    Some((self.sessions.registry.resource_id(&r.key)?, launch.pane_id?))
                }
                _ => None,
            })
            .collect();
        self.idspace.reserve_terminals(saved);
    }

    /// Whether any activation is current: the server still executes work
    /// (R13).
    #[must_use]
    pub fn has_live_activation(&self) -> bool {
        self.org()
            .resources()
            .any(|r| r.activation.as_ref().is_some_and(|a| a.state.is_current()))
    }
}
