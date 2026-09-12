//! Wire-level integration tests for the `AgentSession` resource kind
//! (ADR-0103, ADR-0104; phux-am9y.9 / phux-am9y.10).
//!
//! `bd memory wire-dispatch-test-rule`: a new `FrameKind` dispatch gap only
//! shows up on the production `handle_client` read/dispatch loop, not in a
//! unit test against `AgentSessionActor` alone (that engine is already
//! covered directly in `crates/phux-server/src/resource/agent_session/`).
//! Every test here drives a real server over its Unix-domain socket.
//!
//! Coverage: spawning a session bound to a Terminal parent and its
//! `GET_STATE` facet; `APPEND_RESOURCE_OUTPUT` fan-out to two live
//! subscribers; bootstrap replay of retained records; ring overflow and its
//! tombstone; every Terminal-facet wrong-kind refusal an `AgentSession`
//! earns; parent-close cascade (explicit kill and PTY exit) with
//! `CloseReason`; a mixed `KILL_RESOURCES` batch; the two `SpawnError`s a
//! malformed parent earns; the `phux.agent/v1` projection derived from the
//! stream; and `REPORT_AGENT_STATE` landing as a stream record once a child
//! is live.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(
    clippy::doc_markdown,
    reason = "test-only file; the narrative uses bare wire-frame names the way the sibling integration tests do"
)]

use std::time::Duration;

use phux_protocol::ids::{
    BootstrapId, GroupId, ResourceId, ResourceKind as WireResourceKind, StreamId,
};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, ErrorCode, FrameKind, ReportedAgentState, Scope,
    SpawnError, SpawnResource, SpawnResult, StateScope,
};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, attach_by_name, await_command_result,
    join_after_shutdown, recv_typed, run_local, send_frame, spawn_server_with,
    spawn_server_with_seed_cmd, wait_for_socket,
};

/// A shell that never exits on its own — every parent Terminal these tests
/// bind an `AgentSession` under, unless the test is specifically about the
/// parent exiting.
fn immortal_shell() -> CommandBuilder {
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.arg("-c");
    cmd.arg("while :; do sleep 3600; done");
    cmd
}

/// Shrink the detector's startup grace and identity recheck interval for
/// this test process (production defaults 3s / 5s). Must run before the
/// server — and therefore any detector — starts; the overrides are read
/// once per process. Same seam `agents/agent_detect.rs` uses.
fn shorten_agent_detect_timers() {
    // SAFETY-adjacent: `set_var` is unsafe on edition 2024 because of
    // concurrent env access; nextest runs each test in its own process and
    // this runs before the server thread exists.
    unsafe {
        std::env::set_var("PHUX_AGENT_STARTUP_GRACE_MS", "200");
        std::env::set_var("PHUX_AGENT_IDENTIFY_RECHECK_MS", "200");
    }
}

/// Write an executable no-op agent literally named `claude` into `dir`, and
/// return its path.
///
/// `phux.agent/v1` state derivation (ADR-0103 §5) only ever *ranks* the
/// stream's evidence — the arbiter still requires the pane's detector to
/// have identified an occupant before it publishes anything at all
/// (`AgentDetector::report_stream_state`), and identification reads the
/// PTY's foreground process argv, resolved against `rules/claude.toml`'s
/// `binaries = ["claude", "claude-code"]`. A shebang script invoked by this
/// path keeps that argv as the script's own path, so naming the file
/// `claude` is what makes identification resolve — no screen painting
/// required, since binary-name identification does not read the grid.
fn write_fake_claude(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("claude");
    std::fs::write(&path, "#!/bin/sh\nwhile :; do sleep 3600; done\n").expect("write fake agent");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake agent");
    }
    path
}

/// A shell that exits with `code` the moment a line reaches its stdin —
/// used to make a PTY exit deterministic without a race against the test's
/// own setup (spawning the `AgentSession` child before the parent is gone).
fn exits_on_input_argv(code: u8) -> Vec<String> {
    vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        format!("read _line; exit {code}"),
    ]
}

/// Spawn a server with an immortal seed pane, connect, and drain the
/// `ATTACHED` + first bootstrap frame so later `recv_typed` calls only see
/// test-driven traffic. Mirrors `spawn_terminal.rs`'s `spawn_and_attach`.
async fn connect_and_attach(
    tmp: &TempDir,
) -> (
    UnixStream,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<(), phux_server::ServerError>>,
) {
    use phux_protocol::wire::frame::{TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN};
    let socket_path = tmp.path().join("phux.sock");
    let (shutdown_tx, server_handle) =
        spawn_server_with_seed_cmd(socket_path.clone(), "demo", immortal_shell());
    let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(&mut stream, &attach_by_name("demo")).await;
    let (type_byte, _attached) = recv_typed(&mut stream).await;
    assert_eq!(type_byte, TYPE_ATTACHED, "expected ATTACHED");
    let (type_byte, _snap) = recv_typed(&mut stream).await;
    assert_eq!(
        type_byte, TYPE_BOOTSTRAP_BEGIN,
        "expected the seed pane's bootstrap"
    );
    (stream, shutdown_tx, server_handle)
}

/// Like [`connect_and_attach`] but lets the caller configure the
/// [`phux_server::runtime::ServerConfig`] before the socket binds (e.g. a
/// tiny `agent_log_bytes` for the overflow test).
async fn connect_and_attach_with(
    tmp: &TempDir,
    configure: impl FnOnce(&mut phux_server::runtime::ServerConfig),
) -> (
    UnixStream,
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<(), phux_server::ServerError>>,
) {
    use phux_protocol::wire::frame::{TYPE_ATTACHED, TYPE_BOOTSTRAP_BEGIN};
    let socket_path = tmp.path().join("phux.sock");
    let (shutdown_tx, server_handle) =
        spawn_server_with(socket_path.clone(), Some("demo"), |cfg| {
            cfg.seed_with_pty = true;
            cfg.seed_command = Some(immortal_shell());
            configure(cfg);
        });
    let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(&mut stream, &attach_by_name("demo")).await;
    let (type_byte, _attached) = recv_typed(&mut stream).await;
    assert_eq!(type_byte, TYPE_ATTACHED, "expected ATTACHED");
    let (type_byte, _snap) = recv_typed(&mut stream).await;
    assert_eq!(
        type_byte, TYPE_BOOTSTRAP_BEGIN,
        "expected the seed pane's bootstrap"
    );
    (stream, shutdown_tx, server_handle)
}

