#![allow(clippy::expect_used, clippy::panic, reason = "test assertions")]

use super::super::test_support::{COLS, ROWS, remote_with};
use super::*;
use phux_protocol::wire::frame::FrameKind;
fn pane(terminal: u32) -> String {
    id::encode(&ResourceId::local(terminal))
}
#[test]
fn binding_user_submissions_distinguish_journal_raw_and_refusal() {
    use phux_protocol::wire::frame::{Command, CommandResult};
    let offline = RemoteClient::new("unused".into(), COLS, ROWS, None, None);
    assert_eq!(
        offline.apply_user_paste(pane(1), "nothing".into()),
        WireInputSubmission::NotQueued
    );
    assert_eq!(
        offline.send_paste_trusted(pane(1), "nothing".into()),
        WireInputSubmission::NotQueued
    );
    let remote = remote_with(&[(1, b"before".to_vec())]);
    let client = remote.runtime_client().expect("connected");
    let WireInputSubmission::AwaitingReceipt { delivery_id } =
        remote.apply_user_paste(pane(1), "original".into())
    else {
        panic!("small user paste must await its original receipt");
    };
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
        .expect("acknowledged attempt");
    assert!(matches!(
        remote.apply_user_paste(pane(1), "x".repeat(70 * 1024)),
        WireInputSubmission::Refused { .. }
    ));
    assert!(
        client.take_outbound().is_empty(),
        "large raw fallback must not overtake"
    );
    client
        .feed(FrameKind::CommandResult {
            request_id: request,
            result: CommandResult::Ok,
        })
        .expect("delivery receipt");
    assert!(client.take_events().iter().any(|event| matches!(event,
        Event::InputDelivery {
            delivery_id: actual,
            outcome: phux_client_runtime::control::DeliveryOutcome::Delivered,
            ..
        } if *actual == delivery_id
    )));
    assert_eq!(
        remote.apply_user_paste(pane(1), "x".repeat(70 * 1024)),
        WireInputSubmission::QueuedRaw,
    );
    let pastes = client
        .take_outbound()
        .iter()
        .filter(|bytes| {
            matches!(
                phux_wire_fixtures::decode_one(bytes),
                FrameKind::InputPaste { .. }
            )
        })
        .count();
    assert_eq!(pastes, 1, "one atomic raw frame, no delivery claim");
}
