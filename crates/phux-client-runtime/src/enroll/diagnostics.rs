//! Enrollment shares the opt-in connection capture, not its consumer task scope.
use phux_dial::DialError;
use phux_dial::enroll::{EnrollDial, EnrollPhase, enroll_observed};
use phux_protocol::enroll::Request;

use crate::connection::AttemptTrace;

pub(super) struct EnrollmentTrace(AttemptTrace);

impl EnrollmentTrace {
    pub(super) fn new() -> Self {
        let trace = Self(AttemptTrace::new(AttemptTrace::connection(), 1));
        trace.record("attempt_started");
        trace
    }

    pub(super) fn record(&self, phase: &'static str) {
        self.0.record(phase);
    }

    pub(super) fn finish(&self, succeeded: bool) {
        self.record(if succeeded {
            "enroll_succeeded"
        } else {
            "enroll_failed"
        });
    }

    pub(super) async fn exchange(
        &self,
        name: &str,
        dial: &EnrollDial,
        request: &Request,
    ) -> Result<String, String> {
        let exchanged = enroll_observed(dial, request, |phase| self.record(dial_phase(phase)));
        self.wait(name, exchanged).await
    }

    async fn wait(
        &self,
        name: &str,
        exchanged: impl std::future::Future<Output = Result<String, DialError>>,
    ) -> Result<String, String> {
        match tokio::time::timeout(crate::dial::DIAL_TIMEOUT, exchanged).await {
            Err(_) => {
                self.record("enroll_deadline");
                Err(crate::dial::timed_out(name))
            }
            Ok(Err(error)) => {
                self.record(error_phase(&error));
                Err(crate::dial::dial_message(name, &error))
            }
            Ok(Ok(chain)) => Ok(chain),
        }
    }
}

const fn dial_phase(phase: EnrollPhase) -> &'static str {
    match phase {
        EnrollPhase::Prepare => "enroll_prepare",
        EnrollPhase::Connect => "enroll_connect",
        EnrollPhase::RelayConnect => "enroll_relay_connect",
        EnrollPhase::OpenStream => "enroll_open_stream",
        EnrollPhase::Write => "enroll_write",
        EnrollPhase::Read => "enroll_read",
        EnrollPhase::Drain => "enroll_drain",
        EnrollPhase::Decode => "enroll_decode",
    }
}

const fn error_phase(error: &DialError) -> &'static str {
    match error {
        DialError::AuthRefused(_) => "enroll_refused",
        DialError::AuthorityChanged(_) => "enroll_authority_changed",
        DialError::Unreachable(_) => "enroll_unreachable",
        DialError::Stalled(_) => "enroll_stalled",
        DialError::Io(_) | DialError::Connect(_) => "enroll_exchange_failed",
    }
}

#[cfg(test)]
mod tests;
