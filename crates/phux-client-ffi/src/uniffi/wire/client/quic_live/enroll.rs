//! Enrollment: a phone redeems a ticket and attaches to a `paired` server,
//! directly and through a relay.

use super::servers::{QuicServer, ROUTE, RelayedServer, mint, test_key};
use super::*;

/// ADR-0154 for the phone, against a `paired` server: with no certificate
/// it is refused; it enrolls a keystore key with a ticket and attaches; the
/// ticket does not enroll a second key; and a chain presented with another
/// device's key does not complete the handshake.
#[test]
fn a_phone_enrolls_with_a_ticket_and_attaches_to_a_paired_server() {
    let (server, paths) = QuicServer::start_paired();
    let leaf = phux_server::transport::tls::cert_fingerprint(&server.cert()).expect("leaf");
    let endpoint = format!("quic://{}", server.addr);

    let bare = RemoteClient::new(endpoint.clone(), 80, 24, Some(leaf.clone()), None);
    bare.connect().expect("the runtime owns the failure");
    wait_for(&bare, |remote| remote.last_error().is_some());
    assert_ne!(
        bare.status(),
        WireStatus::Attached,
        "no certificate, no admission"
    );
    bare.stop_connection();

    let ticket = mint(&paths);
    let key = test_key();
    let authority = phux_server::workload::ca_fingerprint(&paths.ca_cert).expect("ca");
    // A phone that pins no CA is refused before it spends the ticket: the CA
    // comes from the link, never from the reply (ADR-0179).
    let unpinned = RemoteClient::new(endpoint.clone(), 80, 24, Some(leaf.clone()), None);
    let refused = unpinned
        .enroll_device(ticket.clone(), Arc::clone(&key))
        .expect_err("enrollment needs a pinned CA");
    assert!(refused.to_string().contains("`ca`"), "{refused}");
    let phone = RemoteClient::new(endpoint.clone(), 80, 24, Some(leaf.clone()), None);
    phone.set_authority_pin(Some(authority.clone()));
    let chain = phone
        .enroll_device(ticket.clone(), Arc::clone(&key))
        .expect("the ticket enrolls the device key");
    assert!(!chain.contains("PRIVATE KEY"));
    phone.connect().expect("connect");
    phone.attach_session(SESSION.to_owned());
    wait_for(&phone, |remote| remote.status() == WireStatus::Attached);
    phone.stop_connection();

    // A later launch: the stored chain, the keystore key, and the CA pin.
    let relaunched = RemoteClient::new(endpoint.clone(), 80, 24, Some(leaf.clone()), None);
    relaunched.set_authority_pin(Some(authority.clone()));
    relaunched
        .set_device_identity(chain.clone(), Arc::clone(&key))
        .expect("identity");
    relaunched.connect().expect("connect");
    relaunched.attach_session(SESSION.to_owned());
    wait_for(&relaunched, |remote| {
        remote.status() == WireStatus::Attached
    });
    relaunched.stop_connection();

    // Adversarial: the ticket is spent.
    let thief = RemoteClient::new(endpoint.clone(), 80, 24, Some(leaf.clone()), None);
    thief.set_authority_pin(Some(authority.clone()));
    let replay = thief
        .enroll_device(ticket, test_key())
        .expect_err("a consumed ticket enrolls nothing");
    assert!(
        replay.to_string().contains("enrollment refused"),
        "{replay}"
    );

    // Adversarial: the phone's (public) chain with another device's key.
    let impostor = RemoteClient::new(endpoint, 80, 24, Some(leaf), None);
    impostor.set_authority_pin(Some(authority));
    impostor
        .set_device_identity(chain, test_key())
        .expect("identity");
    impostor.connect().expect("the runtime owns the failure");
    wait_for(&impostor, |remote| remote.last_error().is_some());
    assert_ne!(impostor.status(), WireStatus::Attached);
    impostor.stop_connection();
}

/// ADR-0154 item 5 for the phone: through a relay to a `paired` server, it
/// enrolls with a ticket inside the end-to-end session and attaches; without
/// its certificate the relay route admits nothing.
#[test]
fn a_phone_enrolls_and_attaches_through_a_relay_to_a_paired_server() {
    let server = RelayedServer::start();
    let bare = server.client();
    bare.connect().expect("the runtime owns the failure");
    wait_for(&bare, |remote| remote.last_error().is_some());
    assert_ne!(bare.status(), WireStatus::Attached);
    bare.stop_connection();

    let phone = server.client();
    let deadline = Instant::now() + DEADLINE;
    let ticket = mint(&server.paths);
    let key = test_key();
    // The connector may not hold the route yet; a refused ticket would be
    // spent, so retry only while the route is offline.
    let chain = loop {
        match phone.enroll_device(ticket.clone(), Arc::clone(&key)) {
            Ok(chain) => break chain,
            Err(error) if error.to_string().contains("route offline") => {
                assert!(Instant::now() < deadline, "the route never came up");
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => panic!("enrollment through the relay: {error}"),
        }
    };
    assert!(chain.contains("BEGIN CERTIFICATE"));
    phone.connect().expect("connect");
    phone.attach_session(SESSION.to_owned());
    wait_for(&phone, |remote| remote.status() == WireStatus::Attached);
    let epoch = phone.take_publication().connection_epoch;
    let sni_before_resync = server
        .observed_sni
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .len();
    assert!(
        sni_before_resync >= 1,
        "the relay saw no consumer SNI on the first attach"
    );
    phone.resync();
    let mut seen = Vec::new();
    publish_until(&phone, &mut seen, |publication| {
        publication.connection_epoch > epoch && publication.status == WireStatus::Attached
    });
    assert_eq!(
        phone.target().expect("target").authority.route.as_deref(),
        Some(ROUTE)
    );
    // rustls read this name from the ClientHello, on the enroll/attach
    // handshakes and again on the resync redial. A stored route is not enough.
    let seen = server
        .observed_sni
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert!(
        seen.len() > sni_before_resync && seen.iter().all(|name| name == ROUTE),
        "relay-observed consumer SNI before {sni_before_resync}, after {seen:?}"
    );
    phone.stop_connection();
}
