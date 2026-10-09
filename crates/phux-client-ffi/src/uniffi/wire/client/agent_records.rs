//! `AgentSession` streams through the `UniFFI` lane: the runtime subscribes
//! them, and their records reach a phone as `WireEvent::AgentRecords`.

#![allow(clippy::expect_used, reason = "test assertions")]
#![allow(clippy::panic, reason = "test assertions")]

use phux_client_runtime::control::{ControlOptions, ControlPlane};
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, Layer, LayerSet, ServerCapabilities,
    ServerFeatureSet,
};
use phux_protocol::ids::{BootstrapId, ClientId, SessionId, StreamId, WindowId};
use phux_protocol::wire::frame::{CloseReason, Command, CommandResult, FrameKind};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
use phux_protocol::{AgentFacet, PROTOCOL_VERSION, ResourceKind};

use super::*;

const TERMINAL: u32 = 30;
const AGENT: u32 = 31;

fn terminal() -> ResourceId {
    ResourceId::local(TERMINAL)
}

fn agent() -> ResourceId {
    ResourceId::local(AGENT)
}

/// One terminal, with an agent session bound to it when `with_agent`.
fn snapshot(with_agent: bool) -> SessionSnapshot {
    let session = SessionId::new(1);
    let window = WindowId::new(10);
    let mut resources = vec![ResourceInfo::new(terminal(), window, 80, 24)];
    if with_agent {
        resources.push(
            ResourceInfo::new(agent(), WindowId::new(0), 0, 0)
                .with_kind(ResourceKind::AgentSession)
                .with_parent(Some(terminal()))
                .with_agent(Some(
                    AgentFacet::new("claude", "working").with_native_id(Some("s-1".to_owned())),
                )),
        );
    }
    SessionSnapshot::new(session, window, terminal())
        .with_sessions(vec![SessionInfo::new(session, "working")])
        .with_windows(vec![WindowInfo::new(window, session, "working")])
        .with_resources(resources)
}

fn record(seq: u64, kind: &str, data: &str) -> String {
    format!(
        "{{\"seq\":{seq},\"ts_ms\":{},\"type\":\"{kind}\",\"data\":{data}}}\n",
        seq * 10
    )
}

fn outgoing(client: &Client) -> Vec<FrameKind> {
    client
        .take_outbound()
        .iter()
        .map(|bytes| FrameKind::decode(bytes).expect("decodes").0)
        .collect()
}

/// The one `ATTACH_RESOURCE` the runtime sent for the agent session, and
/// nothing else for it: a stream has no geometry to resize.
fn agent_subscription(frames: &[FrameKind]) -> u32 {
    let mut subscriptions = frames.iter().filter_map(|frame| match frame {
        FrameKind::Command {
            request_id,
            command: Command::AttachResource { terminal_id, .. },
        } if *terminal_id == agent() => Some(*request_id),
        _ => None,
    });
    let request_id = subscriptions.next().expect("agent session subscribed");
    assert!(subscriptions.next().is_none(), "subscribed once");
    assert!(
        !frames.iter().any(|frame| matches!(
            frame,
            FrameKind::ResizeTerminal { terminal_id, .. } if *terminal_id == agent()
        )),
        "an agent stream is never resized"
    );
    request_id
}

/// A `RemoteClient` over a sans-IO runtime client configured as `connect`
/// configures it, attached to `snapshot` with the terminal bootstrapped.
/// Returns the frames the attach queued.
fn attached(snapshot: SessionSnapshot) -> (Arc<RemoteClient>, Vec<FrameKind>) {
    let client = Runtime::embedded(ControlOptions {
        attach: Some(AttachTarget::ByName("working".into())),
        viewport: (80, 24),
        subscribe_agent_sessions: true,
        ..ControlOptions::default()
    });
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client
        .feed(FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new()
                .with_layers(LayerSet::with(&[Layer::L3]))
                .with_features(ServerFeatureSet::new()),
            server_id: vec![0xAB; 16],
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::default(),
        })
        .expect("HELLO_OK");
    let attach_id = outgoing(&client)
        .into_iter()
        .find_map(|frame| match frame {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .expect("ATTACH queued");
    client
        .feed(FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id: ClientId::new(1),
        })
        .expect("ATTACHED");
    let queued = outgoing(&client);
    let stream_id = StreamId::new(1).expect("nonzero");
    let bootstrap_id = BootstrapId::new(1).expect("nonzero");
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
        FrameKind::AttachReady { attach_id },
    ] {
        client.feed(frame).expect("terminal bootstrap");
    }
    let _ = client.take_outbound();
    let remote = RemoteClient::new("unused".into(), 80, 24, None, None);
    *remote.client.lock().unwrap() = Some(client);
    let _ = remote.take_events();
    (remote, queued)
}