/// Connect a second, otherwise-unattached client (HELLO only, no session
/// `ATTACH`) — the `ATTACH_RESOURCE`-only shape `docs/spec/L1.md §5.1`
/// guarantees a subscriber over.
async fn connect_bare(tmp: &TempDir) -> UnixStream {
    let socket_path = tmp.path().join("phux.sock");
    wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await
}

async fn await_terminal_spawned(stream: &mut UnixStream, request_id: u32) -> SpawnResult {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline - tokio::time::Instant::now();
        let Ok((_type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            break;
        };
        if let FrameKind::ResourceSpawned {
            request_id: got,
            result,
        } = frame
            && got == request_id
        {
            return result;
        }
    }
    panic!("timed out waiting for RESOURCE_SPAWNED request_id={request_id}");
}

/// Spawn an immortal-shell Terminal (`resource: None`), the parent every
/// `AgentSession` test binds a child under.
async fn spawn_parent_terminal(stream: &mut UnixStream, request_id: u32) -> ResourceId {
    send_frame(
        stream,
        &FrameKind::SpawnResource {
            request_id,
            group: GroupId::new(1),
            command: Some(vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "while :; do sleep 3600; done".to_owned(),
            ]),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: None,
        },
    )
    .await;
    match await_terminal_spawned(stream, request_id).await {
        SpawnResult::Ok(id) => id,
        other => panic!("SPAWN_RESOURCE (parent) failed: {other:?}"),
    }
}

/// Spawn a Terminal that exits the moment it receives one line of input —
/// for the PTY-exit cascade test, where the exit must be triggerable rather
/// than timed.
async fn spawn_parent_terminal_with(
    stream: &mut UnixStream,
    request_id: u32,
    argv: Vec<String>,
) -> ResourceId {
    send_frame(
        stream,
        &FrameKind::SpawnResource {
            request_id,
            group: GroupId::new(1),
            command: Some(argv),
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: None,
        },
    )
    .await;
    match await_terminal_spawned(stream, request_id).await {
        SpawnResult::Ok(id) => id,
        other => panic!("SPAWN_RESOURCE (parent) failed: {other:?}"),
    }
}

/// `SPAWN_RESOURCE { resource: Some(AgentSession { parent, provider }) }`.
async fn spawn_session(
    stream: &mut UnixStream,
    request_id: u32,
    parent: ResourceId,
    provider: &str,
) -> SpawnResult {
    send_frame(
        stream,
        &FrameKind::SpawnResource {
            request_id,
            group: GroupId::new(1),
            command: None,
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: Some(Box::new(SpawnResource::agent_session(parent, provider))),
        },
    )
    .await;
    await_terminal_spawned(stream, request_id).await
}

async fn append(
    stream: &mut UnixStream,
    request_id: u32,
    terminal_id: ResourceId,
    payload: &str,
) -> CommandResult {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command: Command::AppendResourceOutput {
                terminal_id,
                bytes: payload.as_bytes().to_vec(),
            },
        },
    )
    .await;
    await_command_result(stream, request_id).await
}

/// As [`append`], but also returns any `ResourceOutput` bytes for `session`
/// seen while waiting for the reply.
///
/// The appending client is auto-subscribed to its own session's live output
/// (this program's spawn-time auto-attach), so its own broadcast copy can
/// land in the same mailbox as the `COMMAND_RESULT` it is racing —
/// `await_command_result`'s "discard anything that doesn't match" would
/// silently eat that copy if it happened to arrive first. This collects it
/// instead of throwing it away.
async fn append_and_collect(
    stream: &mut UnixStream,
    request_id: u32,
    session: ResourceId,
    payload: &str,
) -> (CommandResult, Vec<u8>) {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command: Command::AppendResourceOutput {
                terminal_id: session.clone(),
                bytes: payload.as_bytes().to_vec(),
            },
        },
    )
    .await;
    let mut acc = Vec::new();
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for COMMAND_RESULT request_id={request_id}"
        );
        let (_type_byte, frame) = timeout(remaining, recv_typed(stream)).await.unwrap();
        match frame {
            FrameKind::ResourceOutput {
                terminal_id, bytes, ..
            } if terminal_id == session => {
                acc.extend_from_slice(&bytes);
            }
            FrameKind::CommandResult {
                request_id: got,
                result,
            } if got == request_id => {
                return (result, acc);
            }
            _ => {}
        }
    }
}

async fn attach_terminal(stream: &mut UnixStream, request_id: u32, terminal_id: ResourceId) {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command: Command::AttachResource { terminal_id },
        },
    )
    .await;
    let result = await_command_result(stream, request_id).await;
    assert!(
        matches!(result, CommandResult::Ok),
        "ATTACH_RESOURCE must succeed, got {result:?}"
    );
}

const fn state_barrier(request_id: u32) -> FrameKind {
    FrameKind::Command {
        request_id,
        command: Command::GetState {
            scope: StateScope::Server,
        },
    }
}

/// Collect every `RESOURCE_OUTPUT` for `session` up to (and including) the
/// `COMMAND_RESULT` for `barrier_request_id`, concatenating their bytes.
async fn drain_live_records_until(
    stream: &mut UnixStream,
    session: &ResourceId,
    barrier_request_id: u32,
) -> Vec<u8> {
    let mut acc = Vec::new();
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out draining live records for {session:?}"
        );
        let Ok((_type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            panic!("timed out draining live records for {session:?}");
        };
        match frame {
            FrameKind::ResourceOutput {
                terminal_id, bytes, ..
            } if terminal_id == *session => {
                acc.extend_from_slice(&bytes);
            }
            FrameKind::CommandResult { request_id, .. } if request_id == barrier_request_id => {
                return acc;
            }
            _ => {}
        }
    }
}

