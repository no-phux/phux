use super::*;

// --- L3 metadata (SPEC §7.4) ---

/// An attached L3 client and its mailbox (subscriptions capture it).
fn attach_l3_client(
    s: &mut ServerState,
) -> (ClientId, mpsc::Sender<Outbound>, mpsc::Receiver<Outbound>) {
    let _ = s.seed_session("default");
    let cid = s.new_client_id();
    let (tx, rx) = mpsc::channel::<Outbound>(DEFAULT_CLIENT_MAILBOX);
    s.attach_default_caps(cid, "default", tx.clone()).unwrap();
    s.set_client_layers(cid, LayerSet::all());
    (cid, tx, rx)
}

fn attach_l1_only_client(
    s: &mut ServerState,
) -> (ClientId, mpsc::Sender<Outbound>, mpsc::Receiver<Outbound>) {
    let cid = s.new_client_id();
    let (tx, rx) = mpsc::channel::<Outbound>(DEFAULT_CLIENT_MAILBOX);
    s.attach_default_caps(cid, "default", tx.clone()).unwrap();
    s.set_client_layers(cid, LayerSet::new());
    (cid, tx, rx)
}

/// A headless consumer: subscribes without attaching or HELLO
/// (defaults to all layers).
fn unattached_l3_client(
    s: &mut ServerState,
) -> (ClientId, mpsc::Sender<Outbound>, mpsc::Receiver<Outbound>) {
    let cid = s.new_client_id();
    let (tx, rx) = mpsc::channel::<Outbound>(DEFAULT_CLIENT_MAILBOX);
    (cid, tx, rx)
}

/// Pull every queued frame off `rx` and return the inner frames.
fn drain_frames(rx: &mut mpsc::Receiver<Outbound>) -> Vec<FrameKind> {
    let mut out = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        let Outbound::Frame(f) = msg else {
            panic!("unexpected terminal outbound sentinel")
        };
        out.push(f);
    }
    out
}

#[test]
fn metadata_subscribe_then_set_broadcasts_matching_key() {
    let mut s = ServerState::new();
    let (cid, tx, mut rx) = attach_l3_client(&mut s);
    let scope = Scope::Group(DEFAULT_GROUP_ID);

    s.metadata_subscribe(cid, scope.clone(), "phux.tui.layout/v1".to_owned(), tx);
    let delivered = s.metadata_set(&scope, "phux.tui.layout/v1", b"value-1".to_vec());

    assert_eq!(delivered, vec![cid]);
    let frames = drain_frames(&mut rx);
    assert_eq!(frames.len(), 1);
    match &frames[0] {
        FrameKind::MetadataChanged {
            scope: s2,
            key,
            value,
            ..
        } => {
            assert_eq!(s2, &scope);
            assert_eq!(key, "phux.tui.layout/v1");
            assert_eq!(value.as_deref(), Some(b"value-1".as_slice()));
        }
        other => panic!("expected MetadataChanged, got {other:?}"),
    }
}

/// An unattached subscriber (`phux watch`) still receives fanout.
#[test]
fn metadata_broadcast_reaches_a_subscriber_that_never_attached() {
    let mut s = ServerState::new();
    let (cid, tx, mut rx) = unattached_l3_client(&mut s);
    let scope = Scope::Resource(phux_protocol::ids::ResourceId::local(7));
    let key = phux_protocol::wire::frame::RESOURCE_AGENT_KEY;

    s.metadata_subscribe(cid, scope.clone(), key.to_owned(), tx);
    let record = br#"{"name":"claude","kind":"claude","state":"blocked"}"#.to_vec();
    let delivered = s.metadata_set(&scope, key, record.clone());

    assert_eq!(
        delivered,
        vec![cid],
        "a subscriber with no `attached` entry must still be a fanout target",
    );
    let frames = drain_frames(&mut rx);
    assert_eq!(frames.len(), 1);
    match &frames[0] {
        FrameKind::MetadataChanged {
            scope: s2,
            key: k,
            value,
            ..
        } => {
            assert_eq!(s2, &scope);
            assert_eq!(k, key);
            assert_eq!(value.as_deref(), Some(record.as_slice()));
        }
        other => panic!("expected MetadataChanged, got {other:?}"),
    }
}

