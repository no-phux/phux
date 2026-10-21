mod agent_stream;
mod attach;
mod callbacks;
mod clear_presentation;
mod grid;
mod handshake;
mod newer_server;
mod resource_discovery;
mod status_effects;
mod terminal_modes;

use super::*;
use crate::c::test_support::*;
use phux_protocol::caps::ServerCapabilities;
use phux_protocol::wire::frame::DetachReason;

fn feed_complete_bootstrap(
    client: *mut PhuxClient,
    terminal_id: &phux_protocol::ResourceId,
    payload: &[u8],
) {
    feed_bootstrap(client, terminal_id, (7, 1), (40, 12), payload);
}

fn client_with_searchable_scrollback() -> *mut PhuxClient {
    let client = attaching(&[], 7);
    let terminal = phux_protocol::ResourceId::local(1);
    let snapshot = single_terminal_snapshot(terminal.clone(), 40, 12);
    assert_eq!(
        feed(client, &attached_frame(7, snapshot)),
        PhuxClientResult::Ok,
    );
    feed_complete_bootstrap(client, &terminal,
            b"older\r\nOFFSCREEN MATCH\r\n02\r\n03\r\n04\r\n05\r\n06\r\n07\r\n08\r\n09\r\n10\r\n11\r\n12\r\n13\r\n14\r\n15\r\n16\r\n17\r\n18\r\nLIVE TAIL");
    assert_eq!(
        feed(client, &FrameKind::AttachReady { attach_id: 7 }),
        PhuxClientResult::Ok,
    );
    client
}

fn effect_bytes(effect: &PhuxClientEffect) -> &[u8] {
    if effect.bytes.len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(effect.bytes.data, effect.bytes.len) }
    }
}

fn record(seq: u64, kind: &str, data: &str) -> String {
    format!(
        "{{\"seq\":{seq},\"ts_ms\":{},\"type\":\"{kind}\",\"data\":{data}}}\n",
        seq * 10
    )
}

fn effect_at(client: *mut PhuxClient, index: usize) -> PhuxClientEffect {
    let mut effect = PhuxClientEffect::default();
    assert_eq!(
        unsafe { phux_client_effect_get(client, index, &raw mut effect) },
        PhuxClientResult::Ok
    );
    effect
}

fn span_bytes(span: PhuxBytes) -> &'static [u8] {
    if span.len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(span.data, span.len) }
    }
}

/// A focused session holding one terminal and one agent session bound to it.
fn mixed_kind_snapshot(
    terminal: &phux_protocol::ResourceId,
    agent: &phux_protocol::ResourceId,
) -> phux_protocol::wire::info::SessionSnapshot {
    let session_id = SessionId::new(1);
    let window_id = phux_protocol::WindowId::new(10);
    phux_protocol::wire::info::SessionSnapshot::new(session_id, window_id, terminal.clone())
        .with_sessions(vec![phux_protocol::wire::info::SessionInfo::new(
            session_id, "working",
        )])
        .with_windows(vec![phux_protocol::wire::info::WindowInfo::new(
            window_id, session_id, "working",
        )])
        .with_resources(vec![
            phux_protocol::wire::info::ResourceInfo::new(terminal.clone(), window_id, 80, 24),
            phux_protocol::wire::info::ResourceInfo::new(
                agent.clone(),
                phux_protocol::WindowId::new(0),
                0,
                0,
            )
            .with_kind(ResourceKind::AgentSession)
            .with_parent(Some(terminal.clone()))
            .with_agent(Some(
                phux_protocol::AgentFacet::new("claude", "working")
                    .with_native_id(Some("s-1".to_owned())),
            )),
        ])
}

const MIXED_TERMINAL: u32 = 30;
const MIXED_AGENT: u32 = 31;

/// A client attached to [`mixed_kind_snapshot`] with the terminal READY and
/// the attach complete; the agent session is declared but has no stream yet.
fn attached_mixed_client() -> *mut PhuxClient {
    let terminal = phux_protocol::ResourceId::local(MIXED_TERMINAL);
    let agent = phux_protocol::ResourceId::local(MIXED_AGENT);
    attached_resource_client(mixed_kind_snapshot(&terminal, &agent))
}

fn attached_resource_client(
    snapshot: phux_protocol::wire::info::SessionSnapshot,
) -> *mut PhuxClient {
    let client = attaching(&[], 7);
    complete_resource_attach(client, snapshot);
    client
}

/// Answers ATTACH 7 with `snapshot`, bootstraps its focused terminal,
/// releases the barrier, and clears the resulting effects.
fn complete_resource_attach(
    client: *mut PhuxClient,
    snapshot: phux_protocol::wire::info::SessionSnapshot,
) {
    let terminal = snapshot.focused_resource.clone();
    assert_eq!(
        feed(client, &attached_frame(7, snapshot)),
        PhuxClientResult::Ok
    );
    // A real server mints a fresh generation for every transport attach.
    // SAFETY: this fixture exclusively owns the client.
    let generation = unsafe { (*client).inner.control().connection_epoch() };
    feed_bootstrap(client, &terminal, (1, generation), (80, 24), b"");
    assert_eq!(
        feed(client, &FrameKind::AttachReady { attach_id: 7 }),
        PhuxClientResult::Ok
    );
    assert_eq!(
        unsafe { phux_client_effect_clear(client) },
        PhuxClientResult::Ok
    );
}

const AGENT_STREAM: u64 = 4;
const AGENT_BOOTSTRAP: u64 = 5;

/// Opens generation (4, 5) on the agent stream with two retained records
/// and one live record: the retained backlog publishes once at READY,
/// then live records follow.
fn open_agent_stream(client: *mut PhuxClient) {
    let agent = phux_protocol::ResourceId::local(MIXED_AGENT);
    let stream_id = phux_protocol::StreamId::new(AGENT_STREAM).expect("stream");
    let bootstrap_id = phux_protocol::BootstrapId::new(AGENT_BOOTSTRAP).expect("bootstrap");
    let retained = format!(
        "{}{}",
        record(1, "session_start", r#"{"provider":"claude"}"#),
        record(2, "prompt", r#"{"length":3}"#)
    );
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: agent.clone(),
            stream_id,
            bootstrap_id,
            profile: phux_protocol::BootstrapStreamProfile::AgentEventsJsonlV1,
            cols: 0,
            rows: 0,
            base_seq: 2,
        },
        FrameKind::BootstrapChunk {
            terminal_id: agent.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: bytes::Bytes::from(retained),
        },
        FrameKind::BootstrapReady {
            terminal_id: agent.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
        FrameKind::ResourceOutput {
            terminal_id: agent.clone(),
            stream_id,
            bootstrap_id,
            seq: 3,
            bytes: bytes::Bytes::from(record(3, "ask", "{}")),
        },
    ] {
        assert_eq!(feed(client, &frame), PhuxClientResult::Ok);
    }
    assert!(
        unsafe { (*client).inner.projection(&agent) }.is_none(),
        "an agent stream never publishes a terminal replica"
    );
}
