#![allow(clippy::expect_used, clippy::panic, reason = "test assertions")]

use phux_client_runtime::control::{ControlOptions, DocumentPublication};
use phux_protocol::ResourceKind;
use phux_protocol::ids::{SessionId, WindowId};
use phux_protocol::wire::frame::{Command, CommandValue, DocumentKey, FrameKind};
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

use super::*;

const PAGE: u32 = 9;
const PLAIN: u32 = 8;

/// A client with a sans-IO runtime that has seen `PAGE` (class `page`) and
/// `PLAIN` (no class) in a snapshot.
fn remote() -> (Arc<RemoteClient>, Client) {
    let remote = RemoteClient::new("unused".into(), 80, 24, None, None);
    let client = Runtime::embedded(ControlOptions::default());
    *remote.client.lock().unwrap() = Some(client.clone());
    let window = WindowId::new(1);
    let snapshot = SessionSnapshot::new(SessionId::new(1), window, ResourceId::local(0))
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(PAGE), window, 0, 0)
                .with_control(Some(ControlClass::Page)),
            ResourceInfo::new(ResourceId::local(PLAIN), window, 0, 0),
        ]);
    let mut projected = Vec::new();
    remote.project_event(
        None,
        Event::TopologySnapshot {
            attach_id: None,
            snapshot,
        },
        &mut projected,
    );
    assert!(projected.is_empty(), "a snapshot projects nothing itself");
    (remote, client)
}

fn published(remote: &RemoteClient, client: &Client, resource: u32) -> Option<String> {
    let resource_id = ResourceId::local(resource);
    let generation =
        client
            .publication()
            .publish_document_for_testing(Arc::new(DocumentPublication {
                resource_id: resource_id.clone(),
                kind: ResourceKind::PixelSurface,
                seq: 1,
                bytes: Vec::new(),
                content_type: None,
            }));
    let mut projected = Vec::new();
    remote.project_event(
        Some(client),
        Event::DocumentPublished {
            resource_id,
            generation,
        },
        &mut projected,
    );
    let [WireEvent::DocumentPublished { control, .. }] = projected.as_slice() else {
        panic!("expected one document: {projected:?}");
    };
    control.clone()
}

#[test]
fn a_published_document_carries_the_snapshot_control_class() {
    let (remote, client) = remote();
    assert_eq!(published(&remote, &client, PAGE).as_deref(), Some("page"));
    assert_eq!(published(&remote, &client, PLAIN), None);
    assert_eq!(published(&remote, &client, 77), None, "never listed");
}

#[test]
fn send_document_input_queues_one_send_document_input_command() {
    let offline = RemoteClient::new("unused".into(), 80, 24, None, None);
    let click = WireDocumentIntent::Click { x: 3, y: 4 };
    assert_eq!(
        offline.send_document_input("local:9".into(), click.clone()),
        0
    );

    let (remote, client) = remote();
    let _ = client.take_outbound();
    assert_eq!(
        remote.send_document_input("not an id".into(), click.clone()),
        0
    );
    let long = "x".repeat(MAX_DOCUMENT_INPUT_TEXT + 1);
    let typed = WireDocumentIntent::Type { text: long };
    assert_eq!(remote.send_document_input("local:9".into(), typed), 0);
    let huge = "x".repeat(MAX_CLIPBOARD_TEXT + 1);
    let paste = WireDocumentIntent::Paste { text: huge };
    assert_eq!(remote.send_document_input("local:9".into(), paste), 0);
    assert!(client.take_outbound().is_empty(), "refused locally");

    let key = WireDocumentIntent::Key {
        key: WireDocumentKey::ArrowLeft,
    };
    let paste = WireDocumentIntent::Paste {
        text: "y".repeat(MAX_DOCUMENT_INPUT_TEXT + 1),
    };
    let sent = [
        remote.send_document_input("local:9".into(), click),
        remote.send_document_input("local:9".into(), key),
        remote.send_document_input("local:9".into(), paste),
        remote.send_document_input("local:9".into(), WireDocumentIntent::CopyClipboard),
        remote.send_document_input(
            "local:9".into(),
            WireDocumentIntent::Drag {
                from: WireFramePoint { x: 1, y: 2 },
                to: WireFramePoint { x: 30, y: 2 },
            },
        ),
        remote.send_document_input("local:9".into(), WireDocumentIntent::CopySelection),
        remote.send_document_input("local:9".into(), WireDocumentIntent::CutSelection),
        remote.send_document_input("local:9".into(), WireDocumentIntent::SelectAll),
    ];
    let frames: Vec<_> = client
        .take_outbound()
        .iter()
        .map(|bytes| FrameKind::decode(bytes).expect("decodes").0)
        .collect();
    let expected = |request_id, intent| FrameKind::Command {
        request_id,
        command: Command::SendDocumentInput {
            terminal_id: ResourceId::local(PAGE),
            intent,
            control: None,
            on_behalf_of: None,
        },
    };
    assert_eq!(
        frames,
        [
            expected(sent[0], DocumentInput::Click { x: 3, y: 4 }),
            expected(
                sent[1],
                DocumentInput::Key {
                    key: DocumentKey::ArrowLeft
                }
            ),
            expected(
                sent[2],
                DocumentInput::Paste {
                    text: "y".repeat(MAX_DOCUMENT_INPUT_TEXT + 1),
                    chord: true,
                }
            ),
            expected(sent[3], DocumentInput::CopyClipboard { reply: 0 }),
            expected(
                sent[4],
                DocumentInput::Drag {
                    from: (1, 2),
                    to: (30, 2),
                }
            ),
            expected(
                sent[5],
                DocumentInput::CopySelection {
                    reply: 0,
                    cut: false,
                }
            ),
            expected(
                sent[6],
                DocumentInput::CopySelection {
                    reply: 0,
                    cut: true,
                }
            ),
            expected(sent[7], DocumentInput::SelectAll),
        ]
    );
}