/// Open generation (4, 5) on the agent stream: two retained records, then
/// one live record, then the subscription's acknowledgement (the reference
/// server bootstraps before it answers).
fn open_agent_stream(remote: &RemoteClient, request_id: u32) {
    let client = remote.runtime_client().expect("connected");
    let stream_id = StreamId::new(4).expect("stream");
    let bootstrap_id = BootstrapId::new(5).expect("bootstrap");
    let retained = format!(
        "{}{}",
        record(1, "session_start", r#"{"provider":"claude"}"#),
        record(2, "prompt", r#"{"length":3}"#)
    );
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: agent(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::AgentEventsJsonlV1,
            cols: 0,
            rows: 0,
            base_seq: 2,
        },
        FrameKind::BootstrapChunk {
            terminal_id: agent(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: retained.into_bytes().into(),
        },
        FrameKind::BootstrapReady {
            terminal_id: agent(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
        FrameKind::ResourceOutput {
            terminal_id: agent(),
            stream_id,
            bootstrap_id,
            seq: 3,
            bytes: record(3, "ask", "{}").into_bytes().into(),
        },
        FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        },
    ] {
        client.feed(frame).expect("agent stream frame");
    }
    assert!(
        client.acquire(&agent()).is_none(),
        "an agent stream never publishes a terminal replica"
    );
}

fn agent_events(events: Vec<WireEvent>) -> Vec<WireEvent> {
    events
        .into_iter()
        .filter(|event| matches!(event, WireEvent::AgentRecords { .. }))
        .collect()
}

fn lines(jsonl: &str) -> Vec<serde_json::Value> {
    jsonl
        .lines()
        .map(|line| serde_json::from_str(line).expect("record line is JSON"))
        .collect()
}

fn refresh(remote: &RemoteClient, snapshot: SessionSnapshot) -> Vec<FrameKind> {
    let client = remote.runtime_client().expect("connected");
    let request_id = client.refresh_topology().expect("topology read");
    let _ = client.take_outbound();
    client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(phux_protocol::wire::frame::CommandValue::State(
                snapshot,
            )),
        })
        .expect("GET_STATE reply");
    outgoing(&client)
}

#[test]
fn agent_stream_frames_surface_as_agent_records_events() {
    let (remote, queued) = attached(snapshot(true));
    let request_id = agent_subscription(&queued);
    open_agent_stream(&remote, request_id);

    let events = agent_events(remote.take_events());
    assert_eq!(events.len(), 2, "{events:?}");
    let WireEvent::AgentRecords {
        agent_session_id,
        parent_terminal_id,
        provider,
        native_id,
        kind,
        seq,
        jsonl,
    } = &events[0]
    else {
        panic!("agent records");
    };
    assert_eq!(agent_session_id, "local:31");
    assert_eq!(parent_terminal_id.as_deref(), Some("local:30"));
    assert_eq!(provider.as_deref(), Some("claude"));
    assert_eq!(native_id.as_deref(), Some("s-1"));
    assert_eq!((*kind, *seq), (AgentRecordsKind::Retained, 2));
    let retained = lines(jsonl);
    assert_eq!(retained.len(), 2);
    assert_eq!(retained[0]["type"], "session_start");
    assert_eq!(retained[0]["data"]["provider"], "claude");
    assert_eq!(retained[1]["seq"], 2);
    assert_eq!(retained[1]["ts_ms"], 20);

    let WireEvent::AgentRecords {
        parent_terminal_id,
        kind,
        seq,
        jsonl,
        ..
    } = &events[1]
    else {
        panic!("agent records");
    };
    assert_eq!(parent_terminal_id.as_deref(), Some("local:30"));
    assert_eq!((*kind, *seq), (AgentRecordsKind::Live, 3));
    assert_eq!(lines(jsonl)[0]["type"], "ask");

    // A close ends the stream as itself; the pane's vocabulary never sees
    // it, however many times the server says so.
    let client = remote.runtime_client().expect("connected");
    for _ in 0..2 {
        client
            .feed(FrameKind::ResourceClosed {
                terminal_id: agent(),
                exit_status: None,
                reason: CloseReason::ParentClosed,
                signal: None,
            })
            .expect("RESOURCE_CLOSED");
    }
    let events = remote.take_events();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, WireEvent::PaneClosed { .. })),
        "{events:?}"
    );
    assert_eq!(
        agent_events(events),
        vec![WireEvent::AgentRecords {
            agent_session_id: "local:31".into(),
            parent_terminal_id: Some("local:30".into()),
            provider: Some("claude".into()),
            native_id: Some("s-1".into()),
            kind: AgentRecordsKind::Closed,
            seq: 0,
            jsonl: String::new(),
        }]
    );
}

