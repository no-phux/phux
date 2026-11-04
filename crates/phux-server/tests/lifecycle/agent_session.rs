//! The `AgentSession` resource kind over the production dispatch loop
//! (ADR-0103, ADR-0104): spawn and facet, append fan-out and bootstrap
//! replay, ring overflow, wrong-kind refusals, lifecycle edges and the
//! parent-close cascade, and the `phux.agent/v1` projection. The actor engine
//! itself is unit-tested in `src/resource/agent_session/`.

use std::collections::HashMap;

use phux_protocol::caps::BootstrapStreamProfile;
use phux_protocol::ids::{BootstrapId, ResourceId, ResourceKind, StreamId};
use phux_protocol::input::key::PhysicalKey;
use phux_protocol::wire::frame::{
    AgentEvent, CloseReason, Command, CommandResult, ErrorCode, FrameKind, ReportedAgentState,
    Scope, SpawnError, SpawnResource, SpawnResult, TerminalSignal,
};
use phux_server_testkit::{Spawn, ascii_key, command, send_frame, spawn_resource};
use portable_pty::CommandBuilder;
use tokio::net::UnixStream;

use crate::common::{Server, attach, get_metadata, sh, spawned, state, subscribe, wait_frame};

#[path = "agent_session/family.rs"]
mod family;
#[path = "agent_session/frames.rs"]
mod frames;
#[path = "agent_session/resume.rs"]
mod resume;

const FOREVER: &str = "while :; do sleep 3600; done";

/// A server whose seed pane never exits, with `owner` attached to it.
async fn start(configure: impl FnOnce(&mut phux_server::ServerConfig)) -> (Server, UnixStream) {
    let server = Server::start(Some("demo"), |cfg| {
        let mut seed = CommandBuilder::new("/bin/sh");
        seed.args(["-c", FOREVER]);
        phux_server_testkit::seed_pty(cfg, seed);
        configure(cfg);
    });
    let mut owner = server.connect().await;
    attach(&mut owner, "demo").await;
    (server, owner)
}

async fn spawn_session(
    stream: &mut UnixStream,
    request_id: u32,
    parent: ResourceId,
) -> SpawnResult {
    let spawn = Spawn {
        resource: Some(Box::new(SpawnResource::agent_session(parent, "claude"))),
        ..Spawn::default()
    };
    spawn_resource(stream, request_id, spawn).await
}

/// A parent Terminal running `script`, and an `AgentSession` bound under it.
async fn parent_and_session(stream: &mut UnixStream, script: &str) -> (ResourceId, ResourceId) {
    let parent = spawned(stream, 1, sh(script)).await;
    let SpawnResult::Ok(session) = spawn_session(stream, 2, parent.clone()).await else {
        panic!("spawn session failed");
    };
    (parent, session)
}

async fn append(stream: &mut UnixStream, request_id: u32, session: &ResourceId, records: &str) {
    let append = Command::AppendResourceOutput {
        terminal_id: session.clone(),
        bytes: records.as_bytes().to_vec(),
    };
    let result = command(stream, request_id, append).await;
    assert!(
        matches!(result, CommandResult::OkWith(_)),
        "append: {result:?}"
    );
}

async fn attach_resource(stream: &mut UnixStream, request_id: u32, terminal_id: &ResourceId) {
    attach_after(stream, request_id, terminal_id, None).await;
}

async fn attach_after(
    stream: &mut UnixStream,
    request_id: u32,
    terminal_id: &ResourceId,
    agent_after_seq: Option<u64>,
) {
    let attach = Command::attach_agent_session(terminal_id.clone(), None, agent_after_seq);
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command: attach,
        },
    )
    .await;
}

/// The next live `RESOURCE_OUTPUT` for `session`: its seq and bytes.
async fn next_output(stream: &mut UnixStream, session: &ResourceId) -> (u64, Vec<u8>) {
    wait_frame(stream, "agent output", |frame| match frame {
        FrameKind::ResourceOutput {
            terminal_id,
            seq,
            bytes,
            ..
        } if &terminal_id == session => Some((seq, bytes.to_vec())),
        _ => None,
    })
    .await
}

/// A bootstrap: (`BEGIN` grid, profile, `base_seq`, concatenated chunks).
async fn bootstrap(
    stream: &mut UnixStream,
    session: &ResourceId,
) -> ((u16, u16), BootstrapStreamProfile, u64, Vec<u8>) {
    let mut begin = None;
    let mut chunks = Vec::new();
    let mut generation = None;
    wait_frame(stream, "bootstrap", |frame| match frame {
        FrameKind::BootstrapBegin {
            terminal_id,
            cols,
            rows,
            profile,
            base_seq,
            bootstrap_id,
            ..
        } if &terminal_id == session => {
            begin = Some(((cols, rows), profile, base_seq));
            generation = Some(bootstrap_id);
            None
        }
        FrameKind::BootstrapChunk {
            terminal_id,
            payload,
            bootstrap_id,
            ..
        } if &terminal_id == session => {
            assert_eq!(
                Some(bootstrap_id),
                generation,
                "chunk from an old generation"
            );
            chunks.extend_from_slice(&payload);
            None
        }
        FrameKind::BootstrapReady {
            terminal_id,
            bootstrap_id,
            ..
        } if &terminal_id == session => {
            assert_eq!(
                Some(bootstrap_id),
                generation,
                "READY from an old generation"
            );
            Some(())
        }
        FrameKind::ResourceOutput { terminal_id, .. }
            if &terminal_id == session && begin.is_some() =>
        {
            panic!("old-generation live output interleaved with the bootstrap");
        }
        _ => None,
    })
    .await;
    let (grid, profile, base_seq) = begin.expect("BOOTSTRAP_BEGIN");
    (grid, profile, base_seq, chunks)
}