/// Await `RESOURCE_CLOSED` for `victim`, returning its `CloseReason`.
async fn await_terminal_closed_reason(
    stream: &mut UnixStream,
    victim: &ResourceId,
) -> phux_protocol::wire::frame::CloseReason {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for RESOURCE_CLOSED({victim:?})"
        );
        let Ok((_type_byte, frame)) = timeout(remaining, recv_typed(stream)).await else {
            panic!("timed out waiting for RESOURCE_CLOSED({victim:?})");
        };
        if let FrameKind::ResourceClosed {
            terminal_id,
            reason,
            ..
        } = frame
            && terminal_id == *victim
        {
            return reason;
        }
    }
}

/// Collect the `CloseReason` for every id in `targets` observed on `stream`,
/// in one pass — the closes race each other on the wire, so waiting for one
/// target specifically before moving on to the next could silently consume
/// (and lose) a different target's frame that happened to arrive first.
async fn collect_closed_reasons(
    stream: &mut UnixStream,
    targets: &[ResourceId],
) -> std::collections::HashMap<ResourceId, phux_protocol::wire::frame::CloseReason> {
    let mut seen = std::collections::HashMap::new();
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while seen.len() < targets.len() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out collecting RESOURCE_CLOSED for {targets:?}, got {seen:?}"
        );
        let (_type_byte, frame) = timeout(remaining, recv_typed(stream)).await.unwrap();
        if let FrameKind::ResourceClosed {
            terminal_id,
            reason,
            ..
        } = frame
            && targets.contains(&terminal_id)
        {
            seen.insert(terminal_id, reason);
        }
    }
    seen
}

async fn get_metadata(
    stream: &mut UnixStream,
    request_id: u32,
    terminal_id: ResourceId,
) -> Option<Vec<u8>> {
    send_frame(
        stream,
        &FrameKind::GetMetadata {
            request_id,
            scope: Scope::Resource(terminal_id),
            key: phux_protocol::wire::frame::RESOURCE_AGENT_KEY.to_owned(),
        },
    )
    .await;
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for METADATA_VALUE request_id={request_id}"
        );
        let (_type_byte, frame) = timeout(remaining, recv_typed(stream)).await.unwrap();
        if let FrameKind::MetadataValue {
            request_id: got,
            value,
        } = frame
            && got == request_id
        {
            return value;
        }
    }
}

/// Poll `GET_METADATA phux.agent/v1` until the returned bytes contain
/// `needle`, or the deadline elapses.
///
/// The arbiter write reaches `state.metadata_set` off a side channel (the
/// per-pane `agent_state_sink` drain task, `spawn_agent_state_drain`) rather
/// than inline with the `COMMAND_RESULT` that triggered it — a `REPORT_
/// AGENT_STATE` or `APPEND_RESOURCE_OUTPUT` reply proves the *engine* saw
/// the evidence, not that the drain task has already written the record.
/// Polling is the honest wait for that, the same way `await_agent_state` in
/// `agents/agent_detect.rs` waits out the detector's own convergence.
async fn poll_metadata_until(
    stream: &mut UnixStream,
    request_id_base: u32,
    terminal_id: ResourceId,
    needle: &str,
) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    let mut attempt = 0u32;
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for phux.agent/v1 to contain {needle:?}"
        );
        if let Some(bytes) =
            get_metadata(stream, request_id_base + attempt, terminal_id.clone()).await
            && String::from_utf8_lossy(&bytes).contains(needle)
        {
            return bytes;
        }
        attempt = attempt.saturating_add(1);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn shutdown(
    stream: UnixStream,
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
    server_handle: tokio::task::JoinHandle<Result<(), phux_server::ServerError>>,
) {
    drop(stream);
    join_after_shutdown(shutdown_tx, server_handle).await;
}

// ---------------------------------------------------------------------------
// 1. Spawn + GET_STATE facet.
// ---------------------------------------------------------------------------

#[test]
fn spawn_registers_child_with_kind_parent_and_agent_facet() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut stream, shutdown_tx, server_handle) = connect_and_attach(&tmp).await;

        let parent = spawn_parent_terminal(&mut stream, 1).await;
        let session = match spawn_session(&mut stream, 2, parent.clone(), "claude").await {
            SpawnResult::Ok(id) => id,
            other => panic!("SPAWN_RESOURCE (session) failed: {other:?}"),
        };

        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 3,
                command: Command::GetState {
                    scope: StateScope::Server,
                },
            },
        )
        .await;
        let CommandResult::OkWith(CommandValue::State(snapshot)) =
            await_command_result(&mut stream, 3).await
        else {
            panic!("GET_STATE did not return a State snapshot");
        };
        let entry = snapshot
            .resources
            .iter()
            .find(|p| p.id == session)
            .expect("the session must appear in GET_STATE's pane list");
        assert_eq!(entry.kind, WireResourceKind::AgentSession);
        assert_eq!(entry.parent, Some(parent));
        let agent = entry
            .agent
            .as_ref()
            .expect("an AgentSession entry carries an agent facet");
        assert_eq!(agent.provider, "claude");

        shutdown(stream, shutdown_tx, server_handle).await;
    });
}

// ---------------------------------------------------------------------------
// 2. Append fans out live records to two subscribers.
// ---------------------------------------------------------------------------

