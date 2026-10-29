use std::path::PathBuf;

use super::*;
use crate::target::Transport;

#[test]
fn frames_are_cut_at_their_declared_length() {
    let mut pending = BytesMut::from(&b"\x00\x00\x00\x02hi\x00\x00\x00\x03ab"[..]);
    let first = take_frame(&mut pending).expect("valid").expect("complete");
    assert_eq!(&first[..], b"\x00\x00\x00\x02hi");
    // The second frame is short one byte: nothing is taken, nothing lost.
    assert!(take_frame(&mut pending).expect("valid").is_none());
    assert_eq!(&pending[..], b"\x00\x00\x00\x03ab");
    pending.extend_from_slice(b"c");
    assert_eq!(
        &take_frame(&mut pending).expect("valid").expect("complete")[..],
        b"\x00\x00\x00\x03abc"
    );
    assert!(pending.is_empty());
}

#[test]
fn zero_and_oversized_lengths_are_refused() {
    assert!(take_frame(&mut BytesMut::from(&b"\x00\x00\x00\x00"[..])).is_err());
    let oversized = u32::try_from(MAX_FRAME_BODY + 1)
        .expect("fits")
        .to_be_bytes();
    assert!(take_frame(&mut BytesMut::from(&oversized[..])).is_err());
}

#[test]
fn an_inbound_message_must_be_exactly_one_frame() {
    assert!(check_inbound("mini", b"\x00\x00\x00\x02hi").is_ok());
    for malformed in [
        &b"\x00\x00\x00"[..],
        &b"\x00\x00\x00\x03hi"[..],
        &b"\x00\x00\x00\x01hi"[..],
        &b"\x00\x00\x00\x01h\x00\x00\x00\x01i"[..],
    ] {
        let err = check_inbound("mini", malformed).expect_err("malformed");
        assert!(err.starts_with("mini sent a malformed frame"), "{err}");
    }
}

fn resolved(url: &str, pin: Option<&str>) -> Resolved {
    Resolved {
        name: "mini".to_owned(),
        endpoint: url.to_owned(),
        session: None,
        transport: Transport::Ws(url.to_owned()),
        token_file: Some(PathBuf::from("/secret/mini.token")),
        cert_fingerprint: pin.map(str::to_owned),
        tls_server_name: None,
        client_identity: phux_dial::TlsClientIdentity::None,
        authority: crate::target::AuthorityPin::default(),
    }
}

#[test]
fn routable_websocket_needs_tls_pin_and_token_like_the_cli() {
    let pin = "ab".repeat(32);
    let routable = "wss://mini.ts.net:8787";
    assert!(
        plan_ws(
            &resolved("ws://mini.ts.net:8787", Some(&pin)),
            "ws://mini.ts.net:8787",
            Some("ab".to_owned())
        )
        .is_err()
    );
    assert!(plan_ws(&resolved(routable, None), routable, Some("ab".to_owned())).is_err());
    assert!(plan_ws(&resolved(routable, Some(&pin)), routable, None).is_err());
    let ok = plan_ws(
        &resolved(routable, Some(&pin)),
        routable,
        Some(" ab ".to_owned()),
    )
    .expect("pinned");
    assert_eq!(ok.token.as_deref(), Some("ab"));
    assert_eq!(ok.trust, CertTrust::Pinned(pin));
    // Loopback is the development path: no pin, no token, plaintext.
    let local = plan_ws(
        &resolved("ws://127.0.0.1:8787", None),
        "ws://127.0.0.1:8787",
        None,
    )
    .expect("loopback");
    assert_eq!(local.trust, CertTrust::SkipVerify);
}

#[test]
fn a_malformed_token_is_reported_by_file_never_by_its_bytes() {
    let pin = "ab".repeat(32);
    let routable = "wss://mini.ts.net:8787";
    let err = plan_ws(
        &resolved(routable, Some(&pin)),
        routable,
        Some("zq-secret".to_owned()),
    )
    .expect_err("not hex");
    assert_eq!(
        err,
        "mini: the token file /secret/mini.token is not valid hex"
    );
    for leaked in ['z', 'q', 's'] {
        assert!(!err[..err.find(" the token file").expect("prefix")].contains(leaked));
    }
}

