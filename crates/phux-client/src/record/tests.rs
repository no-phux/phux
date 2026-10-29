use super::*;
use crate::testkit::{EndOfScript, ScriptSpec, ScriptedServer};
use tokio::net::UnixStream;

fn terminal() -> ResourceId {
    ResourceId::local(7)
}

fn snapshot(cols: u16, rows: u16, replay: &[u8]) -> Vec<FrameKind> {
    let stream_id = StreamId::new(1).expect("stream");
    let bootstrap_id = BootstrapId::new(2).expect("bootstrap");
    vec![
        FrameKind::BootstrapBegin {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols,
            rows,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: bytes::Bytes::copy_from_slice(replay),
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ]
}

fn output(seq: u64, bytes: &[u8]) -> FrameKind {
    FrameKind::ResourceOutput {
        terminal_id: terminal(),
        stream_id: StreamId::new(1).expect("stream"),
        bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
        seq,
        bytes: bytes::Bytes::copy_from_slice(bytes),
    }
}

fn pane_closed(exit_status: Option<i32>) -> FrameKind {
    FrameKind::Event {
        terminal: Some(terminal()),
        event: AgentEvent::ResourceClosed { exit_status },
        stamp: None,
    }
}

/// A session whose client attaches a Terminal: the testkit sends the
/// priming bootstrap *before* the `ATTACH_RESOURCE` ack, as the server
/// does, so every test exercises that interleave.
fn attached(cols: u16, rows: u16, replay: &[u8]) -> ScriptSpec {
    ScriptSpec::new().priming_snapshot(&terminal(), cols, rows, replay)
}

struct Run {
    recorded: Result<HeadlessRecording, AttachError>,
    seen: Vec<FrameKind>,
    progress: Vec<(Duration, usize)>,
}

impl Run {
    fn recording(self) -> HeadlessRecording {
        self.recorded.expect("recording")
    }
}

/// Run one scripted session on a `LocalSet` (`Connection` is `!Send`).
fn run(spec: ScriptSpec, max_duration: Option<Duration>) -> Run {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    tokio::task::LocalSet::new().block_on(&rt, async {
        let (client_stream, server_stream) = UnixStream::pair().expect("pair");
        let mut client = Connection::from_stream(client_stream);
        let server_side =
            tokio::task::spawn_local(ScriptedServer::on_stream(server_stream, spec).run());
        client
            .negotiate(recorder_client_name(), recorder_client_caps())
            .await
            .expect("recorder HELLO");
        let mut progress = Vec::new();
        let recorded = record_on_connection(&mut client, terminal(), max_duration, |at, n| {
            progress.push((at, n));
        })
        .await;
        drop(client);
        let seen = server_side.await.expect("server task");
        Run {
            recorded,
            seen,
            progress,
        }
    })
}

fn codes(recording: &HeadlessRecording) -> Vec<EventCode> {
    recording.events.iter().map(|ev| ev.code).collect()
}

fn sent_detach(seen: &[FrameKind]) -> bool {
    seen.iter().any(|frame| {
        matches!(
            frame,
            FrameKind::Command {
                command: Command::DetachResource { .. },
                ..
            }
        )
    })
}

/// ADR-0127: against a server advertising `ATTACH_ROLES` the recorder
/// declares itself a viewer; against an older one it declares nothing.
#[test]
fn recorder_attaches_as_viewer_and_still_records() {
    use phux_protocol::caps::{ServerFeature, ServerFeatureSet};
    let declared_role = |seen: &[FrameKind]| {
        seen.iter().find_map(|frame| match frame {
            FrameKind::Command {
                command: Command::AttachResource { role_policy, .. },
                ..
            } => *role_policy,
            _ => None,
        })
    };
    let roles = ServerFeatureSet::with(&[ServerFeature::AttachRoles]);
    let viewer = run(
        attached(80, 24, b"hi")
            .push(pane_closed(Some(0)))
            .server_features(roles),
        None,
    );
    assert_eq!(
        declared_role(&viewer.seen),
        Some(phux_protocol::wire::frame::RolePolicy::VIEWER)
    );
    assert_eq!(
        codes(&viewer.recording()),
        vec![EventCode::Output, EventCode::Exit]
    );

    let older = run(attached(80, 24, b"hi").push(pane_closed(Some(0))), None);
    assert_eq!(declared_role(&older.seen), None, "no bit, no byte");
}

/// The priming bootstrap arrives before the `COMMAND_RESULT` on a real
/// server. Dropping it left every capture a 0x0 grid with no opening
/// screen, so it must seed the header and land as the t=0 event.
#[test]
fn snapshot_arriving_before_the_command_result_opens_the_recording() {
    let recorded = run(
        attached(120, 40, b"opening screen")
            .push(output(1, b" more"))
            .push(pane_closed(Some(0))),
        None,
    )
    .recording();
    assert_eq!((recorded.cols, recorded.rows), (120, 40));
    let first = recorded.events.first().expect("an opening event");
    assert_eq!(
        (first.code, first.data.as_str(), first.time_ms),
        (EventCode::Output, "opening screen", 0)
    );
    assert_eq!(
        codes(&recorded),
        vec![EventCode::Output, EventCode::Output, EventCode::Exit],
    );
}

/// `HELLO` (`TrueColor`, so the server does not downsample; L1-only; raw
/// output), then `ATTACH_RESOURCE`, then `SUBSCRIBE_EVENTS` (the only way
/// an observer learns the pane exited), then `DETACH_RESOURCE` last.
#[test]
fn negotiates_as_a_recorder_then_subscribes_in_order() {
    let session = run(attached(80, 24, b"hi").push(pane_closed(Some(0))), None);
    let seen = session.seen;
    let Some(FrameKind::Hello {
        client_caps,
        client_name,
        ..
    }) = seen.first()
    else {
        panic!("first frame is HELLO, got {seen:?}");
    };
    assert_eq!(client_caps.color_support, ColorSupport::TrueColor);
    assert_eq!(client_caps.layers, phux_protocol::caps::LayerSet::new());
    assert_eq!(
        client_caps.output_mode,
        phux_protocol::caps::OutputMode::Raw
    );
    assert!(client_name.starts_with("phux-rec/"), "{client_name:?}");
    assert!(matches!(
        seen.get(1),
        Some(FrameKind::Command {
            command: Command::AttachResource { .. },
            ..
        })
    ));
    assert!(matches!(
        seen.get(2),
        Some(FrameKind::SubscribeEvents { .. })
    ));
    assert!(sent_detach(&seen[seen.len() - 1..]), "{seen:?}");
}

/// Regression guard: a session-scoped `ATTACH` (or `VIEWPORT_RESIZE`)
/// from a TTY-less recorder would shrink the live session to 80x24.
#[test]
#[allow(non_snake_case, reason = "the name is the warning")]
fn NEVER_SENDS_ATTACH_OR_VIEWPORT_RESIZE() {
    let session = run(
        attached(120, 34, b"first")
            .push(output(1, b"more"))
            .extend(snapshot(100, 30, b"repaint"))
            .push(output(2, b"tail"))
            .push(pane_closed(Some(0))),
        None,
    );
    for frame in &session.seen {
        assert!(
            !matches!(
                frame,
                FrameKind::Attach { .. } | FrameKind::ViewportResize { .. }
            ),
            "the headless recorder sent {frame:?}: that resizes the live session"
        );
    }
}

/// A later snapshot with new dims is a resize (`r` then `o`, header
/// unchanged); one with the same dims is a lag-recovery resync (`o` only).
#[test]
fn a_later_snapshot_emits_r_only_when_the_dims_change() {
    for (cols, rows, expected) in [
        (
            100,
            30,
            vec![
                EventCode::Output,
                EventCode::Resize,
                EventCode::Output,
                EventCode::Exit,
            ],
        ),
        (
            80,
            24,
            vec![EventCode::Output, EventCode::Output, EventCode::Exit],
        ),
    ] {
        let recorded = run(
            attached(80, 24, b"a")
                .extend(snapshot(cols, rows, b"b"))
                .push(pane_closed(Some(0))),
            None,
        )
        .recording();
        assert_eq!(codes(&recorded), expected, "{cols}x{rows}");
        assert_eq!((recorded.cols, recorded.rows), (80, 24));
        assert_eq!(recorded.events[expected.len() - 2].data, "b");
        if cols == 100 {
            assert_eq!(recorded.events[1].data, "100x30");
        }
    }
}

/// Arrival order wins; `seq` is a per-consumer counter, not a timebase.
#[test]
fn terminal_output_frames_become_o_events_in_arrival_order() {
    let recorded = run(
        attached(80, 24, b"")
            .push(output(9, b"one"))
            .push(output(4, b"two"))
            .push(output(7, b"three"))
            .push(pane_closed(Some(0))),
        None,
    )
    .recording();
    let data: Vec<&str> = recorded
        .events
        .iter()
        .filter(|ev| ev.code == EventCode::Output)
        .map(|ev| ev.data.as_str())
        .collect();
    assert_eq!(data, vec!["one", "two", "three"]);
}

/// `ResourceClosed` emits `x` with the status (or the unknown sentinel)
/// and stops the capture.
#[test]
fn pane_closed_emits_x_with_the_exit_status_and_stops() {
    for (status, data) in [(Some(42), "42"), (None, UNKNOWN_EXIT_STATUS)] {
        let recorded = run(
            attached(80, 24, b"")
                .push(pane_closed(status))
                .push(output(1, b"after the end")),
            None,
        )
        .recording();
        let last = recorded.events.last().expect("an exit event");
        assert_eq!((last.code, last.data.as_str()), (EventCode::Exit, data));
        assert!(!recorded.events.iter().any(|ev| ev.data == "after the end"));
    }
}

#[test]
fn error_frame_becomes_attach_error_refused() {
    let session = run(
        attached(80, 24, b"").push(FrameKind::Error {
            request_id: None,
            code: phux_protocol::wire::frame::ErrorCode::TerminalNotFound,
            message: "no such terminal".to_owned(),
        }),
        None,
    );
    match session.recorded {
        Err(AttachError::Refused(message)) => assert_eq!(message, "no such terminal"),
        other => panic!("expected Refused, got {other:?}"),
    }
}

/// A clean EOF and the duration cap are both successful stops that keep
/// what was captured; a capped capture still tears its subscription down.
#[test]
fn eof_and_the_duration_cap_finalize_successfully() {
    let eof = run(
        attached(80, 24, b"grid")
            .push(output(1, b"bytes"))
            .end(EndOfScript::HangUp),
        None,
    );
    assert_eq!(
        codes(&eof.recording()),
        vec![EventCode::Output, EventCode::Output]
    );

    let capped = run(
        attached(80, 24, b"grid").push(output(1, b"bytes")),
        Some(Duration::from_millis(120)),
    );
    assert!(sent_detach(&capped.seen));
    assert_eq!(
        codes(&capped.recording()),
        vec![EventCode::Output, EventCode::Output]
    );
}

/// A character split across frames stays one character (no U+FFFD at
/// the boundary); a tail truncated by EOF surfaces as one U+FFFD.
#[test]
fn utf8_carry_spans_frames_and_flushes_a_truncated_tail() {
    let text = |recorded: &HeadlessRecording| -> String {
        recorded
            .events
            .iter()
            .filter(|ev| ev.code == EventCode::Output)
            .map(|ev| ev.data.as_str())
            .collect()
    };
    let split = run(
        attached(80, 24, b"")
            .push(output(1, &[0xe2, 0x94]))
            .push(output(2, &[0x80]))
            .push(pane_closed(Some(0))),
        None,
    )
    .recording();
    assert_eq!(text(&split), "\u{2500}");

    let truncated = run(
        attached(80, 24, b"")
            .push(output(1, &[0xe2, 0x94]))
            .end(EndOfScript::HangUp),
        None,
    )
    .recording();
    assert_eq!(text(&truncated), "\u{fffd}");
}

/// The first progress tick fires from the priming snapshot, so a short
/// capture still reports a count (no 250 ms dead zone).
#[test]
fn progress_is_reported_at_least_once_for_a_captured_session() {
    let session = run(attached(80, 24, b"grid").push(pane_closed(Some(0))), None);
    assert!(!session.progress.is_empty());
    assert!(session.progress.iter().all(|(_, count)| *count > 0));
}