#[test]
fn append_delivers_live_records_to_two_subscribers() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut owner, shutdown_tx, server_handle) = connect_and_attach(&tmp).await;
        let parent = spawn_parent_terminal(&mut owner, 1).await;
        let session = match spawn_session(&mut owner, 2, parent, "claude").await {
            SpawnResult::Ok(id) => id,
            other => panic!("spawn session failed: {other:?}"),
        };

        let mut watcher = connect_bare(&tmp).await;
        attach_terminal(&mut watcher, 100, session.clone()).await;

        let (result, owner_bytes) =
            append_and_collect(&mut owner, 3, session.clone(), "{\"type\":\"prompt\"}\n").await;
        assert!(
            matches!(result, CommandResult::OkWith(_)),
            "append must be accepted: {result:?}"
        );
        assert!(
            String::from_utf8_lossy(&owner_bytes).contains("\"type\":\"prompt\""),
            "the spawning (auto-subscribed) owner must also see its own live record: {owner_bytes:?}"
        );

        // Barrier: order the watcher's read behind whatever the broadcast
        // already queued for it.
        send_frame(&mut watcher, &state_barrier(101)).await;
        let watcher_bytes = drain_live_records_until(&mut watcher, &session, 101).await;
        assert!(
            String::from_utf8_lossy(&watcher_bytes).contains("\"type\":\"prompt\""),
            "the ATTACH_RESOURCE-only watcher must see the live record: {watcher_bytes:?}"
        );

        shutdown(owner, shutdown_tx, server_handle).await;
    });
}

// ---------------------------------------------------------------------------
// 3. Bootstrap replay.
// ---------------------------------------------------------------------------

/// Capture the envelope too: a multi-record append distinguishes the actual
/// record cut from the former documentation's separate frame counter.
async fn next_agent_output(stream: &mut UnixStream, agent: &ResourceId) -> (u64, Vec<u8>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let (
                _,
                FrameKind::ResourceOutput {
                    terminal_id,
                    seq,
                    bytes,
                    ..
                },
            ) = recv_typed(stream).await
                && &terminal_id == agent
            {
                return (seq, bytes.to_vec());
            }
        }
    })
    .await
    .expect("agent output must arrive")
}

fn parse_records(payload: &[u8]) -> Vec<serde_json::Value> {
    payload
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}

#[test]
fn multi_record_append_envelope_and_bootstrap_use_the_last_record_cut() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut owner, shutdown_tx, server_handle) = connect_and_attach(&tmp).await;
        let parent = spawn_parent_terminal(&mut owner, 1).await;
        let session = match spawn_session(&mut owner, 2, parent, "claude").await {
            SpawnResult::Ok(id) => id,
            other => panic!("spawn session failed: {other:?}"),
        };
        let mut watcher = connect_bare(&tmp).await;
        attach_terminal(&mut watcher, 100, session.clone()).await;
        let result = append(
            &mut owner,
            3,
            session.clone(),
            "{\"type\":\"prompt\"}\n{\"type\":\"ask\"}\n{\"type\":\"stop\"}\n",
        )
        .await;
        assert!(matches!(result, CommandResult::OkWith(_)));
        let (seq, payload) = next_agent_output(&mut watcher, &session).await;
        let records = parse_records(&payload);
        assert_eq!(
            seq, 3,
            "live envelope is final record seq, not append count 1"
        );
        assert_eq!(
            records
                .iter()
                .map(|record| record["seq"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        let mut replacement = connect_bare(&tmp).await;
        send_frame(
            &mut replacement,
            &FrameKind::Command {
                request_id: 101,
                command: Command::AttachResource {
                    terminal_id: session.clone(),
                },
            },
        )
        .await;
        let mut cut = None;
        let mut retained = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match recv_typed(&mut replacement).await.1 {
                    FrameKind::BootstrapBegin {
                        terminal_id,
                        base_seq,
                        ..
                    } if terminal_id == session => cut = Some(base_seq),
                    FrameKind::BootstrapChunk {
                        terminal_id,
                        payload,
                        ..
                    } if terminal_id == session => retained.extend_from_slice(&payload),
                    FrameKind::BootstrapReady { terminal_id, .. } if terminal_id == session => {
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(cut, Some(3));
        assert_eq!(parse_records(&retained), records);
        assert!(matches!(
            append(
                &mut owner,
                4,
                session.clone(),
                "{\"type\":\"prompt\"}\n{\"type\":\"ask\"}\n"
            )
            .await,
            CommandResult::OkWith(_)
        ));
        let (seq, payload) = next_agent_output(&mut replacement, &session).await;
        assert_eq!(seq, 5);
        assert_eq!(
            parse_records(&payload)
                .iter()
                .map(|record| record["seq"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![4, 5]
        );
        drop(watcher);
        drop(replacement);
        shutdown(owner, shutdown_tx, server_handle).await;
    });
}

#[test]
fn bootstrap_replays_retained_records_on_attach() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut owner, shutdown_tx, server_handle) = connect_and_attach(&tmp).await;
        let parent = spawn_parent_terminal(&mut owner, 1).await;
        let session = match spawn_session(&mut owner, 2, parent, "claude").await {
            SpawnResult::Ok(id) => id,
            other => panic!("spawn session failed: {other:?}"),
        };

        for (n, req) in (10..13).enumerate() {
            let record = format!("{{\"type\":\"tool_start\",\"data\":{{\"n\":{n}}}}}\n");
            let result = append(&mut owner, req, session.clone(), &record).await;
            assert!(
                matches!(result, CommandResult::OkWith(_)),
                "append {n} must be accepted: {result:?}"
            );
        }

        let mut late = connect_bare(&tmp).await;
        send_frame(
            &mut late,
            &FrameKind::Command {
                request_id: 200,
                command: Command::AttachResource {
                    terminal_id: session.clone(),
                },
            },
        )
        .await;

        let mut chunk_payload = Vec::new();
        let mut saw_begin_with_zero_grid = false;
        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(!remaining.is_zero(), "timed out collecting the bootstrap");
            let (_type_byte, frame) = timeout(remaining, recv_typed(&mut late)).await.unwrap();
            match frame {
                FrameKind::BootstrapBegin {
                    terminal_id,
                    cols,
                    rows,
                    profile,
                    ..
                } if terminal_id == session => {
                    assert_eq!(cols, 0, "an AgentSession's bootstrap grid is 0x0");
                    assert_eq!(rows, 0);
                    assert_eq!(
                        profile,
                        phux_protocol::caps::BootstrapStreamProfile::AgentEventsJsonlV1,
                        "must declare the AgentEventsJsonlV1 profile"
                    );
                    saw_begin_with_zero_grid = true;
                }
                FrameKind::BootstrapChunk {
                    terminal_id,
                    payload,
                    ..
                } if terminal_id == session => {
                    chunk_payload.extend_from_slice(&payload);
                }
                FrameKind::BootstrapReady { terminal_id, .. } if terminal_id == session => break,
                _ => {}
            }
        }
        assert!(
            saw_begin_with_zero_grid,
            "BOOTSTRAP_BEGIN for the session must have arrived"
        );
        let replayed = String::from_utf8_lossy(&chunk_payload);
        for n in 0..3 {
            assert!(
                replayed.contains(&format!("\"n\":{n}")),
                "the replay must carry every retained record (missing n={n}): {replayed}"
            );
        }

        shutdown(owner, shutdown_tx, server_handle).await;
    });
}

