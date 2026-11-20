//! Exercise the shared producer without a socket, actor, or payload logging.
#![allow(
    clippy::wildcard_imports,
    reason = "the attach test topics share the parent namespace"
)]
use super::*;
use phux_terminal_api::actor::TerminalResource as _;
use std::sync::{Arc, Mutex};

pub(super) type Captured = Arc<Mutex<Vec<u8>>>;

struct Writer(Captured);

impl std::io::Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn capture() -> (Captured, tracing::subscriber::DefaultGuard) {
    let bytes = Captured::default();
    let writer = bytes.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter("off,phux_server::publication_diagnostics=debug")
        .with_ansi(false)
        .without_time()
        .json()
        .with_writer(move || Writer(writer.clone()))
        .finish();
    (bytes, tracing::subscriber::set_default(subscriber))
}

pub(super) fn assert_event(bytes: &Captured, phase: &str, outcome: &str) {
    let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    let rows: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(!text.contains("private-context-sentinel"));
    assert!(!text.contains("private-address-sentinel"));
    assert!(
        rows.iter()
            .any(|row| row["fields"]["phase"] == phase && row["fields"]["outcome"] == outcome),
        "missing {phase}/{outcome}: {text}"
    );
    for row in &rows {
        let fields = row["fields"].as_object().unwrap();
        assert_eq!(
            fields.len(),
            2,
            "no payload, identity, or raw errors in diagnostic events"
        );
        assert!(fields.values().all(serde_json::Value::is_string));
    }
}

#[tokio::test(start_paused = true)]
async fn activation_refusal_and_actor_loss_are_not_capture_deadlines() {
    let (bytes, _guard) = capture();
    let _context = tracing::info_span!(
        target: "phux_server::publication_diagnostics",
        "private-context-sentinel",
        endpoint = "private-address-sentinel"
    )
    .entered();
    let (handle, _, _, mut requests) = native_attach_handle();
    let terminal = handle.terminal().unwrap();
    let activate = activate_native_publication(
        terminal,
        7,
        phux_protocol::ids::ResourceId::local(1),
        two_pump_stream(),
        two_pump_initial_generation(),
        [9; 32],
    );
    tokio::pin!(activate);
    let request = tokio::select! {
        result = &mut activate => panic!("unexpected completion: {result:?}"),
        request = requests.recv() => request.unwrap(),
    };
    // No new activation timer: a held reply remains pending past 18 seconds.
    tokio::time::advance(std::time::Duration::from_secs(18)).await;
    assert!(futures_util::poll!(&mut activate).is_pending());
    request
        .reply
        .send(Err(
            phux_terminal_api::native_state::NativeStateError::InvalidHandle,
        ))
        .unwrap();
    assert!(matches!(
        activate.await,
        Err(NativeRequestFailure::Refused(_))
    ));
    assert_event(&bytes, "native_activation", "refused");
    drop(requests);
    let result = activate_native_publication(
        terminal,
        7,
        phux_protocol::ids::ResourceId::local(1),
        two_pump_stream(),
        two_pump_initial_generation(),
        [9; 32],
    )
    .await;
    assert!(matches!(result, Err(NativeRequestFailure::Unsent)));
    assert_event(&bytes, "native_activation", "actor_unsent");
    assert!(
        !std::str::from_utf8(&bytes.lock().unwrap())
            .unwrap()
            .contains("deadline")
    );
}