#[test]
fn routable_quic_without_a_pin_is_refused_after_resolution() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let mut unpinned = resolved("quic://192.0.2.1:8788", None);
    unpinned.transport = Transport::Quic("192.0.2.1:8788".to_owned());
    unpinned.token_file = None;
    let err = runtime
        .block_on(plan_quic(&unpinned, "192.0.2.1:8788"))
        .expect_err("unpinned");
    assert!(err.contains("certificate pin"), "{err}");
    let loopback = runtime
        .block_on(plan_quic(&unpinned, "127.0.0.1:8788"))
        .expect("loopback");
    assert_eq!(loopback.trust, CertTrust::SkipVerify);
    assert_eq!(loopback.server_name, "localhost");
}

#[test]
fn a_relay_route_rides_the_tls_server_name_on_both_lanes() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let pin = "AB".repeat(32);
    let mut routed = resolved("quic://127.0.0.1:4433", Some(&pin));
    routed.transport = Transport::Quic("127.0.0.1:4433".to_owned());
    routed.token_file = None;
    routed.tls_server_name = Some("mini-route".to_owned());
    routed.authority.route = Some("mini-route".to_owned());
    // ADR-0180: no server CA for the route, no dial; the relay would
    // read the stream.
    let refused = runtime
        .block_on(plan_quic(&routed, "127.0.0.1:4433"))
        .expect_err("a relay route without the server's CA");
    assert!(
        refused.contains("mini-route")
            && refused.contains("pin the server's CA")
            && !refused.contains("phux "),
        "{refused}"
    );

    let ca = format!("sha256:{}", "c".repeat(64));
    routed.authority.ca = Some(ca.clone());
    let quic = runtime
        .block_on(plan_quic(&routed, "127.0.0.1:4433"))
        .expect("routed quic plan");
    assert_eq!(quic.server_name, "mini-route");
    // The outer leg pins the relay and presents nothing; the server's CA
    // is verified inside the end-to-end session.
    assert_eq!(quic.trust, CertTrust::Pinned(pin));
    assert_eq!(quic.identity, Some(phux_dial::TlsClientIdentity::None));
    let inner = quic.inner.expect("an end-to-end session");
    assert_eq!(inner.trust, CertTrust::Authority { ca });
    assert_eq!(inner.server_name, "mini-route");

    let ws = plan_ws(&routed, "ws://127.0.0.1:8787", None).expect("routed ws plan");
    assert_eq!(ws.tls_server_name.as_deref(), Some("mini-route"));
}

#[test]
fn revoked_quic_token_is_re_pair_not_connection_lost() {
    let msg = dial_message(
        "stale-token",
        &DialError::AuthRefused("unauthorized".to_owned()),
    );
    assert!(
        msg.contains("refused the pairing token") && msg.contains("unauthorized"),
        "{msg}"
    );
    assert!(
        msg.contains("phux host add stale-token"),
        "B9 remedy must name re-pair: {msg}"
    );
    assert!(
        !msg.contains("connection was lost") && !msg.contains("connection lost"),
        "{msg}"
    );
}

#[test]
fn quic_auth_close_is_not_generic_loss() {
    let err = quinn::ConnectionError::ApplicationClosed(quinn::ApplicationClose {
        error_code: quinn::VarInt::from_u32(phux_dial::quic::AUTH_FAILED_CODE),
        reason: b"unauthorized".as_slice().into(),
    });
    let msg = quic_closed_message("stale-token", &err);
    assert!(msg.contains("refused the pairing token"), "{msg}");
    assert!(!msg.contains("connection was lost"), "{msg}");
    assert!(!msg.contains("connection lost"), "{msg}");
}

#[test]
fn quic_idle_timeout_is_reachability_not_auth() {
    let msg = quic_closed_message("mini", &quinn::ConnectionError::TimedOut);
    assert!(msg.contains("did not answer"), "{msg}");
    assert!(!msg.contains("pairing token"), "{msg}");
}

#[test]
fn graceful_quic_close_is_not_auth() {
    let err = quinn::ConnectionError::ApplicationClosed(quinn::ApplicationClose {
        error_code: quinn::VarInt::from_u32(0),
        reason: b"bye".as_slice().into(),
    });
    let msg = quic_closed_message("mini", &err);
    assert!(!msg.contains("pairing token"), "{msg}");
    assert!(!msg.contains("enroll"), "{msg}");
}
