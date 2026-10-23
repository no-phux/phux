//! Retry executor evidence, never execution. Queue size is bounded by extant
//! current activations, with at most a start and an end for each. Undurable
//! evidence can still be lost on a hard crash; the committed state is unchanged.
use super::{
    ExecState, ExitOutcome, InstanceId, Op, Org, Outcome, ProcIdentity, Refusal, ResourceId,
    ResourceKey, ServerState,
};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy)]
struct StartedReport {
    proc: Option<ProcIdentity>,
}

#[derive(Debug)]
pub(super) struct PendingActivation {
    resource: ResourceKey,
    id: phux_core::ActivationId,
    instance: InstanceId,
    start: Option<StartedReport>,
    end: Option<(ExitOutcome, bool, Option<phux_core::LaunchDiagnostic>)>,
}

impl PendingActivation {
    fn matches(&self, org: &Org) -> bool {
        org.resource(&self.resource)
            .and_then(|r| r.activation.as_ref())
            .is_some_and(|a| {
                a.id == self.id
                    && a.instance == self.instance
                    && (a.state.is_current() || a.state == ExecState::Lost)
            })
    }
}

impl ServerState {
    /// Report successful acquisition of this activation's engine/process.
    pub fn activation_started(&mut self, resource: ResourceId, proc: Option<ProcIdentity>) {
        let Some(report) = self.activation_report(resource) else {
            return;
        };
        report.start.get_or_insert(StartedReport { proc });
        self.retry_activation_reports();
    }

    /// Report an observed end, without a launch diagnostic.
    pub fn activation_ended(&mut self, resource: ResourceId, outcome: ExitOutcome, failed: bool) {
        self.activation_ended_with_diagnostic(resource, outcome, failed, None);
    }

    /// Retain the first end report until durable, fenced to the exact execution.
    pub fn activation_ended_with_diagnostic(
        &mut self,
        resource: ResourceId,
        outcome: ExitOutcome,
        failed: bool,
        diagnostic: Option<phux_core::LaunchDiagnostic>,
    ) {
        if self.org.shutting_down {
            return;
        }
        let Some(report) = self.activation_report(resource) else {
            return;
        };
        report.end.get_or_insert((outcome, failed, diagnostic));
        self.retry_activation_reports();
    }

    fn activation_report(&mut self, resource: ResourceId) -> Option<&mut PendingActivation> {
        let (key, activation) =
            self.current_activation(resource, |s| s.is_current() || s == ExecState::Lost)?;
        let instance = self.org().resource(&key)?.activation.as_ref()?.instance;
        let index = self
            .org
            .pending_reports
            .iter()
            .position(|r| r.resource == key && r.id == activation && r.instance == instance);
        let index = index.unwrap_or_else(|| {
            self.org.pending_reports.push_back(PendingActivation {
                resource: key,
                id: activation,
                instance,
                start: None,
                end: None,
            });
            self.org.pending_reports.len() - 1
        });
        self.org.pending_reports.get_mut(index)
    }

    /// Give known evidence one final persistence attempt before orderly shutdown.
    /// Shutdown is still allowed if the store is unavailable; unlike hot handoff,
    /// it must not trap an operator indefinitely in a broken server.
    pub(crate) fn flush_reports_before_shutdown(&mut self) {
        self.retry_activation_reports();
        if self.has_pending_activation_reports() {
            tracing::warn!(
                "shutdown with undurable executor evidence; restart can only reconcile committed state"
            );
        }
        self.begin_shutdown();
    }

    /// Whether a handoff would lose undurable lifecycle evidence.
    #[must_use]
    pub fn has_pending_activation_reports(&self) -> bool {
        !self.org.pending_reports.is_empty()
    }

    /// Whether the shown committed activation has undurable executor evidence.
    #[must_use]
    pub fn activation_persistence_pending(&self, key: ResourceKey) -> bool {
        self.org
            .pending_reports
            .iter()
            .any(|r| r.resource == key && r.matches(self.org()))
    }

    /// Drop stale evidence on every committed transition, including removal and
    /// relaunch. No report may attach to a replacement or resurrect a record.
    pub(super) fn prune_activation_reports(&mut self) {
        let org = self.sessions.registry.org();
        self.org.pending_reports.retain(|r| r.matches(org));
    }

    /// Bounded fair pass. A failed start stays ahead of its end; the next pass
    /// retries only the refused store write. Permanent stale reports are dropped.
    pub fn retry_activation_reports(&mut self) {
        let count = self.org.pending_reports.len().min(64);
        for _ in 0..count {
            let Some(mut report) = self.org.pending_reports.pop_front() else {
                break;
            };
            if !report.matches(self.org()) {
                continue;
            }
            if self.persist_activation_report(&mut report).is_err() {
                self.org.pending_reports.push_back(report);
            }
        }
    }

    fn persist_activation_report(&mut self, report: &mut PendingActivation) -> Result<(), ()> {
        if let Some(started) = report.start {
            let starting = self
                .org()
                .resource(&report.resource)
                .and_then(|r| r.activation.as_ref())
                .is_some_and(|a| a.state == ExecState::Starting);
            if starting {
                self.persist_executor_op(Op::ActivationStarted {
                    resource: report.resource,
                    activation: report.id,
                    proc: started.proc,
                })?;
            }
            report.start = None;
        }
        if let Some((outcome, failed, diagnostic)) = report.end {
            self.persist_executor_op(Op::ActivationEnded {
                resource: report.resource,
                activation: report.id,
                state: if failed {
                    ExecState::Failed
                } else {
                    ExecState::Completed
                },
                outcome: Outcome::Exit(outcome),
                diagnostic,
            })?;
        }
        Ok(())
    }

    fn persist_executor_op(&mut self, op: Op) -> Result<(), ()> {
        match self.commit_system(op) {
            Ok(_) => Ok(()),
            Err(refusal @ Refusal::StoreFailure { .. }) => {
                tracing::warn!(%refusal, "executor evidence awaits persistence; will retry without execution");
                Err(())
            }
            Err(refusal) => {
                tracing::warn!(%refusal, "stale executor evidence discarded");
                Ok(())
            }
        }
    }
}
