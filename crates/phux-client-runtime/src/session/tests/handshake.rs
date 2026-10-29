//! Capability negotiation and attach-id allocation.

use super::*;

#[test]
fn negotiation_records_the_server_and_refuses_a_second_round() {
    let caps = phux_protocol::caps::ServerCapabilities::new().with_features(
        phux_protocol::caps::ServerFeatureSet::with(&[ServerFeature::AttachRoles]),
    );
    let (mut client, negotiated) = negotiate_against(move |offer| {
        assert!(
            !offer.quic_streams,
            "a Unix socket never offers QUIC streams"
        );
        hello_ok(offer, caps)
    });
    negotiated.expect("negotiated");
    assert_eq!(client.server_id(), Some(&b"incarnation-1"[..]));
    assert!(client.negotiated_bootstrap().is_some());
    assert_eq!(client.observer_role_policy(), Some(RolePolicy::VIEWER));
    assert!(!client.multistream_enabled(), "UDS is single-stream");
    let again = block_on(client.negotiate("probe".to_owned(), control_caps()));
    assert!(matches!(again, Err(SessionError::Protocol(_))), "{again:?}");
}

/// The code, not the sentence, distinguishes a live old server. The message
/// is the one a 0.9.x server builds in `incompatible_protocol_message`.
#[test]
fn version_incompatible_hello_keeps_its_code() {
    let message = "incompatible protocol: client offered 1.4.0, server requires 0.9.x; \
        update the phux server so protocol major.minor match";
    let (_, incompatible) = negotiate_against(|_| FrameKind::Error {
        request_id: None,
        code: ErrorCode::VersionIncompatible,
        message: message.to_owned(),
    });
    assert!(
        matches!(
            &incompatible,
            Err(SessionError::IncompatibleProtocol(text)) if text == message
        ),
        "{incompatible:?}"
    );
    let (_, refused) = negotiate_against(|_| FrameKind::Error {
        request_id: None,
        code: ErrorCode::PermissionDenied,
        message: message.to_owned(),
    });
    assert!(
        matches!(&refused, Err(SessionError::Refused(text)) if text == message),
        "{refused:?}"
    );
}

#[test]
fn a_hello_error_is_a_refusal_and_any_other_reply_is_skew() {
    let (_, refused) = negotiate_against(|_| FrameKind::Error {
        request_id: None,
        code: ErrorCode::PermissionDenied,
        message: "not on this socket".to_owned(),
    });
    assert!(
        matches!(&refused, Err(SessionError::Refused(message)) if message == "not on this socket"),
        "{refused:?}"
    );
    let (_, skewed) = negotiate_against(|_| FrameKind::Pong { nonce: 1 });
    assert!(
        matches!(&skewed, Err(SessionError::Protocol(message)) if message.contains("unexpected HELLO reply")),
        "{skewed:?}"
    );
}

#[test]
fn a_hello_ok_outside_the_offer_is_a_protocol_error() {
    let (client, refused) = negotiate_against(|offer| {
        let FrameKind::HelloOk {
            server_caps,
            server_id,
            selected_profile,
            bootstrap_limits,
            ..
        } = hello_ok(offer, phux_protocol::caps::ServerCapabilities::new())
        else {
            unreachable!("hello_ok builds HELLO_OK");
        };
        FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major.wrapping_add(1),
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps,
            server_id,
            selected_profile,
            bootstrap_limits,
        }
    });
    assert!(
        matches!(refused, Err(SessionError::Protocol(_))),
        "{refused:?}"
    );
    assert!(client.server().is_none(), "nothing negotiated on a refusal");
}

#[test]
fn attach_ids_are_non_zero_and_wrap_past_zero() {
    let (mut client, _peer) = block_on(async { pair() });
    assert_eq!(client.next_attach_id(), 1);
    assert_eq!(client.next_attach_id(), 2);
    client.next_attach_id = u32::MAX;
    assert_eq!(client.next_attach_id(), u32::MAX);
    assert_eq!(client.next_attach_id(), 1, "zero is never handed out");
}

// ----- correlated requests -----------------------------------------------