// ---------------------------------------------------------------------------
// 4. Ring overflow and its tombstone.
// ---------------------------------------------------------------------------

#[test]
fn ring_overflow_evicts_oldest_and_reports_the_toll() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut owner, shutdown_tx, server_handle) =
            connect_and_attach_with(&tmp, |cfg| cfg.agent_log_bytes = 200).await;
        let parent = spawn_parent_terminal(&mut owner, 1).await;
        let session = match spawn_session(&mut owner, 2, parent, "claude").await {
            SpawnResult::Ok(id) => id,
            other => panic!("spawn session failed: {other:?}"),
        };

        for req in 10..40 {
            let result = append(
                &mut owner,
                req,
                session.clone(),
                "{\"type\":\"tool_end\"}\n",
            )
            .await;
            assert!(
                matches!(result, CommandResult::OkWith(_)),
                "append must be accepted: {result:?}"
            );
        }

        let mut late = connect_bare(&tmp).await;
        send_frame(
            &mut late,
            &FrameKind::Command {
                request_id: 200,
                command: Command::AttachResource {
                    terminal_id: session.clone(),
                },
            },
        )
        .await;
        let mut base_seq = None;
        let mut chunk_count = 0u32;
        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(
                !remaining.is_zero(),
                "timed out collecting the overflowed bootstrap"
            );
            let (_type_byte, frame) = timeout(remaining, recv_typed(&mut late)).await.unwrap();
            match frame {
                FrameKind::BootstrapBegin {
                    terminal_id,
                    base_seq: seq,
                    ..
                } if terminal_id == session => {
                    base_seq = Some(seq);
                }
                FrameKind::BootstrapChunk { terminal_id, .. } if terminal_id == session => {
                    chunk_count = chunk_count.saturating_add(1);
                }
                FrameKind::BootstrapReady { terminal_id, .. } if terminal_id == session => break,
                _ => {}
            }
        }
        assert_eq!(
            base_seq,
            Some(30),
            "every append still took a sequence, even the evicted ones"
        );
        assert!(
            chunk_count > 0,
            "the ring must still retain (and replay) its newest records"
        );

        shutdown(owner, shutdown_tx, server_handle).await;
    });
}

// ---------------------------------------------------------------------------
// 5. Wrong-kind refusals.
// ---------------------------------------------------------------------------

#[test]
#[allow(clippy::too_many_lines)]
fn wrong_kind_refusals() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut owner, shutdown_tx, server_handle) = connect_and_attach(&tmp).await;
        let parent = spawn_parent_terminal(&mut owner, 1).await;
        let session = match spawn_session(&mut owner, 2, parent.clone(), "claude").await {
            SpawnResult::Ok(id) => id,
            other => panic!("spawn session failed: {other:?}"),
        };

        // (a) an input atom addressed at the session gets an uncorrelated
        // ERROR{WrongResourceKind} — it has no reply frame of its own.
        send_frame(
            &mut owner,
            &FrameKind::InputKey {
                terminal_id: session.clone(),
                event: phux_protocol::input::key::KeyEvent {
                    action: phux_protocol::input::key::KeyAction::Press,
                    key: phux_protocol::input::key::PhysicalKey::A,
                    mods: phux_protocol::input::key::ModSet::empty(),
                    text: Some("a".to_owned()),
                    composing: false,
                    consumed_mods: phux_protocol::input::key::ModSet::empty(),
                    unshifted_codepoint: None,
                },
            },
        )
        .await;
        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        let mut saw_wrong_kind_error = false;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let Ok((_type_byte, frame)) = timeout(remaining, recv_typed(&mut owner)).await else {
                break;
            };
            if let FrameKind::Error {
                code: ErrorCode::WrongResourceKind,
                ..
            } = frame
            {
                saw_wrong_kind_error = true;
                break;
            }
        }
        assert!(
            saw_wrong_kind_error,
            "INPUT_KEY on an AgentSession must push an uncorrelated ERROR{{WrongResourceKind}}"
        );

        // (b) GET_SCREEN.
        send_frame(
            &mut owner,
            &FrameKind::Command {
                request_id: 10,
                command: Command::GetScreen {
                    terminal_id: session.clone(),
                    request_scrollback: None,
                    cells: false,
                },
            },
        )
        .await;
        assert!(
            matches!(
                await_command_result(&mut owner, 10).await,
                CommandResult::Error {
                    code: ErrorCode::WrongResourceKind,
                    ..
                }
            ),
            "GET_SCREEN on an AgentSession must be WRONG_RESOURCE_KIND"
        );

        // (c) HISTORY_REQUEST: refused uncorrelated, per the FRAME_ACK
        // sibling rule — a session's bootstrap already replayed everything.
        send_frame(
            &mut owner,
            &FrameKind::HistoryRequest {
                terminal_id: session.clone(),
                stream_id: StreamId::new(1).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                cursor: bytes::Bytes::new(),
                max_bytes: 1024,
                max_rows: 10,
            },
        )
        .await;
        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        let mut saw_history_refusal = false;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let Ok((_type_byte, frame)) = timeout(remaining, recv_typed(&mut owner)).await else {
                break;
            };
            if let FrameKind::Error {
                code: ErrorCode::WrongResourceKind,
                ..
            } = frame
            {
                saw_history_refusal = true;
                break;
            }
        }
        assert!(
            saw_history_refusal,
            "HISTORY_REQUEST on an AgentSession stream must be refused WRONG_RESOURCE_KIND"
        );

        // (d) APPEND_RESOURCE_OUTPUT on a Terminal.
        let result = append(&mut owner, 11, parent.clone(), "{\"type\":\"prompt\"}\n").await;
        assert!(
            matches!(
                result,
                CommandResult::Error {
                    code: ErrorCode::WrongResourceKind,
                    ..
                }
            ),
            "APPEND_RESOURCE_OUTPUT on a Terminal must be WRONG_RESOURCE_KIND: {result:?}"
        );

        // (e) SIGNAL_TERMINAL on the session — a Terminal-facet command
        // this program specifically had to wire through the same helper.
        send_frame(
            &mut owner,
            &FrameKind::Command {
                request_id: 12,
                command: Command::SignalTerminal {
                    terminal_id: session.clone(),
                    signal: phux_protocol::wire::frame::TerminalSignal::Interrupt,
                },
            },
        )
        .await;
        assert!(
            matches!(
                await_command_result(&mut owner, 12).await,
                CommandResult::Error {
                    code: ErrorCode::WrongResourceKind,
                    ..
                }
            ),
            "SIGNAL_TERMINAL on an AgentSession must be WRONG_RESOURCE_KIND"
        );

        shutdown(owner, shutdown_tx, server_handle).await;
    });
}

