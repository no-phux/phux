use super::*;

// --- L3 metadata ---

/// An attached L3 client and its mailbox (subscriptions capture it).
fn attach_l3_client(
    s: &mut ServerState,
) -> (ClientId, mpsc::Sender<Outbound>, mpsc::Receiver<Outbound>) {
    let _ = s.seed_session("default");
    let cid = s.clients_mut().new_client_id();
    let (tx, rx) = mpsc::channel::<Outbound>(DEFAULT_CLIENT_MAILBOX);
    s.attach_default_caps(cid, "default", tx.clone()).unwrap();
    s.clients_mut().set_layers(cid, LayerSet::all());
    (cid, tx, rx)
}

fn attach_l1_only_client(
    s: &mut ServerState,
) -> (ClientId, mpsc::Sender<Outbound>, mpsc::Receiver<Outbound>) {
    let cid = s.clients_mut().new_client_id();
    let (tx, rx) = mpsc::channel::<Outbound>(DEFAULT_CLIENT_MAILBOX);
    s.attach_default_caps(cid, "default", tx.clone()).unwrap();
    s.clients_mut().set_layers(cid, LayerSet::new());
    (cid, tx, rx)
}

/// A headless consumer: subscribes without attaching or HELLO
/// (defaults to all layers).
fn unattached_l3_client(
    s: &mut ServerState,
) -> (ClientId, mpsc::Sender<Outbound>, mpsc::Receiver<Outbound>) {
    let cid = s.clients_mut().new_client_id();
    let (tx, rx) = mpsc::channel::<Outbound>(DEFAULT_CLIENT_MAILBOX);
    (cid, tx, rx)
}

/// Every queued `METADATA_CHANGED` on `rx`, as `(scope, key, value)`.
fn changes(rx: &mut mpsc::Receiver<Outbound>) -> Vec<(Scope, String, Option<Vec<u8>>)> {
    let mut out = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        let Outbound::Frame(FrameKind::MetadataChanged {
            scope, key, value, ..
        }) = msg
        else {
            panic!("expected METADATA_CHANGED, got {msg:?}")
        };
        out.push((scope, key, value));
    }
    out
}

#[test]
fn metadata_set_broadcasts_only_the_subscribed_scope_and_key() {
    let mut s = ServerState::new();
    let (cid, tx, mut rx) = attach_l3_client(&mut s);
    let key = "phux.tui.layout/v1";
    let group = Scope::Group(DEFAULT_GROUP_ID);
    s.metadata_subscribe(cid, group.clone(), key.to_owned(), tx);

    let terminal = Scope::Resource(phux_protocol::ids::ResourceId::local(7));
    for other in [terminal, Scope::Global] {
        assert!(s.metadata_set(&other, key, b"t".to_vec()).is_empty());
    }
    assert!(
        s.metadata_set(&group, "phux.b/v1", b"x".to_vec())
            .is_empty()
    );
    assert_eq!(s.metadata_set(&group, key, b"value-1".to_vec()), vec![cid]);
    let expected = (group.clone(), key.to_owned(), Some(b"value-1".to_vec()));
    assert_eq!(changes(&mut rx), vec![expected]);
    assert!(
        s.metadata_set(&group, key, b"value-1".to_vec()).is_empty(),
        "unchanged value"
    );
    assert!(changes(&mut rx).is_empty());
}

/// An unattached subscriber (`phux watch`) still receives fanout, tombstones
/// included; deleting a missing key is silent.
#[test]
fn metadata_set_and_tombstone_reach_a_subscriber_that_never_attached() {
    let mut s = ServerState::new();
    let (cid, tx, mut rx) = unattached_l3_client(&mut s);
    let scope = Scope::Resource(phux_protocol::ids::ResourceId::local(7));
    let key = phux_protocol::wire::frame::RESOURCE_AGENT_KEY;
    s.metadata_subscribe(cid, scope.clone(), key.to_owned(), tx);

    assert!(s.metadata_delete(&scope, key).is_empty());
    let record = br#"{"name":"claude","kind":"claude","state":"blocked"}"#.to_vec();
    assert_eq!(s.metadata_set(&scope, key, record.clone()), vec![cid]);
    assert_eq!(s.metadata_delete(&scope, key), vec![cid]);
    assert_eq!(
        changes(&mut rx),
        vec![
            (scope.clone(), key.to_owned(), Some(record)),
            (scope, key.to_owned(), None)
        ]
    );
}

/// Detach forgets the subscription and its captured mailbox, attached or not.
#[test]
fn detach_drops_metadata_subscriptions_and_their_mailboxes() {
    let mut s = ServerState::new();
    let (attached, attached_tx, mut attached_rx) = attach_l3_client(&mut s);
    let (headless, headless_tx, mut headless_rx) = unattached_l3_client(&mut s);
    let scope = Scope::Global;
    s.metadata_subscribe(attached, scope.clone(), "phux.k/v1".to_owned(), attached_tx);
    s.metadata_subscribe(headless, scope.clone(), "phux.k/v1".to_owned(), headless_tx);
    s.detach(attached);
    s.detach(headless);
    assert!(
        s.metadata_set(&scope, "phux.k/v1", b"v".to_vec())
            .is_empty()
    );
    assert!(changes(&mut attached_rx).is_empty() && changes(&mut headless_rx).is_empty());
}