/// ...including DELETE tombstones.
#[test]
fn metadata_tombstone_reaches_a_subscriber_that_never_attached() {
    let mut s = ServerState::new();
    let (cid, tx, mut rx) = unattached_l3_client(&mut s);
    let scope = Scope::Resource(phux_protocol::ids::ResourceId::local(7));
    let key = phux_protocol::wire::frame::RESOURCE_AGENT_KEY;

    s.metadata_subscribe(cid, scope.clone(), key.to_owned(), tx);
    s.metadata_set(&scope, key, br#"{"name":"claude"}"#.to_vec());
    drain_frames(&mut rx);

    let delivered = s.metadata_delete(&scope, key);
    assert_eq!(delivered, vec![cid]);
    let frames = drain_frames(&mut rx);
    assert_eq!(frames.len(), 1);
    assert!(matches!(
        &frames[0],
        FrameKind::MetadataChanged { value: None, .. }
    ));
}

/// Detach forgets the captured mailbox, bounding the map.
#[test]
fn detach_forgets_the_captured_metadata_mailbox() {
    let mut s = ServerState::new();
    let (cid, tx, mut rx) = unattached_l3_client(&mut s);
    let scope = Scope::Global;
    s.metadata_subscribe(cid, scope.clone(), "phux.k/v1".to_owned(), tx);

    s.detach(cid);

    let delivered = s.metadata_set(&scope, "phux.k/v1", b"v".to_vec());
    assert!(delivered.is_empty());
    assert!(drain_frames(&mut rx).is_empty());
}

#[test]
fn metadata_set_on_different_key_does_not_fan_to_subscriber() {
    let mut s = ServerState::new();
    let (cid, tx, mut rx) = attach_l3_client(&mut s);
    let scope = Scope::Group(DEFAULT_GROUP_ID);

    s.metadata_subscribe(cid, scope.clone(), "phux.a/v1".to_owned(), tx);
    let delivered = s.metadata_set(&scope, "phux.b/v1", b"x".to_vec());

    assert!(delivered.is_empty(), "no subscriber for the b/v1 key");
    assert!(drain_frames(&mut rx).is_empty());
}

#[test]
fn metadata_scope_isolation_terminal_vs_group_vs_global() {
    let mut s = ServerState::new();
    let (cid, tx, mut rx) = attach_l3_client(&mut s);
    let key = "phux.same/v1";
    let t_scope = Scope::Resource(phux_protocol::ids::ResourceId::local(7));
    let c_scope = Scope::Group(DEFAULT_GROUP_ID);
    let g_scope = Scope::Global;

    // Only subscribe to Group.
    s.metadata_subscribe(cid, c_scope.clone(), key.to_owned(), tx);

    // Writes to Terminal and Global must NOT fire the subscriber.
    assert!(s.metadata_set(&t_scope, key, b"t".to_vec()).is_empty());
    assert!(s.metadata_set(&g_scope, key, b"g".to_vec()).is_empty());

    // Write to Group MUST fire it.
    let delivered = s.metadata_set(&c_scope, key, b"c".to_vec());
    assert_eq!(delivered, vec![cid]);

    // And the receiver MUST see exactly one frame (for Group).
    let frames = drain_frames(&mut rx);
    assert_eq!(frames.len(), 1);
    match &frames[0] {
        FrameKind::MetadataChanged { scope, value, .. } => {
            assert_eq!(scope, &c_scope);
            assert_eq!(value.as_deref(), Some(b"c".as_slice()));
        }
        other => panic!("expected MetadataChanged, got {other:?}"),
    }
}

#[test]
fn metadata_delete_emits_tombstone_only_if_key_existed() {
    let mut s = ServerState::new();
    let (cid, tx, mut rx) = attach_l3_client(&mut s);
    let scope = Scope::Global;

    s.metadata_subscribe(cid, scope.clone(), "phux.k/v1".to_owned(), tx);

    // Deleting a missing key is idempotent and silent.
    let delivered = s.metadata_delete(&scope, "phux.k/v1");
    assert!(delivered.is_empty());
    assert!(drain_frames(&mut rx).is_empty());

    // After a SET, DELETE fires the tombstone.
    s.metadata_set(&scope, "phux.k/v1", b"v".to_vec());
    drain_frames(&mut rx); // consume the SET broadcast

    let delivered = s.metadata_delete(&scope, "phux.k/v1");
    assert_eq!(delivered, vec![cid]);
    let frames = drain_frames(&mut rx);
    assert_eq!(frames.len(), 1);
    match &frames[0] {
        FrameKind::MetadataChanged {
            value: None,
            key,
            scope: s2,
            ..
        } => {
            assert_eq!(key, "phux.k/v1");
            assert_eq!(s2, &scope);
        }
        other => panic!("expected tombstone MetadataChanged, got {other:?}"),
    }
}

#[test]
fn metadata_set_unchanged_value_does_not_broadcast() {
    let mut s = ServerState::new();
    let (cid, tx, mut rx) = attach_l3_client(&mut s);
    let scope = Scope::Global;
    s.metadata_subscribe(cid, scope.clone(), "phux.k/v1".to_owned(), tx);

    let first = s.metadata_set(&scope, "phux.k/v1", b"v".to_vec());
    assert_eq!(first, vec![cid]);
    drain_frames(&mut rx);

    let second = s.metadata_set(&scope, "phux.k/v1", b"v".to_vec());
    assert!(second.is_empty(), "no broadcast on identical write");
    assert!(drain_frames(&mut rx).is_empty());
}

#[test]
fn non_l3_consumer_does_not_receive_metadata_changed() {
    // SPEC §16.4: non-L3 clients see no L3 frames.
    let mut s = ServerState::new();
    let (l3_cid, l3_tx, mut l3_rx) = attach_l3_client(&mut s);
    let (l1_cid, l1_tx, mut l1_rx) = attach_l1_only_client(&mut s);
    let scope = Scope::Global;

    s.metadata_subscribe(l3_cid, scope.clone(), "phux.k/v1".to_owned(), l3_tx);
    // The runtime refuses this subscribe; simulate a bypass.
    s.metadata_subscribe(l1_cid, scope.clone(), "phux.k/v1".to_owned(), l1_tx);

    let delivered = s.metadata_set(&scope, "phux.k/v1", b"v".to_vec());
    // Only the L3 client makes it through.
    assert_eq!(delivered, vec![l3_cid]);
    assert_eq!(drain_frames(&mut l3_rx).len(), 1);
    assert!(drain_frames(&mut l1_rx).is_empty());
}

#[test]
fn detach_drops_metadata_subscriptions() {
    let mut s = ServerState::new();
    let (cid, tx, mut rx) = attach_l3_client(&mut s);
    let scope = Scope::Global;
    s.metadata_subscribe(cid, scope.clone(), "phux.k/v1".to_owned(), tx);

    s.detach(cid);

    let delivered = s.metadata_set(&scope, "phux.k/v1", b"v".to_vec());
    assert!(delivered.is_empty());
    assert!(drain_frames(&mut rx).is_empty());
}

/// `detach` keeps connection-scoped state (negotiated layers, peer
/// identity), which a DETACH does not end and HELLO cannot restore.
#[test]
fn detach_keeps_negotiated_layers_and_peer_identity() {
    let mut s = ServerState::new();
    let (cid, _tx, _rx) = attach_l3_client(&mut s);
    let (l1_cid, _l1_tx, _l1_rx) = attach_l1_only_client(&mut s);
    s.set_connection_identity(
        cid,
        crate::auth::ConnectionIdentity {
            ssh_origin: None,
            bearer: None,
            peer: phux_protocol::policy::PeerIdentity {
                uid: 0,
                pid: None,
                exe_path: None,
                mcp_host_key: Some("credential-id".to_owned()),
                transport: phux_protocol::policy::TransportType::WebSocket,
                source_addr: None,
            },
            credential: Some(crate::auth::AuthenticatedCredential {
                id: "credential-id".to_owned(),
                principal: "device:cockpit".to_owned(),
                scopes: vec![crate::auth::TERMINAL_CONTROL_SCOPE.to_owned()],
                issued_at: chrono::Utc::now(),
                expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                generation: 7,
                registry_instance: None,
            }),
        },
    );
    s.set_peer_identity(
        l1_cid,
        phux_protocol::policy::PeerIdentity {
            uid: 501,
            pid: Some(4242),
            exe_path: None,
            mcp_host_key: None,
            transport: phux_protocol::policy::TransportType::UnixSocket,
            source_addr: None,
        },
    );

    s.detach(cid);
    s.detach(l1_cid);

    assert!(
        !s.client_speaks_l3(l1_cid),
        "an L1-only peer must not be promoted to L3 by detaching",
    );
    assert_eq!(s.client_layers(cid), LayerSet::all());
    let credential = s
        .authenticated_credential(cid)
        .expect("detach retains connection credential attestation");
    assert_eq!(credential.id, "credential-id");
    assert_eq!(credential.principal, "device:cockpit");
    assert_eq!(credential.scopes, [crate::auth::TERMINAL_CONTROL_SCOPE]);
    assert_eq!(credential.generation, 7);
    assert!(credential.expires_at.is_some());
    assert_eq!(
        s.peer_identity(l1_cid).map(|peer| peer.transport),
        Some(phux_protocol::policy::TransportType::UnixSocket),
        "the SHUTDOWN local-transport gate keys on this",
    );
}

/// Transport close forgets both.
#[test]
fn forget_connection_drops_negotiated_layers_and_peer_identity() {
    let mut s = ServerState::new();
    let (cid, _tx, _rx) = attach_l1_only_client_seeded(&mut s);
    s.set_peer_identity(
        cid,
        phux_protocol::policy::PeerIdentity {
            uid: 501,
            pid: None,
            exe_path: None,
            mcp_host_key: None,
            transport: phux_protocol::policy::TransportType::UnixSocket,
            source_addr: None,
        },
    );

    s.forget_connection(cid);

    assert!(s.peer_identity(cid).is_none());
    assert_eq!(s.client_layers(cid), LayerSet::all());
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
fn metadata_list_returns_keys_sorted_and_scope_isolated() {
    let mut s = ServerState::new();
    let scope_a = Scope::Group(DEFAULT_GROUP_ID);
    let scope_b = Scope::Global;

    s.metadata_set(&scope_a, "zeta", b"z".to_vec());
    s.metadata_set(&scope_a, "alpha", b"a".to_vec());
    s.metadata_set(&scope_a, "mu", b"m".to_vec());
    s.metadata_set(&scope_b, "global-only", b"g".to_vec());

    let keys_a = s.metadata().list(&scope_a);
    assert_eq!(keys_a, vec!["alpha", "mu", "zeta"]);
    let keys_b = s.metadata().list(&scope_b);
    assert_eq!(keys_b, vec!["global-only"]);
}

#[test]
fn metadata_get_returns_stored_value_or_none() {
    let mut s = ServerState::new();
    let scope = Scope::Group(DEFAULT_GROUP_ID);
    s.metadata_set(&scope, "k", b"v".to_vec());
    assert_eq!(s.metadata().get(&scope, "k"), Some(b"v".to_vec()));
    assert_eq!(s.metadata().get(&scope, "missing"), None);
    // Wrong scope: same key returns None.
    assert_eq!(s.metadata().get(&Scope::Global, "k"), None);
}
