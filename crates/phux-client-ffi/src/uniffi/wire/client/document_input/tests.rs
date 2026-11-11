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
        remote.project_event(Event::CommandResult { request_id, result }, &mut projected);
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
