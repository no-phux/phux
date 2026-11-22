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

/// Every frame `peer` has been written so far, without waiting.
fn written(peer: &mut tokio::io::DuplexStream, buffer: &mut BytesMut) -> Vec<FrameKind> {
    while let Some(Ok(read)) = peer.read_buf(buffer).now_or_never() {
        if read == 0 {
            break;
        }
    }
    let mut frames = Vec::new();
    while let Ok(Some(frame)) = phux_protocol::wire::framing::split_frame(buffer) {
        frames.push(phux_wire_fixtures::decode_one(&frame));
    }
    frames
}

/// What `desk::input::send_input` does with one typed line: refuse it
/// while the terminal isn't input-ready, else queue it.
fn type_line(shared: &Shared, terminal: &ResourceId, line: &str) -> bool {
    let mut control = lock(shared);
    if !control.input_ready(terminal) {
        control.trace_input(false);
        return false;
    }
    let sent = control.send_text(terminal, line);
    control.trace_input(sent);
    sent
}

/// The phone's shape (PHUX-834): no session attach, so the shown terminal
/// streams through its own `attach_terminal`. The server publishes its
/// bootstrap before it closes that command, so for a moment a frame exists
/// while input is still refused. With `prompt`, the reply comes first.
#[allow(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "one linear protocol script, read top to bottom"
)]
async fn attach_then_type(prompt: bool) -> (Vec<serde_json::Value>, u64, Vec<FrameKind>) {
    let recorded = Recorded::default();
    let _subscriber = tracing::subscriber::set_default(recorded.clone());
    let mut fixture = WriteFixture::new(64 * 1024);
    fixture.shared = Arc::new(Mutex::new(ControlPlane::new(ControlOptions {
        attach: None,
        session_recovery: crate::control::SessionRecovery::CallerValidated,
        ..ControlOptions::default()
    })));
    let outbound = Arc::clone(&fixture.signals.outbound);
    let WriteFixture {
        mut io,
        mut peer,
        shared,
        mut signals,
        close,
        ..
    } = fixture;
    let hello = phux_wire_fixtures::HelloOk::new([7; 16], phux_wire_fixtures::l3_caps()).build();
    peer.write_all(&encode(&hello)).await.unwrap();
    let wake: Wake = Arc::new(|| {});
    let connection = AttemptTrace::connection();
    let trace = AttemptTrace::new(connection, 1);
    trace.record("attempt_started");
    let terminal = ResourceId::local(1);
    let mut buffer = BytesMut::new();
    let mut sent_to_server = Vec::new();
    {
        let mut pump = Pump::new(
            "private-host-sentinel",
            &mut io,
            ConnectOptions::default(),
            &shared,
            &mut signals,
            &wake,
        );
        let running = trace.scope(pump.run());
        tokio::pin!(running);
        // Let the pump flush what was queued and read what the peer wrote.
        macro_rules! turn {
            ($running:ident) => {
                outbound.notify_one();
                for _ in 0..8 {
                    assert!($running.as_mut().now_or_never().is_none());
                }
            };
        }
        turn!(running);
        let inventory = written(&mut peer, &mut buffer)
            .into_iter()
            .find_map(|frame| match frame {
                FrameKind::Command {
                    request_id,
                    command: Command::GetState { .. },
                } => Some(request_id),
                _ => None,
            })
            .expect("HELLO leads to the inventory read");
        let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal.clone())
            .with_resources(vec![phux_protocol::wire::info::ResourceInfo::new(
                terminal.clone(),
                WindowId::new(1),
                80,
                24,
            )]);
        peer.write_all(&encode(&phux_wire_fixtures::command_value(
            inventory,
            CommandValue::State(snapshot),
        )))
        .await
        .unwrap();
        turn!(running);
        assert_eq!(lock(&shared).status(), Status::Attached);

        // The shown terminal asks for its stream (desk/shown.rs create_view).
        let attach = lock(&shared).attach_terminal(&terminal);
        assert_ne!(attach, 0);
        turn!(running);
        let asked = written(&mut peer, &mut buffer);
        assert!(asked.iter().any(|frame| matches!(frame,
            FrameKind::Command { request_id, command: Command::AttachResource { .. } }
                if *request_id == attach)));
        if prompt {
            peer.write_all(&encode(&phux_wire_fixtures::command_ok(attach)))
                .await
                .unwrap();
        }
        // The bootstrap: the frame the phone draws and reports "connected" on.
        for frame in phux_wire_fixtures::bootstrap_frames(&terminal, [1, 1], [80, 24], b"sh-3.2$ ")
        {
            peer.write_all(&encode(&frame)).await.unwrap();
        }
        turn!(running);
        let published = lock(&shared)
            .engine()
            .is_some_and(|engine| engine.input_ready(&terminal));
        assert!(published, "the replica is published: a frame exists");
        assert_eq!(lock(&shared).input_ready(&terminal), prompt);

        // The probe's one-shot `insertText(command)`, `insertText("\n")`.
        let typed = type_line(&shared, &terminal, "printf phux-qualification-marker\n");
        assert_eq!(typed, prompt, "typed input accepted only once attached");
        turn!(running);
        sent_to_server.extend(written(&mut peer, &mut buffer));

        if !prompt {
            // The reply lands late. The runtime is input-ready from here on,
            // but nothing re-sends what was refused.
            tokio::time::advance(Duration::from_secs(5)).await;
            peer.write_all(&encode(&phux_wire_fixtures::command_ok(attach)))
                .await
                .unwrap();
            turn!(running);
            assert!(lock(&shared).input_ready(&terminal));
            sent_to_server.extend(written(&mut peer, &mut buffer));
        }
        close.send(true).unwrap();
        assert!(matches!(running.await, ConnectionEnd::Closed));
    }
    let rows = recorded.0.lock().unwrap().clone();
    (rows, connection, sent_to_server)
}

fn typed_keys(frames: &[FrameKind]) -> usize {
    frames
        .iter()
        .filter(|frame| matches!(frame, FrameKind::InputKey { .. }))
        .count()
}

fn input_phases(rows: &[serde_json::Value]) -> Vec<String> {
    rows.iter()
        .filter_map(|row| row["phase"].as_str())
        .filter(|phase| phase.starts_with("input_"))
        .map(str::to_owned)
        .collect()
}

#[tokio::test(start_paused = true)]
async fn input_typed_before_the_attach_reply_is_refused_logged_and_never_resent() {
    let (rows, connection, sent) = attach_then_type(false).await;
    assert_eq!(input_phases(&rows), ["input_refused"]);
    let refused = rows
        .iter()
        .find(|row| row["phase"] == "input_refused")
        .unwrap();
    assert_eq!(refused["connection"], connection);
    assert_eq!(refused["attempt"], 1);
    assert_eq!(
        typed_keys(&sent),
        0,
        "the refused line never reaches the server"
    );
}

#[tokio::test(start_paused = true)]
async fn input_typed_after_a_prompt_attach_reply_is_injected() {
    let (rows, connection, sent) = attach_then_type(true).await;
    assert_eq!(input_phases(&rows), ["input_injected"]);
    let injected = rows
        .iter()
        .find(|row| row["phase"] == "input_injected")
        .unwrap();
    assert_eq!(injected["connection"], connection);
    assert!(typed_keys(&sent) > 0, "the line reaches the server");
}
