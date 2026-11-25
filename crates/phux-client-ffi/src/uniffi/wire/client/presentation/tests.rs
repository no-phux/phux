#![allow(clippy::expect_used, clippy::panic, reason = "test assertions")]

use super::*;
use phux_client_runtime::control::{ControlOptions, ControlPlane};
use phux_protocol::ids::{SessionId, WindowId};
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
const COLS: u16 = 24;
const ROWS: u16 = 4;
#[path = "../search/tests/support.rs"]
mod support;
use support::remote_with;
fn pane(terminal: u32) -> String { id::encode(&ResourceId::local(terminal)) }
fn bootstrap(client: &Client, terminal_id: ResourceId, bytes: &[u8]) {
    for frame in phux_wire_fixtures::bootstrap_frames(&terminal_id, [1, 1], [COLS, ROWS], bytes) {
        client.feed(frame).expect("bootstrap frame");
    }
}
fn unknown_delivery(client: &Client) {
    use phux_protocol::wire::frame::{Command, ErrorCode};
    let delivery_id = client.apply_line(&ResourceId::local(1), "possibly written");
    let request = client
        .take_outbound()
        .iter()
        .find_map(|bytes| match phux_wire_fixtures::decode_one(bytes) {
            FrameKind::Command {
                request_id,
                command: Command::ApplyInput { .. },
            } => Some(request_id),
            _ => None,
        })
        .expect("original APPLY_INPUT");
    client
        .feed(phux_wire_fixtures::command_err(
            request,
            ErrorCode::InputDeliveryUnknown,
            "lost delivery evidence",
        ))
        .expect("unknown receipt");
    assert!(client.delivery_fenced(&ResourceId::local(1)));
    assert!(client.take_events().iter().any(|event| matches!(event,
        Event::InputDelivery {
            delivery_id: actual,
            outcome: phux_client_runtime::control::DeliveryOutcome::Unknown,
            ..
        } if *actual == delivery_id
    )));
}

fn authoritative_output(client: &Client, seq: u64) {
    client
        .feed(phux_wire_fixtures::output(
            &ResourceId::local(1),
            seq,
            format!("\r\n{seq}").as_bytes(),
        ))
        .expect("authoritative output");
}

fn projection_receipt(remote: &RemoteClient) -> Arc<engine::PresentationReceipt> {
    remote
        .render_projection(pane(1))
        .expect("render")
        .expect("frame")
        .presentation_receipt
        .expect("fresh frame carries evidence")
}

#[test]
fn acquisition_hidden_and_clipped_frames_never_acknowledge_delivery() {
    let remote = remote_with(&[(1, b"before".to_vec())]);
    let client = remote.runtime_client().expect("connected");
    unknown_delivery(&client);
    // Acquisition is not actual visibility, even when it carries a receipt.
    let _ = remote
        .render_projection(pane(1))
        .expect("render")
        .expect("frame");
    assert!(client.delivery_fenced(&ResourceId::local(1)));
    authoritative_output(&client, 1);
    let receipt = projection_receipt(&remote);
    assert!(!remote.acknowledge_presentation(Arc::clone(&receipt), false, true));
    assert!(!remote.acknowledge_presentation(Arc::clone(&receipt), true, false));
    let _ = remote
        .render_projection(pane(1))
        .expect("another acquisition");
    assert!(client.delivery_fenced(&ResourceId::local(1)));
    assert!(remote.acknowledge_presentation(Arc::clone(&receipt), true, true));
    assert!(!client.delivery_fenced(&ResourceId::local(1)));
    assert!(!remote.acknowledge_presentation(receipt, true, true));
}

#[test]
fn replaced_frames_newer_uncertainty_and_epochs_reject_old_receipts() {
    let remote = remote_with(&[(1, b"before".to_vec())]);
    let client = remote.runtime_client().expect("connected");
    unknown_delivery(&client);
    authoritative_output(&client, 1);
    let first = projection_receipt(&remote);
    authoritative_output(&client, 2);
    assert!(!remote.acknowledge_presentation(first, true, true));
    let current = projection_receipt(&remote);
    assert!(remote.acknowledge_presentation(Arc::clone(&current), true, true));
    unknown_delivery(&client);
    authoritative_output(&client, 3);
    assert!(!remote.acknowledge_presentation(current, true, true));
    assert!(client.delivery_fenced(&ResourceId::local(1)));
    let before_reconnect = projection_receipt(&remote);
    client.with_control(ControlPlane::connection_opened);
    assert!(!remote.acknowledge_presentation(before_reconnect, true, true));
    assert!(client.delivery_fenced(&ResourceId::local(1)));
}

#[test]
fn destroyed_views_cannot_acknowledge_a_sibling_views_terminal_fence() {
    let remote = remote_with(&[(1, b"before".to_vec())]);
    let client = remote.runtime_client().expect("connected");
    let id = ResourceId::local(1);
    let source = client.view_source(&id, Some(&[0xAB; 16]), client.connection_epoch()).expect("source");
    let left = client.create_view(&source).expect("left view");
    let right = client.create_view(&source).expect("right view");
    unknown_delivery(&client);
    authoritative_output(&client, 1);
    let left_frame = client.acquire_view(left).expect("left frame");
    let right_frame = client.acquire_view(right).expect("right frame");
    let left_receipt = client
        .presentation_evidence(Some(left), &left_frame)
        .expect("left evidence");
    let right_receipt = client
        .presentation_evidence(Some(right), &right_frame)
        .expect("right evidence");
    let _ = client.take_outbound();
    client.destroy_view(left).expect("destroy only left");
    assert!(client.take_outbound().is_empty(), "closing one view must send no execution command");
    assert!(client.has_projection(&id), "view retirement preserves the live terminal");
    assert!(!client.acknowledge_presentation(&left_receipt, true, true));
    assert!(client.delivery_fenced(&id));
    assert!(client.acknowledge_presentation(&right_receipt, true, true));
    assert!(client.acquire_view(right).is_some());
}

#[test]
fn a_replacement_bootstrap_cannot_acknowledge_the_previous_frame() {
    let remote = remote_with(&[(1, b"before".to_vec())]);
    let client = remote.runtime_client().expect("connected");
    unknown_delivery(&client);
    authoritative_output(&client, 1);
    let old = projection_receipt(&remote);
    for frame in phux_wire_fixtures::bootstrap_frames(
        &ResourceId::local(1),
        [1, 2],
        [COLS, ROWS],
        b"replacement",
    ) {
        client.feed(frame).expect("replacement bootstrap");
    }
    assert!(!remote.acknowledge_presentation(old, true, true));
    assert!(client.delivery_fenced(&ResourceId::local(1)));
    let current = projection_receipt(&remote);
    assert!(remote.acknowledge_presentation(current, true, true));
}