#[test]
fn replies_settle_as_outcomes_and_others_pass_through() {
    let (remote, _) = remote();
    let click = || WireDocumentIntent::Click { x: 0, y: 0 };
    let error = |code| CommandResult::Error {
        code,
        message: "no".into(),
    };
    let cases = [
        (PAGE, CommandResult::Ok, WireDocumentInputOutcome::Sent),
        (
            PAGE,
            error(ErrorCode::PermissionDenied),
            WireDocumentInputOutcome::NoGrant,
        ),
        (
            PLAIN,
            error(ErrorCode::PermissionDenied),
            WireDocumentInputOutcome::NotControllable,
        ),
        (
            PAGE,
            error(ErrorCode::NotProducer),
            WireDocumentInputOutcome::NoProducer,
        ),
        (
            PAGE,
            CommandResult::OkWith(CommandValue::Bytes(b"host text".to_vec())),
            WireDocumentInputOutcome::Copied {
                text: "host text".into(),
            },
        ),
        (
            PAGE,
            CommandResult::Error {
                code: ErrorCode::Unavailable,
                message: "Nothing copied on the host".into(),
            },
            WireDocumentInputOutcome::Other {
                message: "Nothing copied on the host".into(),
            },
        ),
        (
            PAGE,
            error(ErrorCode::Overflow),
            WireDocumentInputOutcome::Other {
                message: "Overflow: no".into(),
            },
        ),
    ];
    for (resource, result, outcome) in cases {
        let resource_id = id::encode(&ResourceId::local(resource));
        let request_id = remote.send_document_input(resource_id.clone(), click());
        let mut projected = Vec::new();
        remote.project_event(
            None,
            Event::CommandResult { request_id, result },
            &mut projected,
        );
        assert_eq!(
            projected,
            [WireEvent::DocumentInputResult {
                request_id,
                resource_id,
                outcome
            }]
        );
    }
    let mut projected = Vec::new();
    remote.project_event(
        None,
        Event::CommandResult {
            request_id: 4242,
            result: CommandResult::Ok,
        },
        &mut projected,
    );
    assert!(projected.is_empty(), "not a document input reply");
}

#[test]
fn the_mapping_helpers_are_the_cores() {
    // 200x100 in a 100x200 pane: barred above and below, two pixels a point.
    for point in [[0.0, 75.0], [50.0, 100.0], [99.9, 124.9], [50.0, 74.0]] {
        assert_eq!(
            document_frame_point(100.0, 200.0, 200, 100, point[0], point[1]).map(|p| [p.x, p.y]),
            core_input::frame_point([0.0, 0.0, 100.0, 200.0], [200, 100], point),
        );
    }
    assert_eq!(
        document_frame_point(100.0, 200.0, 200, 100, 50.0, 100.0),
        Some(WireFramePoint { x: 100, y: 50 })
    );
    assert_eq!(
        document_frame_point(100.0, 200.0, 200, 100, 50.0, 74.0),
        None
    );
    assert_eq!(
        document_scroll(100.0, 50.0, 200, 100, 0.0, -10.0),
        Some(WireDocumentIntent::Scroll { dx: 0, dy: 20 })
    );
    assert_eq!(document_scroll(100.0, 50.0, 200, 100, 0.1, 0.2), None);
    assert_eq!(document_scroll(0.0, 50.0, 200, 100, 0.0, 10.0), None);
}

