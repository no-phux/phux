use super::*;

#[test]
fn parse_token_hex_roundtrips_and_rejects_garbage() {
    let raw = [0xde, 0xad, 0xbe, 0xef];
    let hexed = hex::encode(raw);
    assert_eq!(parse_token_hex(&hexed).expect("valid hex"), raw);
    assert_eq!(
        parse_token_hex(&format!("  {hexed}\n")).expect("trimmed"),
        raw
    );
    assert!(parse_token_hex("nothex!!").is_err());
}

#[test]
fn handshake_errors_classify_timeout_as_unreachable() {
    let addr: SocketAddr = "127.0.0.1:4433".parse().expect("addr");
    let err = handshake_error(addr, &quinn::ConnectionError::TimedOut);
    assert!(matches!(err, DialError::Unreachable(_)), "got {err:?}");
    assert_eq!(
        err.to_string(),
        "transport connect error: QUIC handshake with 127.0.0.1:4433: timed out"
    );
    let err = handshake_error(addr, &quinn::ConnectionError::VersionMismatch);
    assert!(matches!(err, DialError::Connect(_)), "got {err:?}");
}

fn application_close(code: u32, reason: &'static [u8]) -> quinn::ConnectionError {
    quinn::ConnectionError::ApplicationClosed(quinn::ApplicationClose {
        error_code: quinn::VarInt::from_u32(code),
        reason: reason.into(),
    })
}

#[test]
fn only_auth_failed_closes_are_credential_refusal() {
    for reason in [&b"unauthorized"[..], b""] {
        let err = close_error(&application_close(AUTH_FAILED_CODE, reason));
        assert!(
            matches!(err, DialError::AuthRefused(ref r) if r == "unauthorized"),
            "got {err:?}"
        );
    }
    for err in [
        application_close(0, b"bye"),
        application_close(AUTH_FAILED_CODE, b"stream timeout"),
        quinn::ConnectionError::TimedOut,
        quinn::ConnectionError::Reset,
        quinn::ConnectionError::LocallyClosed,
        quinn::ConnectionError::VersionMismatch,
    ] {
        let classified = close_error(&err);
        assert!(
            !matches!(classified, DialError::AuthRefused(_)),
            "{err:?} became {classified:?}"
        );
    }
}

/// Dial a loopback peer that reads the bearer preamble and then closes
/// with `code` / `reason`; return how the client classifies the close.
async fn dial_and_get_closed(code: u32, reason: &'static [u8]) -> DialError {
    let dir = tempfile::tempdir().expect("tempdir");
    let endpoint = crate::testing::quic_server(dir.path(), QUIC_ALPN);
    let addr = endpoint.local_addr().expect("local");
    let server = tokio::spawn(async move {
        let conn = endpoint
            .accept()
            .await
            .expect("incoming")
            .await
            .expect("handshake");
        let (_send, mut recv) = conn.accept_bi().await.expect("stream");
        let mut token = [0u8; 4 + 32];
        recv.read_exact(&mut token).await.expect("preamble");
        conn.close(code.into(), reason);
        conn.closed().await;
    });

    let dialed = dial(&QuicDial {
        addr,
        server_name: "localhost".to_owned(),
        token: Some(vec![0xAB; 32]),
        trust: CertTrust::SkipVerify,
        identity: None,
        inner: None,
    })
    .await;
    let classified = match dialed {
        Err(err) => err,
        // The write can finish before the close arrives; the close
        // reason then comes from the connection, not the stream error.
        Ok((_endpoint, conn, _send, mut recv)) => {
            let mut buf = [0u8; 1];
            let _ = recv.read(&mut buf).await;
            close_error(&conn.closed().await)
        }
    };
    server.await.expect("server");
    classified
}

#[tokio::test]
async fn refused_token_surfaces_auth_close_not_connection_lost() {
    let classified = dial_and_get_closed(AUTH_FAILED_CODE, b"unauthorized").await;
    assert!(
        matches!(classified, DialError::AuthRefused(ref reason) if reason == "unauthorized"),
        "got {classified:?}"
    );
    assert!(!classified.to_string().contains("connection lost"));
}

/// The authority downgrade over QUIC: a client requiring paired authority fails
/// the handshake against a listener that does not request a client
/// certificate, so no bearer preamble and no phux frame is ever sent.
#[tokio::test]
async fn require_paired_refuses_a_quic_listener_that_did_not_ask() {
    let dir = tempfile::tempdir().expect("tempdir");
    let endpoint = crate::testing::quic_server(dir.path(), QUIC_ALPN);
    let addr = endpoint.local_addr().expect("local");
    let server = tokio::spawn(async move {
        let incoming = endpoint.accept().await.expect("incoming");
        incoming.await.is_ok()
    });
    let (certificate, private_key) = (dir.path().join("client.pem"), dir.path().join("c.key"));
    crate::cert::ensure_self_signed(&certificate, &private_key).expect("client pair");

    let dialed = dial(&QuicDial {
        addr,
        server_name: "localhost".to_owned(),
        token: Some(vec![0xAB; 32]),
        trust: CertTrust::Pinned(
            crate::cert::cert_fingerprint(&dir.path().join("cert.pem")).expect("pin"),
        ),
        identity: Some(TlsClientIdentity::RequirePaired {
            certificate,
            private_key,
        }),
        inner: None,
    })
    .await;
    let Err(err) = dialed else {
        panic!("a listener that did not ask for the certificate was accepted");
    };
    assert!(
        err.to_string().contains(crate::tls::DOWNGRADE_REFUSED),
        "got {err:?}"
    );
    assert!(
        !server.await.expect("server"),
        "the server never completes the handshake"
    );
}

/// A relayed dial opens its stream with the inner `ClientHello`, also to a
/// routable relay address: the connector's first-byte gate refuses any
/// preamble. Loopback skips the preamble anyway, so the relay is dialed at
/// its v4-mapped address, which `is_loopback` does not count as loopback.
#[tokio::test]
async fn relayed_dial_to_a_routable_relay_starts_with_the_client_hello() {
    let dir = tempfile::tempdir().expect("tempdir");
    let endpoint = crate::testing::quic_server(dir.path(), QUIC_ALPN);
    let port = endpoint.local_addr().expect("local").port();
    let server = tokio::spawn(async move {
        let conn = endpoint
            .accept()
            .await
            .expect("incoming")
            .await
            .expect("handshake");
        let (_send, mut recv) = conn.accept_bi().await.expect("stream");
        let mut first = [0u8; 1];
        recv.read_exact(&mut first).await.expect("first byte");
        first[0]
    });

    let addr: SocketAddr = format!("[::ffff:127.0.0.1]:{port}").parse().expect("addr");
    assert!(!addr.ip().is_loopback());
    let plan = QuicDial {
        addr,
        server_name: "localhost".to_owned(),
        token: None,
        trust: CertTrust::SkipVerify,
        identity: None,
        inner: None,
    };
    let inner = InnerTls {
        trust: CertTrust::SkipVerify,
        identity: TlsClientIdentity::None,
        server_name: "route".to_owned(),
    };
    // The server never answers the inner handshake; only the first byte matters.
    let client = tokio::spawn(async move { dial_relayed(&plan, &inner, QUIC_ALPN).await.is_ok() });
    assert_eq!(
        server.await.expect("server"),
        0x16,
        "a TLS handshake record"
    );
    client.abort();
}
