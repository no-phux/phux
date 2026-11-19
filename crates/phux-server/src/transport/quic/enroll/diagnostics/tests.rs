use super::*;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Writer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn fixed_server_events_are_rooted_and_capture_blocking_worker_boundaries() {
    let writer = Writer::default();
    let output = writer.0.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter("off,phux_server::enrollment_diagnostics=debug")
        .json()
        .without_time()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let dispatch = tracing::Dispatch::new(subscriber);
    tracing::dispatcher::with_default(&dispatch, || {
        let _sensitive = tracing::info_span!(
            target: "phux_server::enrollment_diagnostics",
            "private-context-sentinel", endpoint = "private-endpoint-sentinel"
        )
        .entered();
        let trace = Trace::new();
        trace.record(Phase::WorkQueued);
        let worker_dispatch = dispatch.clone();
        std::thread::spawn(move || {
            tracing::dispatcher::with_default(&worker_dispatch, || {
                trace.record(Phase::WorkStarted);
                trace.record(Phase::Issued);
            });
        })
        .join()
        .unwrap();
        trace.record(Phase::WorkJoined);
        trace.record(Phase::DeliveryPeerStopped);
    });
    let text = String::from_utf8(output.lock().unwrap().clone()).unwrap();
    assert!(!text.contains("private-"));
    let rows: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let phases: Vec<_> = rows
        .iter()
        .map(|row| row["fields"]["phase"].as_str().unwrap())
        .collect();
    assert_eq!(
        phases,
        [
            "WorkQueued",
            "WorkStarted",
            "Issued",
            "WorkJoined",
            "DeliveryPeerStopped"
        ]
    );
    let identity = rows[0]["fields"]["enrollment"].as_u64().unwrap();
    assert!(identity > 0);
    for row in rows {
        assert_eq!(row["fields"].as_object().unwrap().len(), 3);
        assert_eq!(row["fields"]["enrollment"], identity);
        assert!(row["fields"]["elapsed_ms"].is_u64());
    }
}

#[test]
fn normal_info_logging_emits_no_extra_enrollment_events() {
    let writer = Writer::default();
    let output = writer.0.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || Trace::new().record(Phase::WorkQueued));
    assert!(output.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn acknowledgement_peer_stop_failure_and_deadline_are_not_conflated() {
    assert_eq!(delivery_phase(&Ok(Ok(None))), Phase::DeliveryAcknowledged);
    assert_eq!(
        delivery_phase(&Ok(Ok(Some(7_u32.into())))),
        Phase::DeliveryPeerStopped
    );
    assert_eq!(delivery_phase(&Ok(Err(()))), Phase::DeliveryFailed);
    let elapsed = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        std::future::pending::<Result<Option<quinn::VarInt>, ()>>(),
    )
    .await;
    assert_eq!(delivery_phase(&elapsed), Phase::DeliveryDeadline);
}