// ---------------------------------------------------------------------------
// 5b. A child's lifecycle edges reach the parent's event subscribers.
// ---------------------------------------------------------------------------

/// Subscribe `stream` to each scope in `scopes`, then prove the server
/// installed them with a `GET_STATE` round trip.
///
/// `SUBSCRIBE_EVENTS` carries no request id and is answered with no frame,
/// so the barrier is the only thing that makes "subscribed before the spawn"
/// a fact rather than a race. The per-connection frame loop handles frames in
/// order, so a `COMMAND_RESULT` for a request sent after the subscribes is
/// proof they already ran.
async fn subscribe_events(stream: &mut UnixStream, request_id: u32, scopes: &[Option<ResourceId>]) {
    for terminal in scopes {
        send_frame(
            stream,
            &FrameKind::SubscribeEvents {
                terminal: terminal.clone(),
            },
        )
        .await;
    }
    send_frame(stream, &state_barrier(request_id)).await;
    assert!(
        !matches!(
            await_command_result(stream, request_id).await,
            CommandResult::Error { .. }
        ),
        "the subscribe barrier must succeed",
    );
}

/// Count the `pane_spawned` and `pane_closed` EVENT frames naming `wanted`
/// that arrive before the `COMMAND_RESULT` for `barrier_request_id`.
///
/// Counting to a barrier rather than returning on the first match is what
/// makes the "once each" half provable: a fan-out that put a client on the
/// list twice would deliver two copies, and a test that stopped at the first
/// would never see the second.
async fn count_child_events(
    stream: &mut UnixStream,
    wanted: &ResourceId,
    barrier_request_id: u32,
) -> (usize, usize) {
    let mut spawned = 0_usize;
    let mut closed = 0_usize;
    loop {
        let (_type_byte, frame) = recv_typed(stream).await;
        match frame {
            FrameKind::Event {
                terminal: Some(id),
                event,
            } if &id == wanted => match event {
                phux_protocol::wire::frame::AgentEvent::ResourceSpawned { kind, parent } => {
                    assert_eq!(
                        kind,
                        WireResourceKind::AgentSession,
                        "the announcement names the child's kind"
                    );
                    assert!(parent.is_some(), "and the pane it lives in");
                    spawned = spawned.saturating_add(1);
                }
                phux_protocol::wire::frame::AgentEvent::ResourceClosed { .. } => {
                    closed = closed.saturating_add(1);
                }
                _ => {}
            },
            FrameKind::CommandResult { request_id, .. } if request_id == barrier_request_id => {
                return (spawned, closed);
            }
            _ => {}
        }
    }
}

/// A watcher scoped to the PANE learns that a session opened inside it, and
/// that it closed again — the two edges of a child's life (ADR-0104 §2).
///
/// The event names the child, because the child is the resource that
/// appeared; its audience is the parent's, because a consumer cannot
/// subscribe to an id it is being told about for the first time. Before this,
/// `phux watch @pane` saw neither edge: the fan-out matched the envelope id
/// alone, so a `pane_spawned` addressed to the session reached only clients
/// already watching the session — of which, by construction, there are none.
///
/// The watcher holds BOTH scopes so the delivery is also pinned as once
/// each: two matching scopes on one client are one subscription entry and
/// must produce one frame, not two.
#[test]
fn a_childs_spawn_and_close_reach_the_parents_event_watchers() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut owner, shutdown_tx, server_handle) = connect_and_attach(&tmp).await;
        let parent = spawn_parent_terminal(&mut owner, 1).await;

        // A pure `watch` client: never attached, subscribed to the pane and
        // server-wide, exactly as `phux watch @pane` is.
        let mut watcher = connect_bare(&tmp).await;
        subscribe_events(&mut watcher, 100, &[Some(parent.clone()), None]).await;

        let session = match spawn_session(&mut owner, 2, parent.clone(), "claude").await {
            SpawnResult::Ok(id) => id,
            other => panic!("spawn session failed: {other:?}"),
        };
        assert_ne!(session, parent, "the session is its own resource");

        send_frame(
            &mut owner,
            &FrameKind::Command {
                request_id: 3,
                command: Command::KillResource {
                    terminal_id: session.clone(),
                },
            },
        )
        .await;
        assert!(matches!(
            await_command_result(&mut owner, 3).await,
            CommandResult::Ok
        ));

        // The barrier runs on the watcher's own connection, after both edges
        // were emitted on the owner's, so anything the fan-out sent has
        // already been queued ahead of the result being counted to.
        send_frame(&mut watcher, &state_barrier(101)).await;
        let (spawned, closed) = count_child_events(&mut watcher, &session, 101).await;
        assert_eq!(
            spawned, 1,
            "the parent's watcher must learn a session opened inside the pane, once",
        );
        assert_eq!(closed, 1, "and that it closed again, once");

        shutdown(owner, shutdown_tx, server_handle).await;
    });
}

