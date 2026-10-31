//! Activation reports: an execution started, ended, or was lost.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "shares the parent's namespace"
)]
use super::*;

impl ServerState {
    /// Report that `resource`'s current activation holds a live handle.
    /// A no-op when it is not `Starting` (an already-running import).
    pub fn activation_started(&mut self, resource: ResourceId, proc: Option<ProcIdentity>) {
        let Some((key, activation)) =
            self.current_activation(resource, |s| s == ExecState::Starting)
        else {
            return;
        };
        if let Err(refusal) = self.commit_system(Op::ActivationStarted {
            resource: key,
            activation,
            proc,
        }) {
            tracing::warn!(?resource, %refusal, "activation start not recorded");
        }
    }

    /// Report that `resource`'s current activation ended with an observed
    /// `outcome`. An [`ExitPolicy::Remove`] record goes in the same commit.
    /// A no-op once nothing is current, or during shutdown.
    pub fn activation_ended(&mut self, resource: ResourceId, outcome: ExitOutcome, failed: bool) {
        if self.org.shutting_down {
            return;
        }
        let Some((key, activation)) =
            self.current_activation(resource, |s| s.is_current() || s == ExecState::Lost)
        else {
            return;
        };
        let state = if failed {
            ExecState::Failed
        } else {
            ExecState::Completed
        };
        if let Err(refusal) = self.commit_system(Op::ActivationEnded {
            resource: key,
            activation,
            state,
            outcome: Outcome::Exit(outcome),
        }) {
            tracing::warn!(?resource, %refusal, "activation end not recorded");
        }
    }

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
        self.activation_ended(resource, outcome, false);
        if self.org.terminating.remove(&resource) {
            return;
        }
        let Some(record) = self.sessions.registry.record(resource) else {
            return;
        };
        let key = record.key;
        if let Err(refusal) = self.commit_system(Op::ResourceRemove { resource: key }) {
            tracing::warn!(?resource, %refusal, "record removal refused");
        }
    }

    pub(super) fn current_activation(
        &self,
        resource: ResourceId,
        accept: impl Fn(ExecState) -> bool,
    ) -> Option<(phux_core::ResourceKey, phux_core::ActivationId)> {
        let record = self.sessions.registry.record(resource)?;
        let activation = record.activation.as_ref()?;
        accept(activation.state).then_some((record.key, activation.id))
    }

    /// Startup reconciliation (ADR-0162 §5): every activation a previous
    /// run left current or lost, and no engine in this process vouches for,
    /// becomes `Lost` when its process still runs on this boot with the
    /// recorded start time, `Failed` when launch intent committed and no
    /// process was recorded, and `Completed`/`Unobserved` when a recorded
    /// process is gone. Nothing is relaunched. Idempotent.
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
                Some((r.key, a.id, a.state, a.host_proc))
            })
            .collect();
        let mut out = Reconciled::default();
        for (key, activation, state, proc) in candidates {
            let vouched = self
                .sessions
                .registry
                .resource_id(&key)
                .is_some_and(|id| self.resource_handle(id).is_some());
            let op = match decide(state, vouched, proc, boot, &observe) {
                Verdict::Keep => continue,
                Verdict::Lost => {
                    out.lost += 1;
                    Op::ActivationLost {
                        resource: key,
                        activation,
                    }
                }
                Verdict::Unobserved => {
                    out.unobserved += 1;
                    Op::ActivationEnded {
                        resource: key,
                        activation,
                        state: ExecState::Completed,
                        outcome: Outcome::Unobserved,
                    }
                }
                Verdict::NeverLaunched => {
                    out.never_launched += 1;
                    Op::ActivationEnded {
                        resource: key,
                        activation,
                        state: ExecState::Failed,
                        outcome: Outcome::Unobserved,
                    }
                }
            };
            if let Err(refusal) = self.commit_system(op) {
                tracing::warn!(resource = %key, %refusal, "reconcile verdict not recorded");
            }
        }
        out
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
