//! Retry executor evidence and intended retirement, never execution or teardown.
//! One entry per extant execution or dormant record bounds retained work.
//! Undurable evidence and retirement intent can still be lost on a hard crash.
use super::{
    ExecState, ExitOutcome, InstanceId, Op, Org, Outcome, ProcIdentity, Refusal, ResourceId,
    ResourceKey, ServerState,
};

#[cfg(test)]
static EXECUTOR_PERSIST_ATTEMPTS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
mod tests;

/// The execution a producer owns, captured before handing work to its engine.
/// Numeric live handles are deliberately not accepted as completion evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActivationToken {
    /// Stable organization resource identity.
    pub resource: ResourceKey,
    /// Exact execution of that resource.
    pub activation: phux_core::ActivationId,
    /// Server run that admitted the execution.
    pub instance: InstanceId,
}

impl ActivationToken {
    fn matches(self, org: &Org) -> bool {
        org.resource(&self.resource)
            .and_then(|r| r.activation.as_ref())
            .is_some_and(|a| (a.id, a.instance) == (self.activation, self.instance))
    }
}

#[derive(Debug, Clone, Copy)]
struct StartedReport {
    proc: Option<ProcIdentity>,
}

#[derive(Debug)]
pub(super) struct PendingActivation {
    resource: ResourceKey,
    execution: Option<(phux_core::ActivationId, InstanceId)>,
    start: Option<StartedReport>,
    end: Option<(Outcome, bool, Option<phux_core::LaunchDiagnostic>)>,
    lost: bool,
    retire: bool,
}

impl PendingActivation {
    fn matches(&self, org: &Org) -> bool {
        org.resource(&self.resource).is_some_and(|record| {
            match (self.execution, record.activation.as_ref()) {
                (Some((id, instance)), Some(a)) => {
                    a.id == id
                        && a.instance == instance
                        && (self.retire || a.state.is_current() || a.state == ExecState::Lost)
                }
                // A dormant record's removal is fenced to remaining dormant.
                (None, None) => self.retire,
                _ => false,
            }
        })
    }
}

impl ServerState {
    /// Capture the producer's identity before engine/effect handoff.
    #[must_use]
    pub fn activation_token(&self, resource: ResourceId) -> Option<ActivationToken> {
        let record = self.registry().record(resource)?;
        let activation = record.activation.as_ref()?;
        Some(ActivationToken {
            resource: record.key,
            activation: activation.id,
            instance: activation.instance,
        })
    }

    /// Whether a producer still names the exact recorded execution.
    #[must_use]
    pub fn activation_matches(&self, token: ActivationToken) -> bool {
        token.matches(self.org())
    }

    /// Report successful acquisition of this activation's engine/process.
    pub fn activation_started(&mut self, token: ActivationToken, proc: Option<ProcIdentity>) {
        if self.org.shutting_down || !self.activation_matches(token) {
            return;
        }
        let starting = self
            .org()
            .resource(&token.resource)
            .and_then(|r| r.activation.as_ref())
            .is_some_and(|a| a.state == ExecState::Starting);
        if !starting {
            return;
        }
        let report = self.activation_report(token);
        report.start.get_or_insert(StartedReport { proc });
        self.retry_activation_reports();
        if self.has_pending_activation_reports() {
            super::super::notify_activation_reports();
        }
    }

    /// Report an observed end, without a launch diagnostic.
    pub fn activation_ended(&mut self, token: ActivationToken, outcome: ExitOutcome, failed: bool) {
        self.activation_ended_with_diagnostic(token, outcome, failed, None);
    }

    /// Retain the first end report until durable, fenced to the exact execution.
    pub fn activation_ended_with_diagnostic(
        &mut self,
        token: ActivationToken,
        outcome: ExitOutcome,
        failed: bool,
        diagnostic: Option<phux_core::LaunchDiagnostic>,
    ) {
        if self.org.shutting_down || !self.accepts_activation_evidence(token) {
            return;
        }
        self.activation_report(token).end.get_or_insert((
            Outcome::Exit(outcome),
            failed,
            diagnostic,
        ));
        self.retry_activation_reports();
        if self.has_pending_activation_reports() {
            super::super::notify_activation_reports();
        }
    }

    /// A stop cut off a launch whose own effect reported that no process
    /// started. That is positive evidence, unlike the identity-less Starting
    /// record a crash leaves, so it alone may end `Failed`/`Unobserved` and
    /// read as `launch_interrupted` (restorable). Persisted now, while
    /// stopping; if the store refuses, the next start still reconciles the
    /// Starting record as unresolved, never as interrupted.
    pub fn activation_interrupted(&mut self, token: ActivationToken) {
        if !self.accepts_activation_evidence(token) {
            return;
        }
        self.activation_report(token)
            .end
            .get_or_insert((Outcome::Unobserved, true, None));
        self.retry_activation_reports();
    }

    fn accepts_activation_evidence(&self, token: ActivationToken) -> bool {
        self.activation_matches(token)
            && self
                .org()
                .resource(&token.resource)
                .and_then(|r| r.activation.as_ref())
                .is_some_and(|a| a.state.is_current() || a.state == ExecState::Lost)
    }

