//! OSC 52 reads surface only from the focused terminal of a server that
//! takes `CLIPBOARD_REPLY`, and a reply goes to the asking terminal only.

use super::*;
use phux_client_core::engine::ClipboardSelection;
use phux_protocol::caps::{ServerFeatureExt, ServerFeatureExtSet};
use phux_protocol::input::focus::FocusEvent;

const READ: &[u8] = b"\x1b]52;c;?\x07";

/// `negotiated()` + attach against a server that does (or does not)
/// advertise `CLIPBOARD_READ`.
fn attached_to(replies: bool) -> ControlPlane {
    let mut plane = ControlPlane::new(ControlOptions {
        attach: Some(AttachTarget::ByName("main".to_owned())),
        viewport: (20, 4),
        ..ControlOptions::default()
    });
    plane.connection_opened();
    let _ = plane.take_outbound();
    let mut hello = hello_ok(PROTOCOL_VERSION.patch);
    if replies && let FrameKind::HelloOk { server_caps, .. } = &mut hello {
        *server_caps = server_caps.with_features_ext(ServerFeatureExtSet::with(&[
            ServerFeatureExt::ClipboardRead,
        ]));
    }
    plane.feed(hello).unwrap();
    let attach_id = plane
        .take_outbound()
        .iter()
        .find_map(|bytes| match decode(bytes) {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .expect("ATTACH");
    attach(&mut plane, attach_id, b"first");
    let _ = plane.take_outbound();
    let _ = plane.take_events();
    plane
}

fn reads(plane: &mut ControlPlane) -> Vec<(ResourceId, ClipboardSelection)> {
    plane
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            Event::ClipboardRead {
                terminal_id,
                selection,
            } => Some((terminal_id, selection)),
            _ => None,
        })
        .collect()
}

#[test]
fn an_unfocused_terminal_never_surfaces_a_read() {
    let mut plane = attached_to(true);
    output(&mut plane, 1, READ);
    assert!(reads(&mut plane).is_empty(), "no focus, no read");
    plane.send_focus(&terminal(), FocusEvent::Gained);
    plane.send_focus(&terminal(), FocusEvent::Lost);
    output(&mut plane, 2, READ);
    assert!(reads(&mut plane).is_empty(), "focus lost, no read");
}

#[test]
fn the_focused_terminal_reads_and_the_reply_goes_to_it_only() {
    let mut plane = attached_to(true);
    plane.send_focus(&terminal(), FocusEvent::Gained);
    let _ = plane.take_outbound();
    output(&mut plane, 1, READ);
    assert_eq!(
        reads(&mut plane),
        [(terminal(), ClipboardSelection::Clipboard)]
    );
    assert!(plane.send_clipboard_reply(
        &terminal(),
        ClipboardSelection::Clipboard,
        "secret".to_owned()
    ));
    let sent: Vec<_> = plane.take_outbound().iter().map(|b| decode(b)).collect();
    assert!(matches!(
        sent.as_slice(),
        [FrameKind::ClipboardReply { terminal_id, selection: b'c', text }]
            if *terminal_id == terminal() && text.0 == "secret"
    ));
}

#[test]
fn a_server_without_replies_is_never_asked() {
    let mut plane = attached_to(false);
    plane.send_focus(&terminal(), FocusEvent::Gained);
    let _ = plane.take_outbound();
    output(&mut plane, 1, READ);
    assert!(reads(&mut plane).is_empty());
    assert!(!plane.send_clipboard_reply(
        &terminal(),
        ClipboardSelection::Clipboard,
        "secret".to_owned()
    ));
    assert!(plane.take_outbound().is_empty());
}
