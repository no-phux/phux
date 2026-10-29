use super::*;

/// One mTLS WebSocket attempt: the admitted identity, or `None` when the
/// listener refused it (and the client's dial then failed).
async fn mtls_attempt(
    listener: &WsListener,
    url: &str,
    token: &str,
    identity: &phux_dial::TlsClientIdentity,
) -> Option<crate::auth::ConnectionIdentity> {
    let dial = phux_dial::WsDial {
        url: url.to_owned(),
        token: Some(token.to_owned()),
        trust: phux_dial::CertTrust::SkipVerify,
        tls_server_name: None,
        identity: None,
    };
    let (accepted, dialed) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(
            listener.accept(),
            phux_dial::ws::dial_with_identity(&dial, identity)
        )
    })
    .await
    .expect("the attempt settles");
    let admitted = accepted.ok().map(|(_, _, admitted)| admitted);
    assert_eq!(dialed.is_ok(), admitted.is_some());
    admitted
}

/// With a workload CA, WSS maps the client certificate through the
/// registry exactly as QUIC does: the token stays outer admission, an
/// enrolled certificate becomes the credential, and a missing
/// certificate, wrong token, or revoked credential is refused live.
#[tokio::test]
async fn wss_with_a_configured_ca_looks_up_the_client_certificate_like_quic() {
    use crate::workload::{
        ClientMaterial, ReloadingWorkloadRegistry, WorkloadPaths, WorkloadRegistry,
    };
    let dir = tempfile::tempdir().unwrap();
    let leaf = dir.path().join("leaf.pem");
    let leaf_key = dir.path().join("leaf-key.pem");
    crate::transport::tls::ensure_self_signed_for(&leaf, &leaf_key, &["localhost".to_owned()])
        .unwrap();
    let paths = WorkloadPaths {
        ca_cert: dir.path().join("ca.pem"),
        ca_key: dir.path().join("ca.key"),
        registry: dir.path().join("workload-keys"),
    };
    crate::workload::init_authority(&paths.ca_cert, &paths.ca_key).unwrap();

    let client_key = rcgen::KeyPair::generate().unwrap();
    let csr = rcgen::CertificateParams::new(vec!["client".to_owned()])
        .unwrap()
        .serialize_request(&client_key)
        .unwrap();
    let material = ClientMaterial::from_pem(csr.pem().unwrap().as_bytes()).unwrap();
    let expires = chrono::Utc::now().timestamp() + 3600;
    let prepared = crate::workload::prepare_enrollment(&paths, &material, expires).unwrap();
    let client_cert = dir.path().join("client.pem");
    let client_key_path = dir.path().join("client.key");
    std::fs::write(&client_cert, prepared.issued_chain_pem().unwrap()).unwrap();
    std::fs::write(&client_key_path, client_key.serialize_pem()).unwrap();
    let enrolled = prepared
        .commit(&paths.registry, vec!["*@global".to_owned()], expires)
        .unwrap();

    let tokens = dir.path().join("tokens.json");
    crate::auth::write_test_credential(&tokens, &TEST_TOKEN);
    let acceptor = crate::transport::tls::acceptor_from_pem_with_client_ca(
        &leaf,
        &leaf_key,
        Some(&crate::workload::authority_certificate(&paths.ca_cert).unwrap()),
    )
    .unwrap();
    let listener = WsListener::bind_secure(
        "127.0.0.1:0".parse().unwrap(),
        acceptor,
        Arc::new(crate::auth::ReloadingTokenStore::load(tokens).unwrap()),
        Some(Arc::new(
            ReloadingWorkloadRegistry::load(paths.registry.clone()).unwrap(),
        )),
    )
    .await
    .unwrap();
    let url = format!("wss://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let token = hex::encode(TEST_TOKEN);
    let enrolled_identity = phux_dial::TlsClientIdentity::PemFiles {
        certificate: client_cert,
        private_key: client_key_path,
    };

    let admitted = mtls_attempt(&listener, &url, &token, &enrolled_identity)
        .await
        .expect("an enrolled certificate with a valid token is admitted");
    let credential = admitted.credential.as_ref().expect("workload credential");
    assert_eq!(credential.id, enrolled.id);
    assert_eq!(credential.generation, 1);
    assert_eq!(credential.scopes, ["*@global"]);
    assert_eq!(admitted.mcp_host_key.as_deref(), Some(enrolled.id.as_str()));

    let anonymous = phux_dial::TlsClientIdentity::None;
    assert!(
        mtls_attempt(&listener, &url, &token, &anonymous)
            .await
            .is_none(),
        "no client certificate"
    );
    let wrong_token = hex::encode([0x22; crate::auth::TOKEN_LEN]);
    assert!(
        mtls_attempt(&listener, &url, &wrong_token, &enrolled_identity)
            .await
            .is_none(),
        "the pairing token stays outer admission"
    );
    WorkloadRegistry::revoke(&paths.registry, &enrolled.id).unwrap();
    assert!(
        mtls_attempt(&listener, &url, &token, &enrolled_identity)
            .await
            .is_none(),
        "revocation applies to the next connection without a restart"
    );
}