    /// Retain the intended record removal alongside its end, before any write.
    /// A failed write must not turn an already-torn-down engine into a live one.
    pub(super) fn retire_activation(&mut self, token: ActivationToken, outcome: ExitOutcome) {
        if self.org.shutting_down || !self.activation_matches(token) {
            return;
        }
        let needs_end = self.accepts_activation_evidence(token);
        let report = self.activation_report(token);
        report.retire = true;
        if needs_end {
            report
                .end
                .get_or_insert((Outcome::Exit(outcome), false, None));
        }
        self.retry_activation_reports();
        if self.has_pending_activation_reports() {
            super::super::notify_activation_reports();
        }
    }

    /// Reconciliation observations use the same retained identity-fenced ingress.
    pub(super) fn reconcile_activation(
        &mut self,
        token: ActivationToken,
        verdict: crate::reconcile::Verdict,
    ) {
        if verdict == crate::reconcile::Verdict::Keep
            || self.org.shutting_down
            || !self.accepts_activation_evidence(token)
        {
            return;
        }
        let report = self.activation_report(token);
        match verdict {
            crate::reconcile::Verdict::Keep => return,
            crate::reconcile::Verdict::Lost => report.lost = true,
            crate::reconcile::Verdict::Unobserved => {
                report.end.get_or_insert((Outcome::Unobserved, false, None));
            }
        }
        self.retry_activation_reports();
        if self.has_pending_activation_reports() {
            super::super::notify_activation_reports();
        }
    }

    pub(super) fn retire_dormant(&mut self, resource: ResourceKey) {
        if self.org.shutting_down {
            return;
        }
        self.lifecycle_report(resource, None).retire = true;
        self.retry_activation_reports();
        if self.has_pending_activation_reports() {
            super::super::notify_activation_reports();
        }
    }

    fn activation_report(&mut self, token: ActivationToken) -> &mut PendingActivation {
        self.lifecycle_report(token.resource, Some((token.activation, token.instance)))
    }

    fn lifecycle_report(
        &mut self,
        key: ResourceKey,
        execution: Option<(phux_core::ActivationId, InstanceId)>,
    ) -> &mut PendingActivation {
        let index = self
            .org
            .pending_reports
            .iter()
            .position(|r| r.resource == key && r.execution == execution);
        let index = index.unwrap_or_else(|| {
            self.org.pending_reports.push_back(PendingActivation {
                resource: key,
                execution,
                start: None,
                end: None,
                lost: false,
                retire: false,
            });
            self.org.pending_reports.len() - 1
        });
        &mut self.org.pending_reports[index]
    }

    /// Give known evidence one final persistence attempt before orderly shutdown.
    /// Shutdown is still allowed if the store is unavailable; unlike hot handoff,
    /// it must not trap an operator indefinitely in a broken server.
    pub(crate) fn flush_reports_before_shutdown(&mut self) {
        self.retry_activation_reports();
        if self.has_pending_activation_reports() {
            tracing::warn!(
                "shutdown with undurable lifecycle evidence or retirement; restart can only reconcile committed state"
            );
        }
        self.begin_shutdown();
    }

    /// Whether a handoff would lose undurable lifecycle evidence or retirement.
    #[must_use]
    pub fn has_pending_activation_reports(&self) -> bool {
        !self.org.pending_reports.is_empty()
    }

    /// Whether the shown record has undurable lifecycle evidence or retirement.
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
        if let (Some((activation, _)), Some(started)) = (report.execution, report.start) {
            self.persist_executor_op(Op::ActivationStarted {
                resource: report.resource,
                activation,
                proc: started.proc,
            })?;
            report.start = None;
        }
        // An outage in a child must not let its parent's end/removal erase
        // the child-first retirement boundary. Retry persistence, not teardown.
        if report.retire
            && self
                .org()
                .resources()
                .any(|r| r.provenance.parent == Some(report.resource))
        {
            return Err(());
        }
        if let Some((activation, _)) = report.execution {
            if report.lost {
                self.persist_executor_op(Op::ActivationLost {
                    resource: report.resource,
                    activation,
                })?;
                report.lost = false;
            }
            if let Some((outcome, failed, diagnostic)) = report.end {
                self.persist_executor_op(Op::ActivationEnded {
                    resource: report.resource,
                    activation,
                    state: if failed {
                        ExecState::Failed
                    } else {
                        ExecState::Completed
                    },
                    outcome,
                    diagnostic,
                })?;
                report.end = None;
            }
        }
        if report.retire && report.matches(self.org()) {
            self.persist_executor_op(Op::ResourceRemove {
                resource: report.resource,
            })?;
        }
        Ok(())
    }

    fn persist_executor_op(&mut self, op: Op) -> Result<(), ()> {
        #[cfg(test)]
        EXECUTOR_PERSIST_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match self.commit_system(op) {
            Ok(_) => Ok(()),
            Err(refusal @ Refusal::StoreFailure { .. }) => {
                tracing::warn!(%refusal, "lifecycle evidence or retirement awaits persistence; will retry without execution");
                Err(())
            }
            Err(refusal) => {
                tracing::warn!(%refusal, "stale lifecycle evidence or retirement discarded");
                Ok(())
            }
        }
    }

    /// Zero the test-only executor persistence counter.
    #[cfg(test)]
    pub(crate) fn reset_executor_persist_attempts() {
        EXECUTOR_PERSIST_ATTEMPTS.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// How many times executor evidence has been offered to the store in this process.
    #[cfg(test)]
    pub(crate) fn executor_persist_attempts() -> u64 {
        EXECUTOR_PERSIST_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed)
    }
}
