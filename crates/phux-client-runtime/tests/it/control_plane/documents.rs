//! A document resource publishes through the same slot table as a grid:
//! newest wins, one wake per resource, and the slot retires with the
//! resource or the connection.

use phux_client_runtime::Client;
use phux_client_runtime::control::ControlOptions;
use phux_client_runtime::publication::Published;
use phux_protocol::ResourceKind;

use super::*;

const fn tree() -> ResourceId {
    ResourceId::local(8)
}

fn bootstrap(client: &Client, id: ResourceId, profile: BootstrapStreamProfile, payload: &[u8]) {
    let stream_id = StreamId::new(u64::from(id.local_id().unwrap())).unwrap();
    let bootstrap_id = BootstrapId::new(1).unwrap();
    let terminal = profile == BootstrapStreamProfile::SynthesizedVtRaw;
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: id.clone(),
            stream_id,
            bootstrap_id,
            profile,
            cols: if terminal { 20 } else { 0 },
            rows: if terminal { 4 } else { 0 },
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: payload.to_vec().into(),
        },
        FrameKind::BootstrapReady {
            terminal_id: id,
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ] {
        client.feed(frame).unwrap();
    }
}

fn publish(client: &Client, seq: u64, payload: &[u8]) {
    client
        .feed(FrameKind::ResourceOutput {
            terminal_id: tree(),
            stream_id: StreamId::new(u64::from(tree().local_id().unwrap())).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq,
            bytes: payload.to_vec().into(),
        })
        .unwrap();
}

/// Attached to `main`'s terminal and an accessibility tree, both bootstrapped.
fn attached_with_tree() -> Client {
    let client = phux_client_runtime::Runtime::embedded(ControlOptions {
        attach: Some(AttachTarget::ByName("main".into())),
        viewport: (20, 4),
        ..ControlOptions::default()
    });
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let attach_id = client
        .take_outbound()
        .iter()
        .find_map(|bytes| match decode(bytes) {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .unwrap();
    let mut snapshot = snapshot();
    snapshot.resources.push(
        ResourceInfo::resource(tree(), ResourceKind::AccessibilityTree)
            .with_session(Some(SessionId::new(1))),
    );
    client
        .feed(FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id: ClientId::new(1),
        })
        .unwrap();
    bootstrap(
        &client,
        terminal(),
        BootstrapStreamProfile::SynthesizedVtRaw,
        b"ready",
    );
    bootstrap(
        &client,
        tree(),
        BootstrapStreamProfile::AccessibilityTreeJsonV1,
        b"{\"cut\":0}",
    );
    client.feed(FrameKind::AttachReady { attach_id }).unwrap();
    let _ = client.take_outbound();
    client
}

fn document_wakes(client: &Client) -> Vec<(ResourceId, u64)> {
    client
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            Event::DocumentPublished {
                resource_id,
                generation,
            } => Some((resource_id, generation)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_document_publishes_into_its_slot_and_wakes_once_per_resource() {
    let client = attached_with_tree();
    assert_eq!(document_wakes(&client), [(tree(), 1)]);
    let cut = client.acquire_document(&tree()).expect("the bootstrap cut");
    assert_eq!(
        (cut.kind, cut.bytes.as_slice()),
        (ResourceKind::AccessibilityTree, b"{\"cut\":0}" as &[u8])
    );
    assert!(client.acquire(&tree()).is_none(), "a document is no grid");
    assert!(client.acquire_document(&terminal()).is_none());
    assert_eq!(client.publication().terminals(), [terminal()]);
    let mut resources = client.publication().resources();
    resources.sort_by_key(ResourceId::local_id);
    assert_eq!(resources, [terminal(), tree()]);

    // Two publishes before a drain: one wake, and the slot holds the newest.
    publish(&client, 1, b"{\"live\":1}");
    publish(&client, 2, b"{\"live\":2}");
    assert_eq!(document_wakes(&client), [(tree(), 3)]);
    assert!(matches!(
        client.publication().acquire_published(&tree()),
        Some(Published::Document(document)) if document.seq == 2 && document.bytes == b"{\"live\":2}"
    ));
    assert_eq!(
        cut.bytes, b"{\"cut\":0}",
        "a document handed out stays whole"
    );
}

#[test]
fn closing_a_document_retires_its_slot() {
    let client = attached_with_tree();
    let slot = client.slot(&tree()).expect("published");
    client
        .feed(FrameKind::ResourceClosed {
            terminal_id: tree(),
            exit_status: None,
            reason: phux_protocol::wire::frame::CloseReason::Exited,
            signal: None,
        })
        .unwrap();
    assert!(client.acquire_document(&tree()).is_none());
    assert!(client.generation(&tree()).is_none());
    assert_eq!(
        slot.generation(),
        1,
        "a held handle keeps its last generation"
    );
    assert!(
        client.acquire(&terminal()).is_some(),
        "only the document's slot"
    );
}

#[test]
fn releasing_a_document_retires_its_slot() {
    let client = attached_with_tree();
    assert!(client.with_control(|plane| plane.release_terminal(&tree())));
    assert!(client.acquire_document(&tree()).is_none());
    assert!(
        client.acquire(&terminal()).is_some(),
        "only the document's slot"
    );
}

#[test]
fn a_new_server_incarnation_retires_document_slots() {
    let client = attached_with_tree();
    let held = client.acquire_document(&tree()).expect("published");
    let reconnect = |server_id: u8| {
        client.with_control(ControlPlane::connection_opened);
        let _ = client.take_outbound();
        let mut hello = hello_ok(PROTOCOL_VERSION.patch);
        if let FrameKind::HelloOk { server_id: id, .. } = &mut hello {
            *id = vec![server_id; 16];
        }
        client.feed(hello).unwrap();
    };
    // The same incarnation resumes its streams: like a grid, the newest
    // document stays readable until the re-attach publishes a newer one.
    reconnect(0xAB);
    assert!(client.acquire_document(&tree()).is_some());
    // A new incarnation reuses ids for anything; nothing of the old one stays.
    reconnect(0xCD);
    assert!(client.acquire_document(&tree()).is_none());
    assert!(!client.publication().resources().contains(&tree()));
    assert_eq!(held.bytes, b"{\"cut\":0}");
}