fn records(payload: &[u8]) -> Vec<serde_json::Value> {
    payload
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}

fn record_seqs(payload: &[u8]) -> Vec<u64> {
    records(payload)
        .iter()
        .map(|r| r["seq"].as_u64().unwrap())
        .collect()
}

/// Append from the auto-subscribed owner, returning the owner's own live
/// copy, which may arrive before or after the reply.
async fn append_as_owner(
    owner: &mut UnixStream,
    request_id: u32,
    session: &ResourceId,
    records: &str,
) -> Vec<u8> {
    let append = Command::AppendResourceOutput {
        terminal_id: session.clone(),
        bytes: records.as_bytes().to_vec(),
    };
    send_frame(
        owner,
        &FrameKind::Command {
            request_id,
            command: append,
        },
    )
    .await;
    let mut copy = Vec::new();
    wait_frame(owner, "append reply", |frame| match frame {
        FrameKind::ResourceOutput {
            terminal_id, bytes, ..
        } if &terminal_id == session => {
            copy.extend_from_slice(&bytes);
            None
        }
        FrameKind::CommandResult {
            request_id: got,
            result,
        } if got == request_id => {
            assert!(
                matches!(result, CommandResult::OkWith(_)),
                "append: {result:?}"
            );
            Some(())
        }
        _ => None,
    })
    .await;
    if copy.is_empty() {
        copy = next_output(owner, session).await.1;
    }
    copy
}

/// The `CloseReason` of every id in `targets`, collected in one pass (the
/// closes race each other), counting each close to catch doubles.
async fn closes(
    stream: &mut UnixStream,
    targets: &[ResourceId],
) -> HashMap<ResourceId, (CloseReason, u32)> {
    let mut seen: HashMap<ResourceId, (CloseReason, u32)> = HashMap::new();
    wait_frame(stream, "RESOURCE_CLOSED for every target", |frame| {
        if let FrameKind::ResourceClosed {
            terminal_id,
            reason,
            ..
        } = frame
        {
            seen.entry(terminal_id).or_insert((reason, 0)).1 += 1;
        }
        targets.iter().all(|t| seen.contains_key(t)).then_some(())
    })
    .await;
    seen
}

fn assert_wrong_kind(result: &CommandResult, what: &str) {
    assert!(
        matches!(
            result,
            CommandResult::Error {
                code: ErrorCode::WrongResourceKind,
                ..
            }
        ),
        "{what}: {result:?}"
    );
}

/// Spawn registers the child with its kind, parent, and facet; a
/// multi-record append reaches the owner and an `ATTACH_RESOURCE`-only
/// watcher under the last record's seq; a late attach replays the retained
/// records in a 0x0 `AgentEventsJsonlV1` bootstrap cut at that seq.
#[test]
fn session_spawn_append_fan_out_and_bootstrap_replay() {
    phux_server_testkit::run_local(async {
        let (server, mut owner) = start(|_| {}).await;
        let (parent, session) = parent_and_session(&mut owner, FOREVER).await;
        let snapshot = state(&mut owner, 3).await;
        let entry = snapshot.resources.iter().find(|p| p.id == session).unwrap();
        assert_eq!(entry.kind, ResourceKind::AgentSession);
        assert_eq!(entry.parent, Some(parent));
        assert_eq!(entry.agent.as_ref().unwrap().provider, "claude");

        let mut watcher = server.connect().await;
        attach_resource(&mut watcher, 100, &session).await;
        let _ = bootstrap(&mut watcher, &session).await;
        let owner_copy = append_as_owner(
            &mut owner,
            4,
            &session,
            "{\"type\":\"prompt\"}\n{\"type\":\"ask\"}\n{\"type\":\"stop\"}\n",
        )
        .await;
        let (seq, payload) = next_output(&mut watcher, &session).await;
        assert_eq!(
            seq, 3,
            "the envelope carries the last record's seq, not an append count"
        );
        assert_eq!(record_seqs(&payload), [1, 2, 3]);
        assert_eq!(
            records(&owner_copy),
            records(&payload),
            "the owner sees its own records"
        );

        let mut late = server.connect().await;
        attach_resource(&mut late, 200, &session).await;
        let (grid, profile, base_seq, retained) = bootstrap(&mut late, &session).await;
        assert_eq!(grid, (0, 0));
        assert_eq!(profile, BootstrapStreamProfile::AgentEventsJsonlV1);
        assert_eq!(base_seq, 3);
        assert_eq!(records(&retained), records(&payload));
        append(
            &mut owner,
            5,
            &session,
            "{\"type\":\"prompt\"}\n{\"type\":\"ask\"}\n",
        )
        .await;
        let (seq, payload) = next_output(&mut late, &session).await;
        assert_eq!((seq, record_seqs(&payload)), (5, vec![4, 5]));

        drop((owner, watcher, late));
        server.stop().await;
    });
}