#[test]
fn workspace_refresh_subscribes_agent_sessions_and_closes_removed_ones() {
    let (remote, queued) = attached(snapshot(false));
    assert!(
        !queued.iter().any(|frame| matches!(
            frame,
            FrameKind::Command {
                command: Command::AttachResource { .. },
                ..
            }
        )),
        "nothing to subscribe yet"
    );

    // An agent session opened in the pane is announced; the runtime reads
    // the catalog, and the read subscribes it.
    let client = remote.runtime_client().expect("connected");
    client
        .feed(FrameKind::Event {
            terminal: Some(agent()),
            event: phux_protocol::wire::frame::AgentEvent::ResourceSpawned {
                kind: ResourceKind::AgentSession,
                parent: Some(terminal()),
            },
            stamp: None,
        })
        .expect("spawn announcement");
    assert!(
        outgoing(&client).iter().any(|frame| matches!(
            frame,
            FrameKind::Command {
                command: Command::GetState { .. },
                ..
            }
        )),
        "an announced agent session reads the catalog"
    );
    let request_id = agent_subscription(&refresh(&remote, snapshot(true)));
    open_agent_stream(&remote, request_id);
    let events = agent_events(remote.take_events());
    assert_eq!(events.len(), 2);

    assert!(
        refresh(&remote, snapshot(true)).is_empty(),
        "an unchanged catalog keeps the subscription"
    );
    assert!(agent_events(remote.take_events()).is_empty());

    assert!(refresh(&remote, snapshot(false)).is_empty());
    let events = agent_events(remote.take_events());
    assert!(
        matches!(
            events.as_slice(),
            [WireEvent::AgentRecords {
                kind: AgentRecordsKind::Closed,
                parent_terminal_id: Some(parent),
                ..
            }] if parent == "local:30"
        ),
        "{events:?}"
    );
    assert!(
        refresh(&remote, snapshot(true)).is_empty(),
        "a closed session is never subscribed again"
    );
}

#[test]
fn a_refused_subscription_is_retried_by_the_next_catalog_read() {
    let (remote, queued) = attached(snapshot(true));
    let request_id = agent_subscription(&queued);
    let client = remote.runtime_client().expect("connected");
    client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Error {
                code: phux_protocol::wire::frame::ErrorCode::InvalidCommand,
                message: "busy".into(),
            },
        })
        .expect("refusal");
    assert!(agent_events(remote.take_events()).is_empty());
    agent_subscription(&refresh(&remote, snapshot(true)));
}

#[test]
fn a_session_removed_while_refused_still_closes_once() {
    let (remote, queued) = attached(snapshot(true));
    let request_id = agent_subscription(&queued);
    let client = remote.runtime_client().expect("connected");
    client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Error {
                code: phux_protocol::wire::frame::ErrorCode::InvalidCommand,
                message: "busy".into(),
            },
        })
        .expect("refusal");
    assert!(refresh(&remote, snapshot(false)).is_empty());
    let events = agent_events(remote.take_events());
    assert!(
        matches!(
            events.as_slice(),
            [WireEvent::AgentRecords {
                kind: AgentRecordsKind::Closed,
                ..
            }]
        ),
        "{events:?}"
    );
    assert!(refresh(&remote, snapshot(false)).is_empty());
    assert!(agent_events(remote.take_events()).is_empty());
}