#[test]
fn non_l3_consumer_does_not_receive_metadata_changed() {
    // Non-L3 clients see no L3 frames.
    let mut s = ServerState::new();
    let (l3_cid, l3_tx, mut l3_rx) = attach_l3_client(&mut s);
    let (l1_cid, l1_tx, mut l1_rx) = attach_l1_only_client(&mut s);
    let scope = Scope::Global;

    s.metadata_subscribe(l3_cid, scope.clone(), "phux.k/v1".to_owned(), l3_tx);
    // The runtime refuses this subscribe; simulate a bypass.
    s.metadata_subscribe(l1_cid, scope.clone(), "phux.k/v1".to_owned(), l1_tx);

    let delivered = s.metadata_set(&scope, "phux.k/v1", b"v".to_vec());
    assert_eq!(delivered, vec![l3_cid]);
    assert_eq!(changes(&mut l3_rx).len(), 1);
    assert!(changes(&mut l1_rx).is_empty());
}

/// `detach` keeps connection-scoped state (negotiated layers, peer
/// identity), which a DETACH does not end and HELLO cannot restore.
#[test]
fn detach_keeps_negotiated_layers_and_peer_identity() {
    let mut s = ServerState::new();
    let (cid, _tx, _rx) = attach_l3_client(&mut s);
    let (l1_cid, _l1_tx, _l1_rx) = attach_l1_only_client(&mut s);
    s.clients_mut().set_connection_identity(
        cid,
        crate::auth::ConnectionIdentity {
            ssh_origin: None,
            bearer: None,
            peer: phux_protocol::policy::PeerIdentity {
                mcp_host_key: Some("credential-id".to_owned()),
                ..crate::test_support::peer(phux_protocol::policy::TransportType::WebSocket)
            },
            credential: Some(crate::auth::AuthenticatedCredential {
                principal: "device:cockpit".to_owned(),
                expires_at: Some(jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1)),
                generation: 7,
                ..crate::test_support::credential(
                    "credential-id",
                    &[crate::auth::TERMINAL_CONTROL_SCOPE],
                )
            }),
        },
    );
    let unix = phux_protocol::policy::TransportType::UnixSocket;
    s.clients_mut()
        .set_peer_identity(l1_cid, crate::test_support::peer(unix));

    s.detach(cid);
    s.detach(l1_cid);

    assert!(
        !s.clients().speaks_l3(l1_cid),
        "an L1-only peer must not be promoted to L3 by detaching",
    );
    assert_eq!(s.clients().layers(cid), LayerSet::all());
    let credential = s
        .clients()
        .authenticated_credential(cid)
        .expect("detach retains connection credential attestation");
    assert_eq!(credential.id, "credential-id");
    assert_eq!(credential.principal, "device:cockpit");
    assert_eq!(credential.scopes, [crate::auth::TERMINAL_CONTROL_SCOPE]);
    assert_eq!(credential.generation, 7);
    assert!(credential.expires_at.is_some());
    assert_eq!(
        s.clients().peer_identity(l1_cid).map(|peer| peer.transport),
        Some(phux_protocol::policy::TransportType::UnixSocket),
        "the SHUTDOWN local-transport gate keys on this",
    );
}

/// Transport close forgets both.
#[test]
fn forget_connection_drops_negotiated_layers_and_peer_identity() {
    let mut s = ServerState::new();
    let (cid, _tx, _rx) = attach_l1_only_client_seeded(&mut s);
    let unix = phux_protocol::policy::TransportType::UnixSocket;
    s.clients_mut()
        .set_peer_identity(cid, crate::test_support::peer(unix));

    s.forget_connection(cid);

    assert!(s.clients().peer_identity(cid).is_none());
    assert_eq!(s.clients().layers(cid), LayerSet::all());
    assert!(!s.attached().contains_key(&cid));
    // Idempotent, like `detach` — the accept loop runs it unconditionally.
    s.forget_connection(cid);
}

/// [`attach_l1_only_client`] assumes a caller already seeded `default`.
fn attach_l1_only_client_seeded(
    s: &mut ServerState,
) -> (ClientId, mpsc::Sender<Outbound>, mpsc::Receiver<Outbound>) {
    let _ = s.seed_session("default");
    attach_l1_only_client(s)
}

#[test]
fn metadata_get_and_list_are_scope_isolated_and_sorted() {
    let mut s = ServerState::new();
    let (group, global) = (Scope::Group(DEFAULT_GROUP_ID), Scope::Global);
    for key in ["zeta", "alpha", "mu"] {
        s.metadata_set(&group, key, key.as_bytes().to_vec());
    }
    s.metadata_set(&global, "global-only", b"g".to_vec());
    assert_eq!(s.metadata().list(&group), vec!["alpha", "mu", "zeta"]);
    assert_eq!(s.metadata().list(&global), vec!["global-only"]);
    assert_eq!(s.metadata().get(&group, "mu"), Some(b"mu".to_vec()));
    assert_eq!(s.metadata().get(&group, "missing"), None);
    assert_eq!(s.metadata().get(&global, "mu"), None);
}
