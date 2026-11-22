use super::*;

/// A bearer upgrade round-trips a frame and stamps the credential; a
/// message longer than the frame it declares is malformed, not a batch.
#[tokio::test]
async fn valid_token_upgrades_and_rejects_trailing_bytes() {
    let (listener, addr, token_hex, _tokens) = token_listener().await;
    let overlong = [0, 0, 0, 3, 0xde, 0xad, 0xbe, 0xff, 0xff];
    let (reads, peer, _) = exchange(
        &listener,
        addr,
        bearer_request(addr, &token_hex),
        &[&FRAME, &overlong],
    )
    .await;
    let mut reads = reads.into_iter();
    assert_eq!(reads.next().unwrap().unwrap().unwrap().as_ref(), &FRAME);
    let err = reads.next().unwrap().expect_err("overlong message");
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);

    assert_eq!(peer.transport, TransportType::WebSocket);
    assert_eq!(peer.source_addr, Some(addr.ip()));
    assert_eq!(peer.mcp_host_key.as_deref(), Some("test-credential"));
    assert_eq!(
        (peer.uid, peer.pid, peer.exe_path.as_ref()),
        (0, None, None)
    );
    let credential = peer.credential.as_ref().expect("credential retained");
    assert_eq!(credential.id, "test-credential");
    assert_eq!(credential.principal, "test-principal");
    assert_eq!(credential.scopes, [crate::auth::TERMINAL_CONTROL_SCOPE]);
    assert_eq!(credential.generation, 1);
    assert!(credential.expires_at.is_none());
}

#[tokio::test]
async fn browser_subprotocol_auth_stamps_identity_without_echoing_secret() {
    let (listener, addr, token, _tokens) = token_listener().await;
    let request = browser_request(addr, &format!("phux.v1, phux.bearer.{token}"));
    let (reads, peer, response) = exchange(&listener, addr, request, &[&FRAME]).await;
    assert_eq!(response.headers()["sec-websocket-protocol"], "phux.v1");
    assert!(!format!("{:?}", response.headers()).contains(&token));
    assert_eq!(
        reads[0].as_ref().unwrap().as_ref().unwrap().as_ref(),
        &FRAME
    );
    assert_eq!(peer.credential.unwrap().id, "test-credential");
}

#[tokio::test]
async fn browser_auth_rejects_ambiguous_and_malformed_carriers() {
    let (listener, addr, token, _tokens) = token_listener().await;
    for protocols in [
        format!("phux.bearer.{token}"),
        "phux.v1, phux.bearer.not-hex".to_owned(),
        format!("phux.v1, phux.bearer.{token}, phux.bearer.{token}"),
        format!("phux.v1, phux.v1, phux.bearer.{token}"),
        "phux.v1".to_owned(),
    ] {
        let (error, response) =
            refused_handshake(&listener, addr, browser_request(addr, &protocols)).await;
        assert_generic_unauthorized(response);
        assert!(!error.contains(&token));
    }
    let mut mixed = bearer_request(addr, &token);
    mixed.headers_mut().insert(
        "sec-websocket-protocol",
        format!("phux.v1, phux.bearer.{token}").parse().unwrap(),
    );
    let mut repeated = bearer_request(addr, &token);
    repeated
        .headers_mut()
        .append("authorization", format!("Bearer {token}").parse().unwrap());
    for request in [mixed, repeated] {
        let (_, response) = refused_handshake(&listener, addr, request).await;
        assert_generic_unauthorized(response);
    }
}

#[tokio::test]
async fn invalid_malformed_and_missing_tokens_are_identical_safe_rejections() {
    let (listener, addr, _token_hex, _tokens) = token_listener().await;
    let wrong = hex::encode([0x22u8; crate::auth::TOKEN_LEN]);
    let mut errors = Vec::new();
    for request in [
        bearer_request(addr, &wrong),
        bearer_request(addr, "not-hex"),
        request(addr),
    ] {
        let (error, response) = refused_handshake(&listener, addr, request).await;
        assert_generic_unauthorized(response);
        errors.push(error);
    }
    let expected = format!(
        "WebSocket pairing authentication rejected (source_ip={})",
        addr.ip()
    );
    for error in &errors {
        assert_eq!(error, &expected);
    }
    assert!(!expected.contains(&addr.port().to_string()));
}

/// ADR-0081: `phux pair` needs no restart, so a token minted after the
/// listener bound upgrades against the running listener.
#[tokio::test]
async fn a_token_minted_after_bind_upgrades_without_a_restart() {
    let (listener, addr, _token_hex, tokens) = token_listener().await;
    let paired_bytes = [0x33u8; crate::auth::TOKEN_LEN];
    let paired = hex::encode(paired_bytes);

    let (_, response) = refused_handshake(&listener, addr, bearer_request(addr, &paired)).await;
    assert_generic_unauthorized(response);

    crate::auth::write_test_credential(tokens.path(), &paired_bytes);
    exchange(&listener, addr, bearer_request(addr, &paired), &[]).await;
}
