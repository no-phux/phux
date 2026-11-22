use super::*;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Recorded(Arc<Mutex<Vec<serde_json::Value>>>);

#[derive(Default)]
struct Fields(serde_json::Map<String, serde_json::Value>);

impl tracing::field::Visit for Fields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.0.insert(field.name().to_owned(), value.into());
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.0.insert(field.name().to_owned(), value.into());
    }
    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
}

impl tracing::Subscriber for Recorded {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.is_event() && metadata.target() == "phux_client_runtime::connection_diagnostics"
    }
    fn event(&self, event: &tracing::Event<'_>) {
        if self.enabled(event.metadata()) {
            let mut fields = Fields::default();
            event.record(&mut fields);
            self.0.lock().unwrap().push(fields.0.into());
        }
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

impl Recorded {
    fn phases(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|row| row["phase"].as_str().unwrap().to_owned())
            .collect()
    }
}

struct RefusingSigner {
    point: Vec<u8>,
}

impl super::super::DeviceSigner for RefusingSigner {
    fn public_point(&self) -> Vec<u8> {
        self.point.clone()
    }
    fn sign(&self, _: &[u8]) -> Result<Vec<u8>, String> {
        Err("private-key-error-sentinel".to_owned())
    }
}

#[test]
fn invalid_public_keys_and_refused_csr_signing_leave_distinct_last_phases() {
    use super::super::DeviceSigner as _;
    let software = super::super::SoftwareSigner::generate();
    for (point, last_phase) in [
        (vec![2; 33], "enroll_public_key"),
        (software.public_point(), "enroll_csr"),
    ] {
        let recorded = Recorded::default();
        let _guard = tracing::subscriber::set_default(recorded.clone());
        let mut target = crate::connection::Target::quic("127.0.0.1:1");
        target.authority.ca = Some("ab".repeat(32));
        let result = super::super::enroll_target_blocking(
            &target,
            &"cd".repeat(32),
            Arc::new(RefusingSigner { point }),
        );
        assert!(result.is_err());
        let phases = recorded.phases();
        assert_eq!(&phases[phases.len() - 2..], [last_phase, "enroll_failed"]);
        assert!(!phases.iter().any(|phase| phase == "enroll_prepare"));
        assert!(
            !serde_json::to_string(&*recorded.0.lock().unwrap())
                .unwrap()
                .contains("private-")
        );
    }
}

#[test]
fn blocking_enrollment_records_setup_failures_without_a_consumer_scope_or_payloads() {
    let recorded = Recorded::default();
    let _guard = tracing::subscriber::set_default(recorded.clone());
    let target = crate::connection::Target::quic("private-endpoint-sentinel");
    let error = super::super::enroll_target_blocking(
        &target,
        "private-ticket-sentinel",
        Arc::new(super::super::SoftwareSigner::generate()),
    )
    .unwrap_err();
    assert!(error.contains("ticket"));
    assert_eq!(
        recorded.phases(),
        [
            "attempt_started",
            "enroll_runtime",
            "enroll_validate",
            "enroll_failed",
        ]
    );
    let rows = recorded.0.lock().unwrap().clone();
    assert!(rows.iter().all(|row| row.as_object().unwrap().len() == 4));
    assert!(!serde_json::to_string(&rows).unwrap().contains("private-"));
}

#[tokio::test(start_paused = true)]
async fn the_existing_outer_deadline_is_typed_before_error_wording() {
    let recorded = Recorded::default();
    let _guard = tracing::subscriber::set_default(recorded.clone());
    let trace = EnrollmentTrace::new();
    trace.record("enroll_read");
    let start = tokio::time::Instant::now();
    let result = trace
        .wait("private-host-sentinel", std::future::pending())
        .await;
    assert_eq!(
        tokio::time::Instant::now() - start,
        crate::dial::DIAL_TIMEOUT
    );
    assert!(result.unwrap_err().contains("did not answer"));
    trace.finish(false);
    assert_eq!(
        recorded.phases(),
        [
            "attempt_started",
            "enroll_read",
            "enroll_deadline",
            "enroll_failed",
        ]
    );
    assert!(
        !serde_json::to_string(&*recorded.0.lock().unwrap())
            .unwrap()
            .contains("private-")
    );
}

#[tokio::test]
async fn typed_exchange_errors_remain_distinct_before_redaction() {
    let cases = [
        (
            DialError::Unreachable("private-endpoint-sentinel".into()),
            "enroll_unreachable",
        ),
        (
            DialError::Stalled("private-error-sentinel".into()),
            "enroll_stalled",
        ),
        (
            DialError::AuthRefused("private-token-sentinel".into()),
            "enroll_refused",
        ),
        (
            DialError::Connect("private-chain-sentinel".into()),
            "enroll_exchange_failed",
        ),
        (
            DialError::Io(std::io::Error::other("private-io-sentinel")),
            "enroll_exchange_failed",
        ),
        (
            DialError::AuthorityChanged(phux_dial::AuthorityChange {
                pinned: "private-pin-sentinel".into(),
                presented: None,
            }),
            "enroll_authority_changed",
        ),
    ];
    for (error, phase) in cases {
        let recorded = Recorded::default();
        let _guard = tracing::subscriber::set_default(recorded.clone());
        let trace = EnrollmentTrace::new();
        assert!(
            trace
                .wait("private-host-sentinel", std::future::ready(Err(error)))
                .await
                .is_err()
        );
        trace.finish(false);
        assert_eq!(
            recorded.phases(),
            ["attempt_started", phase, "enroll_failed"]
        );
        assert!(
            !serde_json::to_string(&*recorded.0.lock().unwrap())
                .unwrap()
                .contains("private-")
        );
    }
}