// ---------------------------------------------------------------------------
// 6. Kill parent cascades the child with ParentClosed.
// ---------------------------------------------------------------------------

#[test]
fn kill_parent_cascades_child_with_parent_closed() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut owner, shutdown_tx, server_handle) = connect_and_attach(&tmp).await;
        let parent = spawn_parent_terminal(&mut owner, 1).await;
        let session = match spawn_session(&mut owner, 2, parent.clone(), "claude").await {
            SpawnResult::Ok(id) => id,
            other => panic!("spawn session failed: {other:?}"),
        };

        let mut watcher = connect_bare(&tmp).await;
        attach_terminal(&mut watcher, 100, session.clone()).await;

        send_frame(
            &mut owner,
            &FrameKind::Command {
                request_id: 3,
                command: Command::KillResource {
                    terminal_id: parent.clone(),
                },
            },
        )
        .await;
        assert!(matches!(
            await_command_result(&mut owner, 3).await,
            CommandResult::Ok
        ));

        let watcher_reason = await_terminal_closed_reason(&mut watcher, &session).await;
        assert_eq!(
            watcher_reason,
            phux_protocol::wire::frame::CloseReason::ParentClosed,
            "the cascaded child's RESOURCE_CLOSED must carry ParentClosed"
        );
        let owner_reasons =
            collect_closed_reasons(&mut owner, &[parent.clone(), session.clone()]).await;
        assert_eq!(
            owner_reasons[&session],
            phux_protocol::wire::frame::CloseReason::ParentClosed
        );
        assert_eq!(
            owner_reasons[&parent],
            phux_protocol::wire::frame::CloseReason::Killed,
            "the killed parent's own RESOURCE_CLOSED must carry Killed"
        );

        shutdown(owner, shutdown_tx, server_handle).await;
    });
}

// ---------------------------------------------------------------------------
// 7. PTY exit cascades.
// ---------------------------------------------------------------------------

#[test]
fn pty_exit_cascades_to_the_child() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut owner, shutdown_tx, server_handle) = connect_and_attach(&tmp).await;
        let parent = spawn_parent_terminal_with(&mut owner, 1, exits_on_input_argv(5)).await;
        let session = match spawn_session(&mut owner, 2, parent.clone(), "claude").await {
            SpawnResult::Ok(id) => id,
            other => panic!("spawn session failed: {other:?}"),
        };

        // Trip the shell's `exit 5` — the ordering barrier is unnecessary
        // here (the parent hasn't left yet, there is nothing to race), the
        // wait below is on the frames the exit itself produces.
        send_frame(
            &mut owner,
            &FrameKind::InputKey {
                terminal_id: parent.clone(),
                event: phux_protocol::input::key::KeyEvent {
                    action: phux_protocol::input::key::KeyAction::Press,
                    key: phux_protocol::input::key::PhysicalKey::Enter,
                    mods: phux_protocol::input::key::ModSet::empty(),
                    text: Some("\n".to_owned()),
                    composing: false,
                    consumed_mods: phux_protocol::input::key::ModSet::empty(),
                    unshifted_codepoint: None,
                },
            },
        )
        .await;

        let reasons = collect_closed_reasons(&mut owner, &[parent.clone(), session.clone()]).await;
        assert_eq!(
            reasons[&session],
            phux_protocol::wire::frame::CloseReason::ParentClosed,
            "a session whose parent's PTY exited must close with ParentClosed"
        );
        assert_eq!(
            reasons[&parent],
            phux_protocol::wire::frame::CloseReason::Exited,
            "the parent's own exit must be reported Exited"
        );

        shutdown(owner, shutdown_tx, server_handle).await;
    });
}

// ---------------------------------------------------------------------------
// 8. KILL_RESOURCES over a mixed set.
// ---------------------------------------------------------------------------

