//! Paste trust and sizing.

use super::*;

/// A plane that negotiated acknowledged input, with nothing queued.
fn acknowledging() -> ControlPlane {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    plane.take_outbound();
    plane
        .feed(hello_ok_with(&[ServerFeature::AcknowledgedInput]))
        .unwrap();
    plane.take_outbound();
    plane
}

/// Every paste the frames carry, and whether it went acknowledged.
fn pastes(frames: &[Vec<u8>]) -> Vec<(bool, PasteEvent)> {
    frames
        .iter()
        .filter_map(|bytes| match decode(bytes) {
            FrameKind::Command {
                command: Command::ApplyInput { events, .. },
                ..
            } => events.into_iter().find_map(|event| match event {
                InputEvent::Paste(paste) => Some((true, paste)),
                _ => None,
            }),
            FrameKind::InputPaste { event, .. } => Some((false, event)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_user_paste_is_trusted_so_multiline_text_is_not_refused() {
    let mut plane = acknowledging();
    let delivery = plane.apply_user_paste(&terminal(), "echo a\necho b\n");
    assert!(delivery.is_some(), "one command's worth is acknowledged");
    let sent = pastes(&plane.take_outbound());
    assert_eq!(sent.len(), 1);
    let (acknowledged, paste) = &sent[0];
    assert!(acknowledged);
    assert_eq!(paste.trust, PasteTrust::Trusted);
    assert_eq!(paste.data, b"echo a\necho b\n");
    assert!(
        !plane
            .take_events()
            .iter()
            .any(|event| matches!(event, Event::InputDelivery { .. })),
        "nothing refused locally"
    );
}

#[test]
fn a_program_paste_stays_untrusted() {
    let mut plane = acknowledging();
    plane.apply_paste(&terminal(), "x\n");
    let sent = pastes(&plane.take_outbound());
    assert_eq!(sent[0].1.trust, PasteTrust::Untrusted);
}

#[test]
fn a_user_paste_too_large_for_one_command_goes_as_one_paste_event() {
    let mut plane = acknowledging();
    let text = "0123456789abcdef\n".repeat(8 * 1024);
    assert!(text.len() > 64 * 1024);
    assert_eq!(plane.apply_user_paste(&terminal(), &text), None);
    let sent = pastes(&plane.take_outbound());
    assert_eq!(sent.len(), 1, "never split (docs/protocol.md#input)");
    let (acknowledged, paste) = &sent[0];
    assert!(!acknowledged);
    assert_eq!(paste.trust, PasteTrust::Trusted);
    assert_eq!(paste.data, text.as_bytes());
}

#[test]
fn a_large_user_paste_never_overtakes_unresolved_input() {
    let mut plane = acknowledging();
    plane.apply_user_paste(&terminal(), "first");
    plane.take_outbound();
    // A raw event could reach the PTY before the unresolved paste, so the
    // large one is refused, with a receipt, rather than reordered.
    let text = "x".repeat(70 * 1024);
    let refused = plane.apply_user_paste(&terminal(), &text).expect("receipt");
    assert!(pastes(&plane.take_outbound()).is_empty());
    assert!(plane.take_events().iter().any(|event| matches!(
        event,
        Event::InputDelivery { delivery_id, outcome: DeliveryOutcome::Refused, .. }
            if *delivery_id == refused
    )));
}

#[test]
fn a_user_paste_into_a_satellite_pane_goes_as_one_paste_event() {
    let mut plane = acknowledging();
    let satellite = ResourceId::satellite("peer", 3);
    assert_eq!(plane.apply_user_paste(&satellite, "ls\npwd\n"), None);
    let sent = pastes(&plane.take_outbound());
    assert_eq!(sent.len(), 1);
    assert!(!sent[0].0, "APPLY_INPUT is local-only");
    assert_eq!(sent[0].1.trust, PasteTrust::Trusted);
}

/// A user paste the conservative `APPLY_INPUT` batch rule refuses goes raw,
/// never refused: only a paste that rule admits is sent acknowledged.
#[test]
fn a_user_paste_over_the_batch_rule_goes_as_one_paste_event() {
    use phux_client_core::input_replay::validate_apply_input_batch;
    use phux_protocol::wire::frame::MAX_APPLY_INPUT_COMMAND_BODY;

    let batch = |len: usize| {
        vec![InputEvent::Paste(PasteEvent {
            trust: PasteTrust::Trusted,
            data: vec![b'x'; len],
        })]
    };
    let largest = (0..=MAX_APPLY_INPUT_COMMAND_BODY)
        .rev()
        .find(|len| validate_apply_input_batch(&batch(*len)).is_ok())
        .expect("an empty paste fits");

    let mut plane = acknowledging();
    let text = "x".repeat(largest);
    assert!(plane.apply_user_paste(&terminal(), &text).is_some());
    let sent = pastes(&plane.take_outbound());
    assert_eq!(sent.len(), 1);
    assert!(sent[0].0, "the largest admitted paste goes acknowledged");

    let mut plane = acknowledging();
    let text = "x".repeat(largest + 1);
    assert_eq!(plane.apply_user_paste(&terminal(), &text), None);
    let sent = pastes(&plane.take_outbound());
    assert_eq!(sent.len(), 1, "never split (docs/protocol.md#input)");
    assert!(!sent[0].0, "one byte more goes as a raw paste");
    assert_eq!(sent[0].1.data, text.as_bytes());
    assert!(
        !plane
            .take_events()
            .iter()
            .any(|event| matches!(event, Event::InputDelivery { .. })),
        "nothing refused locally"
    );
}
