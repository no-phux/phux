//! The `AgentSession` resource kind over the production dispatch loop
//! (ADR-0103, ADR-0104): spawn and facet, append fan-out and bootstrap
//! replay, ring overflow, wrong-kind refusals, lifecycle edges and the
//! parent-close cascade, and the `phux.agent/v1` projection. The actor engine
//! itself is unit-tested in `src/resource/agent_session/`.

use std::collections::HashMap;

use phux_protocol::caps::BootstrapStreamProfile;
use phux_protocol::ids::{ResourceId, ResourceKind};
use phux_protocol::wire::frame::{
    AgentEvent, CloseReason, Command, CommandResult, ErrorCode, FrameKind, ReportedAgentState,
    Scope, SpawnError, SpawnResource, SpawnResult, TerminalSignal,
};
use phux_server_testkit::{Spawn, assert_command_error, command, send_frame, spawn_resource};
use tokio::net::UnixStream;

use crate::common::{
    FOREVER, bootstrap, demo as start, get_metadata, next_output, send_command, sh, spawned, state,
    subscribe, wait_frame,
};

#[path = "agent_session/family.rs"]
mod family;
#[path = "agent_session/frames.rs"]
mod frames;
#[path = "agent_session/resume.rs"]
mod resume;

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
    send_command(stream, request_id, attach).await;
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
    send_command(owner, request_id, append).await;
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

/// Any local connection may feed a session, not only its opener: `phux agent
/// session open` and each `phux agent emit` are separate processes
/// (phux-jqlh0.8).
#[test]
fn a_local_connection_that_did_not_open_the_session_may_append() {
    phux_server_testkit::run_local(async {
        let (server, mut opener) = start(|_| {}).await;
        let (_parent, session) = parent_and_session(&mut opener, FOREVER).await;
        drop(opener);
        let mut emitter = server.connect().await;
        append(&mut emitter, 1, &session, "{\"type\":\"prompt\"}\n").await;
        server.stop().await;
    });
}