#[test]
fn kill_terminals_mixed_set_closes_once_each() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut owner, shutdown_tx, server_handle) = connect_and_attach(&tmp).await;
        let parent = spawn_parent_terminal(&mut owner, 1).await;
        let session = match spawn_session(&mut owner, 2, parent.clone(), "claude").await {
            SpawnResult::Ok(id) => id,
            other => panic!("spawn session failed: {other:?}"),
        };
        let unrelated = spawn_parent_terminal(&mut owner, 3).await;

        // Name the parent AND the child explicitly: the child would cascade
        // anyway, so this proves it is not double-closed.
        send_frame(
            &mut owner,
            &FrameKind::Command {
                request_id: 4,
                command: Command::KillResources {
                    ids: vec![parent.clone(), session.clone(), unrelated.clone()],
                },
            },
        )
        .await;
        assert!(matches!(
            await_command_result(&mut owner, 4).await,
            CommandResult::Ok
        ));

        // Phase 1: wait (on the KILL_RESOURCES command's own timeout, no
        // premature ordering barrier — the exit watchers that emit these
        // frames are separate tasks that have not necessarily run yet the
        // instant KILL_RESOURCES's COMMAND_RESULT arrives) until every
        // target has closed at least once. Counted, not just observed: the
        // three closes race each other on the wire, so this is one pass
        // rather than three sequential waits that could silently consume
        // (and lose) a different target's frame arriving out of order.
        let targets = [parent.clone(), session.clone(), unrelated.clone()];
        let mut seen: std::collections::HashMap<ResourceId, u32> = std::collections::HashMap::new();
        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        while !targets.iter().all(|t| seen.contains_key(t)) {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(
                !remaining.is_zero(),
                "timed out collecting KILL_RESOURCES closes: {seen:?}"
            );
            let (_type_byte, frame) = timeout(remaining, recv_typed(&mut owner)).await.unwrap();
            if let FrameKind::ResourceClosed { terminal_id, .. } = frame {
                *seen.entry(terminal_id).or_insert(0) += 1;
            }
        }
        // Phase 2: NOW a barrier is meaningful — everything the cascade
        // emitted is already queued ahead of its reply, so a further close
        // for any target would show up as a second count before the
        // barrier's COMMAND_RESULT does.
        send_frame(&mut owner, &state_barrier(5)).await;
        loop {
            let (_type_byte, frame) = timeout(WIRE_RECV_TIMEOUT, recv_typed(&mut owner))
                .await
                .unwrap();
            match frame {
                FrameKind::ResourceClosed { terminal_id, .. } => {
                    *seen.entry(terminal_id).or_insert(0) += 1;
                }
                FrameKind::CommandResult { request_id: 5, .. } => break,
                _ => {}
            }
        }
        for victim in &targets {
            assert_eq!(
                seen.get(victim).copied().unwrap_or(0),
                1,
                "each victim must close exactly once: {victim:?} in {seen:?}"
            );
        }
        assert_eq!(
            seen.len(),
            3,
            "no other resource should have closed: {seen:?}"
        );

        shutdown(owner, shutdown_tx, server_handle).await;
    });
}

// ---------------------------------------------------------------------------
// 9. Spawn with an unknown parent, and with a session parent.
// ---------------------------------------------------------------------------

#[test]
fn spawn_with_unknown_or_session_parent_earns_typed_refusals() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let (mut owner, shutdown_tx, server_handle) = connect_and_attach(&tmp).await;

        let unknown_parent = ResourceId::local(999_999);
        let result = spawn_session(&mut owner, 1, unknown_parent, "claude").await;
        assert!(
            matches!(result, SpawnResult::Err(SpawnError::ParentNotFound)),
            "an unknown parent must earn ParentNotFound: {result:?}"
        );

        let parent = spawn_parent_terminal(&mut owner, 2).await;
        let session = match spawn_session(&mut owner, 3, parent, "claude").await {
            SpawnResult::Ok(id) => id,
            other => panic!("spawn session failed: {other:?}"),
        };
        let result = spawn_session(&mut owner, 4, session, "claude").await;
        assert!(
            matches!(result, SpawnResult::Err(SpawnError::ParentKindMismatch)),
            "an AgentSession parent must earn ParentKindMismatch: {result:?}"
        );

        shutdown(owner, shutdown_tx, server_handle).await;
    });
}

// ---------------------------------------------------------------------------
// 10. phux.agent/v1 projection: prompt then stop.
// ---------------------------------------------------------------------------

#[test]
fn stream_evidence_projects_onto_the_parents_agent_record() {
    shorten_agent_detect_timers();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let claude = write_fake_claude(tmp.path());
        let (mut owner, shutdown_tx, server_handle) = connect_and_attach(&tmp).await;
        let parent =
            spawn_parent_terminal_with(&mut owner, 1, vec![claude.to_string_lossy().into_owned()])
                .await;
        let session = match spawn_session(&mut owner, 2, parent.clone(), "claude").await {
            SpawnResult::Ok(id) => id,
            other => panic!("spawn session failed: {other:?}"),
        };

        let result = append(&mut owner, 3, session.clone(), "{\"type\":\"prompt\"}\n").await;
        assert!(
            matches!(result, CommandResult::OkWith(_)),
            "append must be accepted: {result:?}"
        );
        poll_metadata_until(&mut owner, 100, parent.clone(), "\"state\":\"working\"").await;

        let result = append(&mut owner, 5, session, "{\"type\":\"stop\"}\n").await;
        assert!(
            matches!(result, CommandResult::OkWith(_)),
            "append must be accepted: {result:?}"
        );
        poll_metadata_until(&mut owner, 200, parent, "\"state\":\"done\"").await;

        shutdown(owner, shutdown_tx, server_handle).await;
    });
}

// ---------------------------------------------------------------------------
// 11. REPORT_AGENT_STATE with a live child lands as a state record.
// ---------------------------------------------------------------------------

#[test]
fn report_agent_state_with_a_live_child_lands_as_a_stream_record() {
    shorten_agent_detect_timers();
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let claude = write_fake_claude(tmp.path());
        let (mut owner, shutdown_tx, server_handle) = connect_and_attach(&tmp).await;
        let parent =
            spawn_parent_terminal_with(&mut owner, 1, vec![claude.to_string_lossy().into_owned()])
                .await;
        let session = match spawn_session(&mut owner, 2, parent.clone(), "claude").await {
            SpawnResult::Ok(id) => id,
            other => panic!("spawn session failed: {other:?}"),
        };

        let mut watcher = connect_bare(&tmp).await;
        attach_terminal(&mut watcher, 100, session.clone()).await;

        send_frame(
            &mut owner,
            &FrameKind::Command {
                request_id: 3,
                command: Command::ReportAgentState {
                    terminal_id: parent.clone(),
                    state: ReportedAgentState::Blocked,
                },
            },
        )
        .await;
        assert!(matches!(
            await_command_result(&mut owner, 3).await,
            CommandResult::Ok
        ));

        send_frame(&mut watcher, &state_barrier(101)).await;
        let bytes = drain_live_records_until(&mut watcher, &session, 101).await;
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("\"type\":\"state\"") && text.contains("\"state\":\"blocked\""),
            "REPORT_AGENT_STATE with a live child must synthesize a state record on its stream: {text}"
        );

        poll_metadata_until(&mut owner, 100, parent, "\"state\":\"blocked\"").await;

        shutdown(owner, shutdown_tx, server_handle).await;
    });
}