#[test]
fn a_stale_wake_projects_nothing_and_the_newer_one_projects_once() {
    let (remote, client) = remote();
    let publish = |seq| {
        client
            .publication()
            .publish_document_for_testing(Arc::new(DocumentPublication {
                resource_id: ResourceId::local(PAGE),
                kind: ResourceKind::PixelSurface,
                seq,
                bytes: b"png".to_vec(),
                content_type: None,
            }))
    };
    let stale = publish(1);
    let current = publish(2);
    let mut projected = Vec::new();
    for generation in [stale, current] {
        remote.project_event(
            Some(&client),
            Event::DocumentPublished {
                resource_id: ResourceId::local(PAGE),
                generation,
            },
            &mut projected,
        );
    }
    let [WireEvent::DocumentPublished { seq: 2, .. }] = projected.as_slice() else {
        panic!("expected the current document once: {projected:?}");
    };
}

/// A sans-IO runtime attached to `main` with a bootstrapped terminal and an
/// accessibility tree whose bootstrap wake is still undrained.
fn attached_tree() -> (Arc<RemoteClient>, Client, ResourceId) {
    use phux_protocol::caps::{
        BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities,
    };
    use phux_protocol::ids::{BootstrapId, ClientId, StreamId};
    use phux_protocol::wire::frame::AttachTarget;
    use phux_protocol::wire::info::{SessionInfo, WindowInfo};

    let client = Runtime::embedded(ControlOptions {
        attach: Some(AttachTarget::ByName("main".into())),
        viewport: (20, 4),
        ..ControlOptions::default()
    });
    client.with_control(phux_client_runtime::control::ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client
        .feed(FrameKind::HelloOk {
            protocol_major: phux_protocol::PROTOCOL_VERSION.major,
            protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
            protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new(),
            server_id: vec![0xAB; 16],
            selected_profile: Some(BootstrapProfile::SynthesizedVtRaw),
            bootstrap_limits: BootstrapLimits::default(),
        })
        .expect("HELLO_OK");
    let attach_id = client
        .take_outbound()
        .iter()
        .find_map(|bytes| match FrameKind::decode(bytes).expect("decodes").0 {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .expect("ATTACH queued");
    let (session, window) = (SessionId::new(1), WindowId::new(1));
    let (terminal, tree) = (ResourceId::local(7), ResourceId::local(PLAIN));
    let snapshot = SessionSnapshot::new(session, window, terminal.clone())
        .with_sessions(vec![SessionInfo::new(session, "main")])
        .with_windows(vec![WindowInfo::new(window, session, "shell")])
        .with_resources(vec![
            ResourceInfo::new(terminal.clone(), window, 20, 4),
            ResourceInfo::resource(tree.clone(), ResourceKind::AccessibilityTree)
                .with_session(Some(session)),
        ]);
    client
        .feed(FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id: ClientId::new(1),
        })
        .expect("ATTACHED");
    for (id, profile, cols, rows) in [
        (&terminal, BootstrapStreamProfile::SynthesizedVtRaw, 20, 4),
        (&tree, BootstrapStreamProfile::AccessibilityTreeJsonV1, 0, 0),
    ] {
        let stream_id = StreamId::new(u64::from(id.local_id().expect("local"))).expect("stream");
        let bootstrap_id = BootstrapId::new(1).expect("bootstrap");
        for frame in [
            FrameKind::BootstrapBegin {
                terminal_id: id.clone(),
                stream_id,
                bootstrap_id,
                profile,
                cols,
                rows,
                base_seq: 0,
            },
            FrameKind::BootstrapChunk {
                terminal_id: id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: b"{}".to_vec().into(),
            },
            FrameKind::BootstrapReady {
                terminal_id: id.clone(),
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
        ] {
            client.feed(frame).expect("bootstrap");
        }
    }
    client
        .feed(FrameKind::AttachReady { attach_id })
        .expect("ATTACH_READY");
    let remote = RemoteClient::new("unused".into(), 20, 4, None, None);
    *remote.client.lock().unwrap() = Some(client.clone());
    (remote, client, tree)
}

fn documents(remote: &RemoteClient) -> usize {
    remote
        .take_events()
        .iter()
        .filter(|event| matches!(event, WireEvent::DocumentPublished { .. }))
        .count()
}

#[test]
fn an_undrained_wake_projects_once_and_never_after_close_or_detach() {
    let (remote, _, _) = attached_tree();
    assert_eq!(documents(&remote), 1, "the bootstrap cut");
    assert_eq!(documents(&remote), 0, "never twice");

    let (remote, client, tree) = attached_tree();
    client
        .feed(FrameKind::ResourceClosed {
            terminal_id: tree,
            exit_status: None,
            reason: phux_protocol::wire::frame::CloseReason::Exited,
            signal: None,
        })
        .expect("RESOURCE_CLOSED");
    assert_eq!(documents(&remote), 0, "closed");

    let (remote, client, tree) = attached_tree();
    assert!(client.with_control(|plane| plane.release_terminal(&tree)));
    assert_eq!(documents(&remote), 0, "detached");
}
