#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]

use super::*;
use crate::testkit::{ScriptSpec, serve_one};
use phux_protocol::caps::ServerFeatureSet;
use phux_protocol::ids::ResourceKind;

fn session() -> ResourceId {
    ResourceId::local(9)
}

fn parent() -> ResourceId {
    ResourceId::local(7)
}

fn kinds() -> ServerFeatureSet {
    ServerFeatureSet::with(&[ServerFeature::ResourceKinds])
}

fn serve(
    spec: ScriptSpec,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    tokio::task::JoinHandle<Vec<FrameKind>>,
) {
    let dir = tempfile::tempdir().unwrap();
    let (socket, server) = serve_one(dir.path(), spec);
    (dir, socket, server)
}

#[test]
fn emit_records_are_validated_before_any_byte_is_sent() {
    assert!(EmitRecord::new("prompt", serde_json::json!({ "chars": 4 })).is_ok());
    assert!(matches!(
        EmitRecord::new("frobnicate", serde_json::json!({})),
        Err(AgentSessionError::RecordInvalid(_))
    ));
    assert!(matches!(
        EmitRecord::new("stop", serde_json::json!("text")),
        Err(AgentSessionError::RecordInvalid(_))
    ));
    let huge = serde_json::json!({ "blob": "x".repeat(MAX_RECORD_BYTES) });
    assert!(matches!(
        EmitRecord::new("provider_raw", huge),
        Err(AgentSessionError::RecordInvalid(_))
    ));
    let line = EmitRecord::new("stop", serde_json::json!({}))
        .unwrap()
        .encode_line();
    let object: serde_json::Value = serde_json::from_slice(&line[..line.len() - 1]).unwrap();
    assert_eq!(object, serde_json::json!({ "type": "stop", "data": {} }));
    assert_eq!(line.last(), Some(&b'\n'));
}

#[tokio::test]
async fn every_verb_is_gated_on_the_resource_kinds_bit() {
    // The default scripted server advertises no features.
    let (_dir, socket, server) = serve(ScriptSpec::new());
    let mut conn = Connection::connect(&socket).await.unwrap();
    assert!(matches!(
        open(&mut conn, &parent(), "claude", None).await,
        Err(AgentSessionError::Unsupported)
    ));
    assert!(matches!(
        close(&mut conn, &session()).await,
        Err(AgentSessionError::Unsupported)
    ));
    let record = EmitRecord::new("stop", serde_json::json!({})).unwrap();
    assert!(matches!(
        emit(&mut conn, &session(), &[record]).await,
        Err(AgentSessionError::Unsupported)
    ));
    assert!(matches!(
        log(&mut conn, &session(), LogOptions::default(), |_| true).await,
        Err(AgentSessionError::Unsupported)
    ));
    drop(conn);
    let seen = server.await.unwrap();
    assert_eq!(
        seen.len(),
        1,
        "only HELLO may reach an unsupporting server: {seen:?}"
    );
}

#[tokio::test]
async fn open_sends_the_kinded_spawn_and_returns_the_new_resource() {
    let spec = ScriptSpec::new()
        .server_features(kinds())
        .spawn_result(SpawnResult::Ok(session()));
    let (_dir, socket, server) = serve(spec);
    let mut conn = Connection::connect(&socket).await.unwrap();
    let opened = open(&mut conn, &parent(), "claude", Some("sess-1"))
        .await
        .unwrap();
    assert_eq!(opened.resource, session());
    assert_eq!(opened.parent, parent());
    drop(conn);
    let seen = server.await.unwrap();
    let Some(FrameKind::SpawnResource {
        resource: Some(resource),
        command,
        ..
    }) = seen.get(1)
    else {
        panic!("SPAWN_RESOURCE with a resource body must follow HELLO, got {seen:?}");
    };
    assert_eq!(resource.kind, ResourceKind::AgentSession);
    assert_eq!(resource.parent, Some(parent()));
    assert_eq!(resource.provider.as_deref(), Some("claude"));
    assert_eq!(resource.native_id.as_deref(), Some("sess-1"));
    assert!(command.is_none(), "an agent session spawns no process");
}