#[test]
fn ring_overflow_evicts_oldest_but_keeps_counting() {
    phux_server_testkit::run_local(async {
        let (server, mut owner) = start(|cfg| cfg.agent_log_bytes = 200).await;
        let (_parent, session) = parent_and_session(&mut owner, FOREVER).await;
        for request_id in 10..40 {
            append(
                &mut owner,
                request_id,
                &session,
                "{\"type\":\"tool_end\"}\n",
            )
            .await;
        }
        let mut late = server.connect().await;
        attach_resource(&mut late, 200, &session).await;
        let (_, _, base_seq, retained) = bootstrap(&mut late, &session).await;
        assert_eq!(base_seq, 30, "evicted appends still took a sequence");
        let seqs = record_seqs(&retained);
        assert!(
            !seqs.is_empty() && seqs.len() < 30,
            "only the newest records survive: {seqs:?}"
        );

        drop((owner, late));
        server.stop().await;
    });
}

#[test]
fn stamped_bounds_and_small_chunk_reconnect_preserve_a_decodable_stream() {
    phux_server_testkit::run_local(async {
        use phux_protocol::caps::{BootstrapCapabilities, BootstrapLimits, ClientCapabilities};
        use phux_server_testkit::{SOCKET_CONNECT_DEADLINE, wait_for_raw_socket};

        let (server, mut owner) = start(|_| {}).await;
        let (_parent, session) = parent_and_session(&mut owner, FOREVER).await;
        let prefix = "{\"type\":\"provider_raw\",\"data\":{\"text\":\"";
        let suffix = "\"}}";
        let oversized = format!(
            "{prefix}{}{suffix}",
            "x".repeat(16_383 - prefix.len() - suffix.len())
        );
        let result = command(
            &mut owner,
            3,
            Command::AppendResourceOutput {
                terminal_id: session.clone(),
                bytes: format!("{{\"type\":\"prompt\"}}\n{oversized}").into_bytes(),
            },
        )
        .await;
        assert!(
            matches!(
                result,
                CommandResult::Error {
                    code: ErrorCode::RecordInvalid,
                    ..
                }
            ),
            "{result:?}"
        );
        let accepted = format!("{prefix}{}{suffix}", "é".repeat(8_000));
        let live = append_as_owner(&mut owner, 4, &session, &accepted).await;
        let parsed = phux_client_core::session::agent_stream::parse_records(&live)
            .expect("live codec accepts canonical line");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].seq, 1, "rejected batch consumed no sequence");

        // Reconnect twice with a budget that splits both JSON and UTF-8.
        for _ in 0..2 {
            let mut late = wait_for_raw_socket(&server.socket, SOCKET_CONNECT_DEADLINE).await;
            let version = phux_protocol::PROTOCOL_VERSION;
            send_frame(
                &mut late,
                &FrameKind::Hello {
                    client_name: "small-agent-chunks".to_owned(),
                    protocol_major: version.major,
                    protocol_minor: version.minor,
                    protocol_patch: version.patch,
                    client_caps: ClientCapabilities::new().with_bootstrap(
                        BootstrapCapabilities::new()
                            .with_limits(BootstrapLimits::new(7, 1024).unwrap()),
                    ),
                },
            )
            .await;
            wait_frame(&mut late, "HELLO_OK", |frame| {
                matches!(frame, FrameKind::HelloOk { .. }).then_some(())
            })
            .await;
            attach_resource(&mut late, 100, &session).await;
            let mut retained = Vec::new();
            let mut next_chunk = 0;
            wait_frame(&mut late, "bounded agent bootstrap", |frame| match frame {
                FrameKind::BootstrapBegin {
                    terminal_id,
                    base_seq,
                    ..
                } if terminal_id == session => {
                    assert_eq!(base_seq, 1);
                    None
                }
                FrameKind::BootstrapChunk {
                    terminal_id,
                    chunk_seq,
                    payload,
                    ..
                } if terminal_id == session => {
                    assert_eq!(chunk_seq, next_chunk);
                    next_chunk += 1;
                    assert!(!payload.is_empty() && payload.len() <= 7);
                    retained.extend_from_slice(&payload);
                    None
                }
                FrameKind::BootstrapReady { terminal_id, .. } if terminal_id == session => Some(()),
                _ => None,
            })
            .await;
            assert_eq!(retained, live);
            assert_eq!(
                phux_client_core::session::agent_stream::parse_records(&retained).unwrap(),
                parsed
            );
        }
        drop(owner);
        server.stop().await;
    });
}
