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
    let attach = Command::AttachResource {
        terminal_id: terminal_id.clone(),
        role_policy: None,
    };
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
    wait_frame(stream, "bootstrap", |frame| match frame {
        FrameKind::BootstrapBegin {
            terminal_id,
            cols,
            rows,
            profile,
            base_seq,
            ..
        } if &terminal_id == session => {
            begin = Some(((cols, rows), profile, base_seq));
            None
        }
        FrameKind::BootstrapChunk {
            terminal_id,
            payload,
            ..
        } if &terminal_id == session => {
            chunks.extend_from_slice(&payload);
            None
        }
        FrameKind::BootstrapReady { terminal_id, .. } if &terminal_id == session => Some(()),
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

async fn uncorrelated_wrong_kind(stream: &mut UnixStream, what: &str) {
    wait_frame(stream, what, |frame| {
        matches!(
            frame,
            FrameKind::Error {
                code: ErrorCode::WrongResourceKind,
                ..
            }
        )
        .then_some(())
    })
    .await;
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
fn terminal_facet_commands_refuse_an_agent_session_and_vice_versa() {
    phux_server_testkit::run_local(async {
        let (server, mut owner) = start(|_| {}).await;
        let (parent, session) = parent_and_session(&mut owner, FOREVER).await;

        let key = FrameKind::InputKey {
            terminal_id: session.clone(),
            event: ascii_key('a', PhysicalKey::A),
        };
        send_frame(&mut owner, &key).await;
        uncorrelated_wrong_kind(&mut owner, "INPUT_KEY refusal").await;
        let history = FrameKind::HistoryRequest {
            terminal_id: session.clone(),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            cursor: bytes::Bytes::new(),
            max_bytes: 1024,
            max_rows: 10,
        };
        send_frame(&mut owner, &history).await;
        uncorrelated_wrong_kind(&mut owner, "HISTORY_REQUEST refusal").await;

        let screen = Command::GetScreen {
            terminal_id: session.clone(),
            request_scrollback: None,
            cells: false,
            format: 0,
        };
        assert_wrong_kind(&command(&mut owner, 10, screen).await, "GET_SCREEN");
        let signal = Command::SignalTerminal {
            terminal_id: session,
            signal: TerminalSignal::Interrupt,
            operation_id: None,
        };
        assert_wrong_kind(&command(&mut owner, 11, signal).await, "SIGNAL_TERMINAL");
        let append = Command::AppendResourceOutput {
            terminal_id: parent,
            bytes: b"{\"type\":\"prompt\"}\n".to_vec(),
        };
        assert_wrong_kind(
            &command(&mut owner, 12, append).await,
            "APPEND on a Terminal",
        );

        drop(owner);
        server.stop().await;
    });
}

#[test]
fn spawn_with_unknown_or_session_parent_earns_typed_refusals() {
    phux_server_testkit::run_local(async {
        let (server, mut owner) = start(|_| {}).await;
        let result = spawn_session(&mut owner, 1, ResourceId::local(999_999)).await;
        assert_eq!(result, SpawnResult::Err(SpawnError::ParentNotFound));
        let (_parent, session) = parent_and_session(&mut owner, FOREVER).await;
        let result = spawn_session(&mut owner, 4, session).await;
        assert_eq!(result, SpawnResult::Err(SpawnError::ParentKindMismatch));

        drop(owner);
        server.stop().await;
    });
}

/// A watcher scoped to the pane (plus server-wide: one subscription entry,
/// one frame) hears the child's spawn and close exactly once each, though
/// the events name the child it could not have subscribed to (ADR-0104 §2).
#[test]
fn a_childs_spawn_and_close_reach_the_parents_watchers_once() {
    phux_server_testkit::run_local(async {
        let (server, mut owner) = start(|_| {}).await;
        let parent = spawned(&mut owner, 1, sh(FOREVER)).await;
        let mut watcher = server.connect().await;
        send_frame(
            &mut watcher,
            &FrameKind::SubscribeEvents {
                terminal: None,
                after_seq: None,
            },
        )
        .await;
        subscribe(&mut watcher, 100, Some(parent.clone()), None).await;

        let SpawnResult::Ok(session) = spawn_session(&mut owner, 2, parent).await else {
            panic!("spawn session failed");
        };
        let kill = Command::KillResource {
            terminal_id: session.clone(),
            operation_id: None,
        };
        assert_eq!(command(&mut owner, 3, kill).await, CommandResult::Ok);

        // Count to a barrier on the watcher's own connection.
        send_frame(
            &mut watcher,
            &FrameKind::Command {
                request_id: 101,
                command: Command::GetState {
                    scope: phux_protocol::wire::frame::StateScope::Server,
                },
            },
        )
        .await;
        let (mut spawned_n, mut closed_n) = (0, 0);
        wait_frame(&mut watcher, "barrier", |frame| match frame {
            FrameKind::Event {
                terminal: Some(id),
                event,
                ..
            } if id == session => {
                match event {
                    AgentEvent::ResourceSpawned { kind, parent } => {
                        assert_eq!(kind, ResourceKind::AgentSession);
                        assert!(parent.is_some());
                        spawned_n += 1;
                    }
                    AgentEvent::ResourceClosed { .. } => closed_n += 1,
                    _ => {}
                }
                None
            }
            FrameKind::CommandResult {
                request_id: 101, ..
            } => Some(()),
            _ => None,
        })
        .await;
        assert_eq!((spawned_n, closed_n), (1, 1));

        drop((owner, watcher));
        server.stop().await;
    });
}

/// Killing the parent and its PTY exiting both cascade the child with
/// `ParentClosed`, to an `ATTACH_RESOURCE` watcher as well as the owner.
#[test]
fn parent_kill_and_pty_exit_cascade_the_child_with_parent_closed() {
    phux_server_testkit::run_local(async {
        let (server, mut owner) = start(|_| {}).await;
        let (parent, session) = parent_and_session(&mut owner, FOREVER).await;
        let mut watcher = server.connect().await;
        attach_resource(&mut watcher, 100, &session).await;
        let kill = Command::KillResource {
            terminal_id: parent.clone(),
            operation_id: None,
        };
        assert_eq!(command(&mut owner, 3, kill).await, CommandResult::Ok);
        let seen = closes(&mut watcher, std::slice::from_ref(&session)).await;
        assert_eq!(seen[&session].0, CloseReason::ParentClosed);
        let seen = closes(&mut owner, &[parent.clone(), session.clone()]).await;
        assert_eq!(seen[&session].0, CloseReason::ParentClosed);
        assert_eq!(seen[&parent].0, CloseReason::Killed);

        let parent = spawned(&mut owner, 10, sh("read _line; exit 5")).await;
        let SpawnResult::Ok(session) = spawn_session(&mut owner, 11, parent.clone()).await else {
            panic!("spawn session failed");
        };
        crate::common::release(&mut owner, &parent).await;
        let seen = closes(&mut owner, &[parent.clone(), session.clone()]).await;
        assert_eq!(seen[&session].0, CloseReason::ParentClosed);
        assert_eq!(seen[&parent].0, CloseReason::Exited);

        drop((owner, watcher));
        server.stop().await;
    });
}

/// `KILL_RESOURCES` naming a parent, its child, and a bystander closes each
/// exactly once (the child is not double-closed by the cascade).
#[test]
fn kill_resources_over_a_mixed_set_closes_each_once() {
    phux_server_testkit::run_local(async {
        let (server, mut owner) = start(|_| {}).await;
        let (parent, session) = parent_and_session(&mut owner, FOREVER).await;
        let unrelated = spawned(&mut owner, 3, sh(FOREVER)).await;
        let targets = [parent, session, unrelated];
        let kill = Command::KillResources {
            ids: targets.to_vec(),
            operation_id: None,
        };
        assert_eq!(command(&mut owner, 4, kill).await, CommandResult::Ok);
        let mut seen = closes(&mut owner, &targets).await;
        // Everything the cascade emitted is now queued ahead of this reply,
        // so a duplicate close would be counted before it.
        send_frame(
            &mut owner,
            &FrameKind::Command {
                request_id: 5,
                command: Command::GetState {
                    scope: phux_protocol::wire::frame::StateScope::Server,
                },
            },
        )
        .await;
        wait_frame(&mut owner, "barrier", |frame| match frame {
            FrameKind::ResourceClosed {
                terminal_id,
                reason,
                ..
            } => {
                seen.entry(terminal_id).or_insert((reason, 0)).1 += 1;
                None
            }
            FrameKind::CommandResult { request_id: 5, .. } => Some(()),
            _ => None,
        })
        .await;
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert!(seen.values().all(|(_, n)| *n == 1), "{seen:?}");

        drop(owner);
        server.stop().await;
    });
}

/// Write an executable named `claude` (so the detector identifies the pane
/// by its foreground argv) that just sleeps.
fn write_fake_claude(dir: &std::path::Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("claude");
    std::fs::write(&path, format!("#!/bin/sh\n{FOREVER}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

/// Poll `phux.agent/v1` on `parent` until it contains `needle` (the arbiter
/// writes it off a side-channel drain task, not inline with the reply).
async fn await_agent_record(stream: &mut UnixStream, base: u32, parent: &ResourceId, needle: &str) {
    let deadline = tokio::time::Instant::now() + phux_server_testkit::WIRE_RECV_TIMEOUT;
    for request_id in base.. {
        let scope = Scope::Resource(parent.clone());
        let key = phux_protocol::wire::frame::RESOURCE_AGENT_KEY;
        if get_metadata(stream, request_id, scope, key)
            .await
            .is_some_and(|bytes| String::from_utf8_lossy(&bytes).contains(needle))
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "phux.agent/v1 never showed {needle}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Stream evidence projects onto the parent's `phux.agent/v1` record
/// (prompt -> working, stop -> done), and `REPORT_AGENT_STATE` with a live
/// child lands as a `state` record on the child's stream too.
#[test]
fn stream_evidence_and_reported_state_project_onto_the_parents_agent_record() {
    // SAFETY: nextest runs each test in its own process; no thread exists yet.
    unsafe {
        std::env::set_var("PHUX_AGENT_STARTUP_GRACE_MS", "200");
        std::env::set_var("PHUX_AGENT_IDENTIFY_RECHECK_MS", "200");
    }
    phux_server_testkit::run_local(async {
        let (server, mut owner) = start(|_| {}).await;
        let claude = write_fake_claude(server.tmp.path());
        let parent = spawned(&mut owner, 1, Spawn::command(&[&claude])).await;
        let SpawnResult::Ok(session) = spawn_session(&mut owner, 2, parent.clone()).await else {
            panic!("spawn session failed");
        };

        append(&mut owner, 3, &session, "{\"type\":\"prompt\"}\n").await;
        await_agent_record(&mut owner, 100, &parent, "\"state\":\"working\"").await;
        append(&mut owner, 4, &session, "{\"type\":\"stop\"}\n").await;
        await_agent_record(&mut owner, 200, &parent, "\"state\":\"done\"").await;

        let mut watcher = server.connect().await;
        attach_resource(&mut watcher, 100, &session).await;
        let _ = bootstrap(&mut watcher, &session).await;
        let report = Command::ReportAgentState {
            terminal_id: parent.clone(),
            state: ReportedAgentState::Blocked,
        };
        assert_eq!(command(&mut owner, 5, report).await, CommandResult::Ok);
        let (_, record) = next_output(&mut watcher, &session).await;
        let record = String::from_utf8_lossy(&record);
        assert!(
            record.contains("\"type\":\"state\"") && record.contains("\"state\":\"blocked\""),
            "{record}"
        );
        await_agent_record(&mut owner, 300, &parent, "\"state\":\"blocked\"").await;

        drop((owner, watcher));
        server.stop().await;
    });
}
