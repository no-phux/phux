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
    assert_eq!(sent.len(), 1, "never split (input.md 5.1)");
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
