//! Dialing: a loopback attach, pinned authorities, path changes, redials,
//! and an unpinned routable target failing closed.

use super::servers::QuicServer;
use super::*;

#[test]
fn quic_endpoint_attaches_over_real_loopback_quic() {
    let server = QuicServer::start();
    let remote = RemoteClient::new(format!("quic://{}", server.addr), 80, 24, None, None);
    remote.connect().expect("connect");
    remote.attach_session(SESSION.to_owned());

    wait_for(&remote, |remote| {
        remote.status() == WireStatus::Attached
            && remote.topology().is_some_and(|topology| {
                topology
                    .sessions
                    .iter()
                    .any(|session| session.name == SESSION)
            })
    });
    assert!(remote.server_protocol_version().is_some());
    remote.stop_connection();
}

/// ADR-0153 for the phone: a leaf-pinned client attaches by its leaf and
/// learns nothing (ADR-0179); pinned, a different CA is refused by name and
/// never attaches, and the right one attaches.
#[test]
fn a_pinned_phone_attaches_and_a_swapped_authority_is_refused() {
    let server = QuicServer::start_with(true);
    let leaf = phux_server::transport::tls::cert_fingerprint(&server.cert()).expect("leaf");
    let authority = phux_server::transport::tls::presented_authority(&server.cert())
        .expect("chain")
        .expect("issued by the workload CA");
    let endpoint = format!("quic://{}", server.addr);

    let leaf_only = RemoteClient::new(endpoint.clone(), 80, 24, Some(leaf.clone()), None);
    leaf_only.connect().expect("connect");
    leaf_only.attach_session(SESSION.to_owned());
    wait_for(&leaf_only, |remote| remote.status() == WireStatus::Attached);
    assert_eq!(
        leaf_only.target().expect("target").authority.ca,
        None,
        "a connection teaches no pin"
    );
    leaf_only.stop_connection();

    let swapped = RemoteClient::new(endpoint.clone(), 80, 24, Some(leaf.clone()), None);
    swapped.set_authority_pin(Some(format!("sha256:{}", "0".repeat(64))));
    swapped.connect().expect("the runtime owns the failure");
    wait_for(&swapped, |remote| {
        remote
            .last_error()
            .is_some_and(|error| error.contains("certificate authority changed"))
    });
    let error = swapped.last_error().expect("an error");
    assert!(
        error.contains(&authority),
        "names the presented CA: {error}"
    );
    assert_ne!(swapped.status(), WireStatus::Attached);
    swapped.stop_connection();

    let pinned = RemoteClient::new(endpoint, 80, 24, Some(leaf), None);
    pinned.set_authority_pin(Some(authority));
    pinned.connect().expect("connect");
    pinned.attach_session(SESSION.to_owned());
    wait_for(&pinned, |remote| remote.status() == WireStatus::Attached);
    pinned.stop_connection();
}

/// A path change migrates the QUIC connection: the next round trip lands on
/// the same incarnation, with no redial.
#[test]
fn a_path_change_keeps_the_incarnation_over_a_fresh_socket() {
    let server = QuicServer::start();
    let remote = RemoteClient::new(format!("quic://{}", server.addr), 80, 24, None, None);
    remote.connect().expect("connect");
    remote.attach_session(SESSION.to_owned());
    let mut seen = Vec::new();
    let epoch = publish_until(&remote, &mut seen, |publication| {
        publication.status == WireStatus::Attached && publication.topology.is_some()
    })
    .connection_epoch;

    remote.network_path_changed();
    remote.refresh_topology();
    let mut seen = Vec::new();
    let after = publish_until(&remote, &mut seen, |publication| {
        publication.events.contains(&WireEvent::TopologyChanged)
    });
    assert_eq!(after.connection_epoch, epoch, "no redial: {seen:?}");
    assert_eq!(after.status, WireStatus::Attached);
    remote.stop_connection();
}

#[test]
fn unpinned_routable_quic_fails_closed() {
    // TEST-NET-1: never routed, so only the pin rule can end this dial.
    let remote = RemoteClient::new("quic://192.0.2.1:8788".into(), 80, 24, None, None);
    remote.connect().expect("the runtime owns the failure");
    wait_for(&remote, |remote| {
        remote
            .last_error()
            .is_some_and(|error| error.contains("certificate pin"))
    });
    assert_ne!(remote.status(), WireStatus::Attached);
    remote.stop_connection();
}

#[test]
fn a_redial_publishes_a_new_incarnation_with_the_same_ids() {
    let server = QuicServer::start();
    let remote = RemoteClient::new(format!("quic://{}", server.addr), 80, 24, None, None);
    remote.connect().expect("connect");
    remote.attach_session(SESSION.to_owned());
    let attached = |publication: &WirePublication| {
        publication.status == WireStatus::Attached
            && publication.topology.as_ref().is_some_and(|topology| {
                topology
                    .sessions
                    .iter()
                    .any(|session| session.name == SESSION)
            })
    };
    let mut seen = Vec::new();
    let first = publish_until(&remote, &mut seen, attached);
    let epoch = first.connection_epoch;
    assert!(seen.contains(&WireEvent::ConnectionOpened {
        connection_epoch: epoch
    }));

    remote.resync();
    let mut seen = Vec::new();
    let second = publish_until(&remote, &mut seen, |publication| {
        publication.connection_epoch > epoch && attached(publication)
    });
    assert!(seen.contains(&WireEvent::ConnectionOpened {
        connection_epoch: second.connection_epoch
    }));
    // The same server hands back the same session: only the incarnation
    // tells the two attachments apart.
    let ids = |publication: &WirePublication| {
        publication
            .topology
            .as_ref()
            .map(|topology| topology.sessions.iter().map(|s| s.id).collect::<Vec<_>>())
    };
    assert_eq!(ids(&first), ids(&second));
    remote.stop_connection();
}