#[tokio::test]
async fn open_maps_the_structured_spawn_refusals() {
    type Check = fn(&AgentSessionError) -> bool;
    let cases: [(SpawnError, Check); 2] = [
        (SpawnError::ParentNotFound, |err| {
            matches!(err, AgentSessionError::ParentNotFound { .. })
        }),
        (SpawnError::ParentKindMismatch, |err| {
            matches!(err, AgentSessionError::ParentKindMismatch { .. })
        }),
    ];
    for (error, check) in cases {
        let spec = ScriptSpec::new()
            .server_features(kinds())
            .spawn_result(SpawnResult::Err(error));
        let (_dir, socket, server) = serve(spec);
        let mut conn = Connection::connect(&socket).await.unwrap();
        let err = open(&mut conn, &parent(), "claude", None)
            .await
            .unwrap_err();
        assert!(check(&err), "{err}");
        drop(conn);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn emit_appends_one_jsonl_line_per_record_and_reads_the_stamped_header() {
    let spec = ScriptSpec::new()
        .server_features(kinds())
        .append_result(CommandResult::OkWith(CommandValue::Json(
            r#"{"seq":42,"ts_ms":1757404800123}"#.to_owned(),
        )));
    let (_dir, socket, server) = serve(spec);
    let mut conn = Connection::connect(&socket).await.unwrap();
    let records = [
        EmitRecord::new("prompt", serde_json::json!({ "chars": 4 })).unwrap(),
        EmitRecord::new("stop", serde_json::json!({})).unwrap(),
    ];
    let emitted = emit(&mut conn, &session(), &records).await.unwrap();
    assert_eq!(emitted.seq, Some(42));
    assert_eq!(emitted.ts_ms, Some(1_757_404_800_123));
    drop(conn);
    let seen = server.await.unwrap();
    let Some(FrameKind::Command {
        command: Command::AppendResourceOutput { terminal_id, bytes },
        ..
    }) = seen.get(1)
    else {
        panic!("APPEND_RESOURCE_OUTPUT must follow HELLO, got {seen:?}");
    };
    assert_eq!(*terminal_id, session());
    let text = std::str::from_utf8(bytes).unwrap();
    assert_eq!(text.lines().count(), 2);
    assert!(text.ends_with('\n'));
}

#[tokio::test]
async fn emit_maps_every_server_refusal_onto_its_own_variant() {
    type Check = fn(&AgentSessionError) -> bool;
    let cases: [(ErrorCode, Check); 4] = [
        (ErrorCode::WrongResourceKind, |err| {
            matches!(err, AgentSessionError::WrongKind { .. })
        }),
        (ErrorCode::NotProducer, |err| {
            matches!(err, AgentSessionError::NotProducer { .. })
        }),
        (ErrorCode::RecordInvalid, |err| {
            matches!(err, AgentSessionError::RecordInvalid(_))
        }),
        (ErrorCode::Overflow, |err| {
            matches!(err, AgentSessionError::Overflow(_))
        }),
    ];
    for (code, check) in cases {
        let spec = ScriptSpec::new()
            .server_features(kinds())
            .append_result(CommandResult::Error {
                code,
                message: "nope".to_owned(),
            });
        let (_dir, socket, server) = serve(spec);
        let mut conn = Connection::connect(&socket).await.unwrap();
        let record = EmitRecord::new("stop", serde_json::json!({})).unwrap();
        let err = emit(&mut conn, &session(), &[record]).await.unwrap_err();
        assert!(check(&err), "{code:?} mapped to {err:?}");
        drop(conn);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn log_delivers_the_retained_records_then_detaches() {
    let lines = [
        r#"{"seq":1,"ts_ms":10,"type":"session_start","data":{}}"#,
        r#"{"seq":2,"ts_ms":20,"type":"prompt","data":{"chars":4}}"#,
        r#"{"seq":3,"ts_ms":30,"type":"stop","data":{}}"#,
    ];
    let spec = ScriptSpec::new()
        .server_features(kinds())
        .agent_log_bootstrap(&session(), &lines);
    let (_dir, socket, server) = serve(spec);
    let mut conn = Connection::connect(&socket).await.unwrap();
    let mut got = Vec::new();
    let outcome = log(
        &mut conn,
        &session(),
        LogOptions {
            follow: false,
            tail: Some(2),
        },
        |record| {
            got.push(record);
            true
        },
    )
    .await
    .unwrap();
    assert_eq!(outcome.retained, 3);
    assert_eq!(outcome.delivered, 2, "--tail 2 keeps the last two");
    assert_eq!(outcome.end, LogEnd::Retained);
    assert_eq!(got[0].seq, 2);
    assert_eq!(got[0].kind, "prompt");
    assert_eq!(got[0].data["chars"], 4);
    assert_eq!(got[1].seq, 3);
    drop(conn);
    let seen = server.await.unwrap();
    assert!(
        matches!(
            seen.get(1),
            Some(FrameKind::Command {
                command: Command::AttachResource { terminal_id, .. },
                ..
            }) if *terminal_id == session()
        ),
        "log attaches the session resource itself: {seen:?}"
    );
    assert!(
        seen.iter().any(|frame| matches!(
            frame,
            FrameKind::Command {
                command: Command::DetachResource { .. },
                ..
            }
        )),
        "a non-following read detaches: {seen:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|frame| matches!(frame, FrameKind::SubscribeEvents { .. })),
        "a non-following read never subscribes to events"
    );
}

#[tokio::test]
async fn log_follow_streams_live_records_until_the_session_closes() {
    let spec = ScriptSpec::new()
        .server_features(kinds())
        .agent_log_bootstrap(
            &session(),
            &[r#"{"seq":1,"ts_ms":10,"type":"session_start","data":{}}"#],
        )
        .push(FrameKind::ResourceOutput {
            terminal_id: session(),
            stream_id: phux_protocol::ids::StreamId::new(1).expect("stream"),
            bootstrap_id: phux_protocol::ids::BootstrapId::new(1).expect("bootstrap"),
            seq: 1,
            bytes: bytes::Bytes::from_static(
                b"{\"seq\":2,\"ts_ms\":20,\"type\":\"prompt\",\"data\":{}}\n{\"seq\":3,\"ts_ms\":30,\"type\":\"stop\",\"data\":{}}\n",
            ),
        })
        .push(FrameKind::Event {
            terminal: Some(session()),
            event: AgentEvent::ResourceClosed { exit_status: None }, stamp: None,
        });
    let (_dir, socket, server) = serve(spec);
    let mut conn = Connection::connect(&socket).await.unwrap();
    let mut seqs = Vec::new();
    let outcome = log(
        &mut conn,
        &session(),
        LogOptions {
            follow: true,
            tail: None,
        },
        |record| {
            seqs.push(record.seq);
            true
        },
    )
    .await
    .unwrap();
    assert_eq!(seqs, [1, 2, 3]);
    assert_eq!(outcome.end, LogEnd::SessionClosed);
    drop(conn);
    server.await.unwrap();
}

#[test]
fn the_reader_reassembles_a_record_split_across_two_chunks() {
    let mut reader = Reader::new(session());
    let first = reader
        .decode(br#"{"seq":1,"ts_ms":10,"type":"sta"#)
        .unwrap();
    assert!(first.is_empty(), "half a line is not a record");
    let second = reader.decode(b"te\",\"data\":{}}\n").unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].kind, "state");
    assert!(matches!(
        reader.decode(b"not json\n"),
        Err(AgentSessionError::Transport(AttachError::Protocol(_)))
    ));
}

fn ready_frame() -> FrameKind {
    FrameKind::BootstrapReady {
        terminal_id: session(),
        stream_id: phux_protocol::ids::StreamId::new(1).unwrap(),
        bootstrap_id: phux_protocol::ids::BootstrapId::new(1).unwrap(),
        history_cursor: None,
    }
}

fn closed_frame() -> FrameKind {
    FrameKind::ResourceClosed {
        terminal_id: session(),
        exit_status: None,
        reason: phux_protocol::wire::frame::CloseReason::Unknown,
        signal: None,
    }
}

fn live_frame(bytes: &[u8]) -> FrameKind {
    FrameKind::ResourceOutput {
        terminal_id: session(),
        stream_id: phux_protocol::ids::StreamId::new(1).unwrap(),
        bootstrap_id: phux_protocol::ids::BootstrapId::new(1).unwrap(),
        seq: 1,
        bytes: bytes::Bytes::copy_from_slice(bytes),
    }
}

#[test]
fn reader_record_bound_includes_lf_and_precedes_buffer_growth() {
    let empty = "{\"seq\":1,\"ts_ms\":1,\"type\":\"provider_raw\",\"data\":{\"text\":\"\"}}\n";
    let line = empty.replace(
        "\"text\":\"\"",
        &format!(
            "\"text\":\"{}\"",
            "x".repeat(MAX_RECORD_BYTES - empty.len())
        ),
    );
    assert_eq!(line.len(), MAX_RECORD_BYTES);
    let mut reader = Reader::new(session());
    assert!(
        reader
            .decode(&line.as_bytes()[..line.len() - 1])
            .unwrap()
            .is_empty()
    );
    assert_eq!(reader.buffer.len(), MAX_RECORD_BYTES - 1);
    assert_eq!(reader.decode(b"\n").unwrap().len(), 1);
    for suffix in [b"x".as_slice(), b"x\n".as_slice()] {
        let mut reader = Reader::new(session());
        reader.decode(&line.as_bytes()[..line.len() - 1]).unwrap();
        assert!(matches!(
            reader.decode(suffix),
            Err(AgentSessionError::Transport(AttachError::Protocol(_)))
        ));
        assert_eq!(
            reader.buffer.len(),
            MAX_RECORD_BYTES - 1,
            "oversize was never appended"
        );
    }
    let mut reader = Reader::new(session());
    assert!(
        reader.decode(&vec![b' '; MAX_RECORD_BYTES]).is_err(),
        "blank bytes cannot bypass the bound"
    );
    assert!(reader.buffer.is_empty());
}

#[test]
fn reader_fragments_preserve_utf8_and_multiple_complete_records() {
    let payload = b"{\"seq\":1,\"ts_ms\":10,\"type\":\"prompt\",\"data\":{\"text\":\"caf\xc3\xa9\"}}\n{\"seq\":2,\"ts_ms\":20,\"type\":\"stop\",\"data\":{}}\n";
    for width in 1..=payload.len() {
        let mut reader = Reader::new(session());
        let mut records = Vec::new();
        for bytes in payload.chunks(width) {
            records.extend(reader.decode(bytes).unwrap());
            assert!(reader.decode(b"").unwrap().is_empty());
        }
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].data["text"], "café");
        assert_eq!(records[1].seq, 2);
        assert!(matches!(
            reader.absorb(ready_frame()).unwrap(),
            Absorbed::Ready
        ));
    }
}

#[test]
fn reader_rejects_incomplete_ready_close_and_live_records() {
    for end in [
        ready_frame(),
        closed_frame(),
        FrameKind::Event {
            terminal: Some(session()),
            event: AgentEvent::ResourceClosed { exit_status: None },
            stamp: None,
        },
    ] {
        let mut reader = Reader::new(session());
        reader.decode(b"{\"seq\":1").unwrap();
        assert!(matches!(
            reader.absorb(end),
            Err(AgentSessionError::Transport(AttachError::Protocol(_)))
        ));
    }
    let partial = b"{\"seq\":1,\"ts_ms\":1,\"type\":\"stop\",\"data\":{}}\n{";
    for payload in [partial.as_slice(), b"", b"\n"] {
        let mut reader = Reader::new(session());
        assert!(
            matches!(
                reader.absorb(live_frame(payload)),
                Err(AgentSessionError::Transport(AttachError::Protocol(_)))
            ),
            "a valid prefix cannot hide a partial or empty live batch"
        );
    }
}

#[tokio::test]
async fn malformed_bootstrap_publishes_nothing_and_detaches() {
    assert_failed_bootstrap_detaches(b"{partial").await;
}

#[tokio::test]
async fn malformed_json_already_refused_by_the_decoder_still_detaches() {
    assert_failed_bootstrap_detaches(b"not json\n").await;
}

async fn assert_failed_bootstrap_detaches(payload: &[u8]) {
    let spec = ScriptSpec::new()
        .server_features(kinds())
        .agent_log_bootstrap_bytes(&session(), payload, false);
    let (_dir, socket, server) = serve(spec);
    let mut conn = Connection::connect(&socket).await.unwrap();
    let mut records = Vec::new();
    let error = log(&mut conn, &session(), LogOptions::default(), |record| {
        records.push(record);
        true
    })
    .await
    .unwrap_err();
    assert!(
        matches!(
            error,
            AgentSessionError::Transport(AttachError::Protocol(_))
        ),
        "{error:?}"
    );
    assert!(records.is_empty());
    drop(conn);
    let seen = server.await.unwrap();
    assert!(
        seen.iter().any(|frame| matches!(frame, FrameKind::Command {
        command: Command::DetachResource { terminal_id }, ..
    } if *terminal_id == session())),
        "failure must detach: {seen:?}"
    );
}

#[tokio::test]
async fn malformed_live_batch_publishes_no_prefix_and_detaches() {
    let spec = ScriptSpec::new()
        .server_features(kinds())
        .agent_log_bootstrap(&session(), &[])
        .push(live_frame(
            b"{\"seq\":1,\"ts_ms\":1,\"type\":\"stop\",\"data\":{}}\n{",
        ));
    let (_dir, socket, server) = serve(spec);
    let mut conn = Connection::connect(&socket).await.unwrap();
    let mut records = Vec::new();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            log(
                &mut conn,
                &session(),
                LogOptions {
                    follow: true,
                    tail: None
                },
                |record| {
                    records.push(record);
                    true
                }
            )
        )
        .await
        .expect("malformed live frame must fail promptly")
        .is_err()
    );
    assert!(records.is_empty());
    drop(conn);
    let seen = server.await.unwrap();
    assert!(seen.iter().any(|frame| matches!(frame, FrameKind::Command {
        command: Command::DetachResource { terminal_id }, ..
    } if *terminal_id == session())));
}

#[tokio::test]
async fn closed_bootstrap_completes_a_follow_without_waiting_for_another_close() {
    let payload = b"{\"seq\":1,\"ts_ms\":1,\"type\":\"stop\",\"data\":{}}\n";
    let spec = ScriptSpec::new()
        .server_features(kinds())
        .agent_log_bootstrap_bytes(&session(), payload, true);
    let (_dir, socket, server) = serve(spec);
    let mut conn = Connection::connect(&socket).await.unwrap();
    let mut records = Vec::new();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        log(
            &mut conn,
            &session(),
            LogOptions {
                follow: true,
                tail: None,
            },
            |record| {
                records.push(record);
                true
            },
        ),
    )
    .await
    .expect("already closed stream must finish")
    .unwrap();
    assert_eq!(outcome.end, LogEnd::SessionClosed);
    assert_eq!((outcome.retained, outcome.delivered), (1, 1));
    assert_eq!(records.len(), 1);
    drop(conn);
    server.await.unwrap();
}
