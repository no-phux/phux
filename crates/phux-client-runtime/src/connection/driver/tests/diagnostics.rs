//! The shipped initial-budget gate and pump, with an in-memory framed peer.
use super::*;
use phux_protocol::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::wire::frame::{Command, CommandValue};
use phux_protocol::wire::info::SessionSnapshot;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

fn inventory_request(peer: &mut tokio::io::DuplexStream) -> u32 {
    let mut bytes = BytesMut::new();
    while let Some(Ok(read)) = peer.read_buf(&mut bytes).now_or_never() {
        if read == 0 {
            break;
        }
    }
    while let Ok(Some(frame)) = phux_protocol::wire::framing::split_frame(&mut bytes) {
        if let FrameKind::Command {
            request_id,
            command: Command::GetState { .. },
        } = phux_wire_fixtures::decode_one(&frame)
        {
            return request_id;
        }
    }
    panic!("HELLO must lead to the inventory read");
}

async fn inventory_attempt(connection: u64, answer: bool) -> Shared {
    let WriteFixture {
        mut io,
        mut peer,
        shared,
        mut signals,
        close,
        ..
    } = WriteFixture::new(64 * 1024);
    let hello = phux_wire_fixtures::HelloOk::new([7; 16], phux_wire_fixtures::l3_caps()).build();
    peer.write_all(&encode(&hello)).await.unwrap();
    let wake: Wake = Arc::new(|| {});
    let options = ConnectOptions::default();
    let deadline = Instant::now() + options.initial_budget;
    let trace = AttemptTrace::new(connection, 1);
    trace.record("attempt_started");
    {
        let connection = async {
            let started = std::time::Instant::now();
            let end = Pump::new(
                "private-host-sentinel",
                &mut io,
                options,
                &shared,
                &mut signals,
                &wake,
            )
            .run()
            .await;
            attempt(end, lock(&shared).status() == Status::Attached, started)
        };
        let running = trace.scope(within_initial_budget(connection, &shared, deadline));
        tokio::pin!(running);
        assert!(running.as_mut().now_or_never().is_none());
        assert_eq!(lock(&shared).status(), Status::Negotiated);
        let request_id = inventory_request(&mut peer);
        if answer {
            let snapshot =
                SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1));
            let frame =
                phux_wire_fixtures::command_value(request_id, CommandValue::State(snapshot));
            peer.write_all(&encode(&frame)).await.unwrap();
            assert!(running.as_mut().now_or_never().is_none());
            assert!(lock(&shared).attached_once());
            // The original 18s budget is moot after the inventory becomes usable.
            tokio::time::advance(options.initial_budget).await;
            assert!(running.as_mut().now_or_never().is_none());
            close.send(true).unwrap();
            assert!(matches!(running.await.unwrap().end, ConnectionEnd::Closed));
        } else {
            tokio::time::advance(options.initial_budget).await;
            assert!(running.await.is_none());
            fail(&shared, None, &wake);
        }
    }
    shared
}

#[tokio::test(start_paused = true)]
async fn delayed_inventory_expires_and_a_replacement_cannot_erase_the_failure() {
    let recorded = Recorded::default();
    let _subscriber = tracing::subscriber::set_default(recorded.clone());
    let first_id = AttemptTrace::connection();
    let first = inventory_attempt(first_id, false).await;
    let error = lock(&first).last_error().unwrap().to_owned();
    assert_eq!(error, "connect: no answer within the initial budget");
    assert_eq!(lock(&first).status(), Status::Failed);

    let replacement_id = AttemptTrace::connection();
    assert_ne!(first_id, replacement_id);
    let replacement = inventory_attempt(replacement_id, true).await;
    assert!(lock(&replacement).attached_once());
    assert_eq!(lock(&first).status(), Status::Failed);
    assert_eq!(lock(&first).last_error(), Some(error.as_str()));
    let rows = recorded.0.lock().unwrap().clone();
    let deadline = rows
        .iter()
        .find(|row| row["phase"] == "initial_deadline")
        .unwrap();
    assert_eq!(deadline["connection"], first_id);
    assert_eq!(deadline["attempt"], 1);
    assert_eq!(deadline["elapsed_ms"], 18_000);
    for phase in [
        "hello_sent",
        "hello_received",
        "hello_accepted",
        "topology_received",
        "topology_applied",
    ] {
        assert!(
            rows.iter().any(|row| row["phase"] == phase),
            "missing {phase}"
        );
    }
    assert!(
        !rows
            .iter()
            .any(|row| row.to_string().contains("private-host-sentinel"))
    );
    if let Some(directory) = std::env::var_os("RUNE_TRACE_TEST_EVIDENCE") {
        let mut bytes = Vec::new();
        for row in rows {
            serde_json::to_writer(&mut bytes, &row).unwrap();
            bytes.push(b'\n');
        }
        std::fs::write(
            std::path::PathBuf::from(directory).join("runtime-phases.jsonl"),
            bytes,
        )
        .unwrap();
    }
}
