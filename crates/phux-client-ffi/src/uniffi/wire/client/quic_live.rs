//! `RemoteClient` against a real server's loopback QUIC listener: the mobile
//! constructor's `quic://` string reaches ATTACHED through the shared runtime,
//! and an unpinned routable target fails closed before any socket opens.

#![allow(clippy::expect_used, reason = "test assertions")]
#![allow(clippy::panic, reason = "test assertions")]

use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use phux_server::{ServerConfig, ServerEnv, ServerRuntime};
use tokio::sync::oneshot;

use super::*;

/// Generous, like the testkit's deadlines: a real server under a parallel run.
const DEADLINE: Duration = Duration::from_secs(20);
const SESSION: &str = "quic-live";

/// A real server on its own thread, listening on loopback QUIC at `addr`.
struct QuicServer {
    addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
    dir: tempfile::TempDir,
}

impl QuicServer {
    fn start() -> Self {
        Self::start_with(false)
    }

    /// With `authority`, the certificate is the one a server provisions
    /// itself: issued by its workload CA, presented after the leaf
    /// (ADR-0153).
    fn start_with(authority: bool) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let (cert, key) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
        if authority {
            std::fs::set_permissions(
                dir.path(),
                <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
            )
            .expect("owner-only state directory");
            let paths = phux_server::workload::WorkloadPaths::with_overrides(
                Some(dir.path().join("workload-ca.pem")),
                Some(dir.path().join("workload-ca.key")),
                Some(dir.path().join("workload-keys")),
            );
            phux_server::transport::tls::ensure_server_identity(&cert, &key, &[], &paths)
                .expect("cert");
        } else {
            phux_server::transport::tls::ensure_self_signed(&cert, &key).expect("cert");
        }
        // The runtime's reconnect ladder covers the listener's startup, so a
        // reserved-then-released port is enough; no readiness probe needed.
        let addr = UdpSocket::bind("127.0.0.1:0")
            .and_then(|socket| socket.local_addr())
            .expect("reserve a loopback UDP port");
        let config = ServerConfig {
            socket_path: dir.path().join("phux.sock"),
            pre_seeded_session: Some(SESSION.to_owned()),
            seed_with_pty: false,
            seed_command: None,
            env: ServerEnv {
                tls_cert: Some(cert),
                tls_key: Some(key),
                ..ServerEnv::default()
            },
            ..ServerConfig::with_default_socket()
        };
        let (shutdown, stopped) = oneshot::channel::<()>();
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            tokio::task::LocalSet::new().block_on(&runtime, async move {
                ServerRuntime::new(config)
                    .listen_quic(addr)
                    .run_async(async move {
                        let _ = stopped.await;
                    })
                    .await
                    .expect("server run");
            });
        });
        Self {
            addr,
            shutdown: Some(shutdown),
            thread: Some(thread),
            dir,
        }
    }

    fn cert(&self) -> std::path::PathBuf {
        self.dir.path().join("cert.pem")
    }

    /// A server in `paired` mode: every QUIC connection must present an
    /// enrolled workload certificate (ADR-0116), and the listener offers the
    /// enrollment ALPN (ADR-0154). Its authority lives in its own directory.
    fn start_paired() -> (Self, phux_server::workload::WorkloadPaths) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::set_permissions(
            dir.path(),
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .expect("owner-only state directory");
        let paths = phux_server::workload::WorkloadPaths::with_overrides(
            Some(dir.path().join("workload-ca.pem")),
            Some(dir.path().join("workload-ca.key")),
            Some(dir.path().join("workload-keys")),
        );
        let (cert, key) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
        phux_server::transport::tls::ensure_server_identity(&cert, &key, &[], &paths)
            .expect("cert");
        let addr = UdpSocket::bind("127.0.0.1:0")
            .and_then(|socket| socket.local_addr())
            .expect("reserve a loopback UDP port");
        let config = ServerConfig {
            socket_path: dir.path().join("phux.sock"),
            pre_seeded_session: Some(SESSION.to_owned()),
            seed_with_pty: false,
            seed_command: None,
            env: ServerEnv {
                tls_cert: Some(cert),
                tls_key: Some(key),
                workload_mtls: true,
                workload_ca: Some(paths.ca_cert.clone()),
                workload_ca_key: Some(paths.ca_key.clone()),
                workload_keys: Some(paths.registry.clone()),
                ..ServerEnv::default()
            },
            ..ServerConfig::with_default_socket()
        };
        let (shutdown, stopped) = oneshot::channel::<()>();
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            tokio::task::LocalSet::new().block_on(&runtime, async move {
                ServerRuntime::new(config)
                    .listen_quic(addr)
                    .run_async(async move {
                        let _ = stopped.await;
                    })
                    .await
                    .expect("server run");
            });
        });
        (
            Self {
                addr,
                shutdown: Some(shutdown),
                thread: Some(thread),
                dir,
            },
            paths,
        )
    }
}

/// A relay, and behind it a `paired` server that reaches it only through its
/// `[[connector]]`: the phone dials the relay and meets the server end to
/// end inside the relayed stream (ADR-0154 item 5).
struct RelayedServer {
    relay: SocketAddr,
    relay_pin: String,
    authority: String,
    paths: phux_server::workload::WorkloadPaths,
    token: String,
    observed_sni: Arc<Mutex<Vec<String>>>,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
    _dir: tempfile::TempDir,
}

const ROUTE: &str = "phone-route";

impl RelayedServer {
    fn start() -> Self {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("owner-only");
        let d = dir.path().to_path_buf();
        let write_secret = |name: &str, body: &str| {
            std::fs::write(d.join(name), body).expect("secret");
            std::fs::set_permissions(d.join(name), std::fs::Permissions::from_mode(0o600))
                .expect("chmod");
        };
        let tunnel = "31".repeat(32);
        let token = "42".repeat(32);
        std::fs::write(d.join("relay-tokens"), format!("{tunnel} {ROUTE}\n")).expect("route");
        write_secret("connector-token", &format!("{tunnel}\n"));
        write_secret("consumer-tokens", &format!("{token}\n"));
        phux_server::auth::migrate_legacy_store(&d.join("consumer-tokens")).expect("migrate");
        let paths = phux_server::workload::WorkloadPaths::with_overrides(
            Some(d.join("workload-ca.pem")),
            Some(d.join("workload-ca.key")),
            Some(d.join("workload-keys")),
        );
        let (cert, key) = (d.join("cert.pem"), d.join("key.pem"));
        phux_server::transport::tls::ensure_server_identity(&cert, &key, &[], &paths)
            .expect("server identity");
        let authority = phux_server::transport::tls::presented_authority(&cert)
            .expect("chain")
            .expect("issued by the CA");
        let (shutdown, stopped) = oneshot::channel::<()>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let env = ServerEnv {
            ws_tokens: Some(d.join("consumer-tokens")),
            tls_cert: Some(cert),
            tls_key: Some(key),
            workload_mtls: true,
            workload_ca: Some(paths.ca_cert.clone()),
            workload_ca_key: Some(paths.ca_key.clone()),
            workload_keys: Some(paths.registry.clone()),
            ..ServerEnv::default()
        };
        let observed_sni = Arc::new(Mutex::new(Vec::new()));
        let state = d.clone();
        let relay_sni = Arc::clone(&observed_sni);
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            tokio::task::LocalSet::new().block_on(&runtime, async move {
                let relay = phux_server_testkit::relay::RelayHarness::start_observing(
                    phux_relay::RelayConfig {
                        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
                        cert_path: state.join("relay-cert.pem"),
                        key_path: state.join("relay-key.pem"),
                        tokens_path: state.join("relay-tokens"),
                        max_conns: 16,
                    },
                    relay_sni,
                );
                let pin =
                    phux_relay::cert_fingerprint(&state.join("relay-cert.pem")).expect("relay pin");
                let connector = phux_config::ConnectorConfigEntry {
                    relay: relay.addr.to_string(),
                    token_file: Some(state.join("connector-token")),
                    cert_fingerprint: Some(pin.clone()),
                };
                ready_tx.send((relay.addr, pin)).expect("ready");
                let config = ServerConfig {
                    socket_path: state.join("phux.sock"),
                    pre_seeded_session: Some(SESSION.to_owned()),
                    seed_with_pty: false,
                    seed_command: None,
                    env,
                    ..ServerConfig::with_default_socket()
                };
                ServerRuntime::new(config)
                    .connectors(vec![connector], None)
                    .run_async(async move {
                        let _ = stopped.await;
                    })
                    .await
                    .expect("server run");
                relay.stop().await;
            });
        });
        let (relay, relay_pin) = ready_rx.recv().expect("relay up");
        Self {
            relay,
            relay_pin,
            authority,
            paths,
            token,
            observed_sni,
            shutdown: Some(shutdown),
            thread: Some(thread),
            _dir: dir,
        }
    }

    fn client(&self) -> Arc<RemoteClient> {
        let client = RemoteClient::new_routed(
            format!("quic://{}", self.relay),
            80,
            24,
            Some(self.relay_pin.clone()),
            Some(self.token.clone()),
            ROUTE.to_owned(),
        )
        .expect("valid relay route");
        client.set_authority_pin(Some(self.authority.clone()));
        client
    }
}

impl Drop for RelayedServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A device key for the binding, in software (the phone's is in its
/// keystore; the binding cannot tell).
struct TestKey(phux_client_runtime::enroll::SoftwareSigner);

impl DeviceKey for TestKey {
    fn public_point(&self) -> Vec<u8> {
        phux_client_runtime::enroll::DeviceSigner::public_point(&self.0)
    }

    fn sign(&self, message: Vec<u8>) -> Result<Vec<u8>, DeviceKeyError> {
        phux_client_runtime::enroll::DeviceSigner::sign(&self.0, &message)
            .map_err(|reason| DeviceKeyError::Failed { reason })
    }
}

fn test_key() -> Arc<dyn DeviceKey> {
    Arc::new(TestKey(
        phux_client_runtime::enroll::SoftwareSigner::generate(),
    ))
}

fn mint(paths: &phux_server::workload::WorkloadPaths) -> String {
    phux_server::workload::tickets::mint_ticket(
        &phux_server::workload::tickets::tickets_path(&paths.registry),
        vec!["inventory,observe,create,bind,input,signal@global".to_owned()],
        3600,
        600,
    )
    .expect("ticket")
    .secret_hex
}

impl Drop for QuicServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn wait_for(remote: &RemoteClient, done: impl Fn(&RemoteClient) -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !done(remote) {
        assert!(
            Instant::now() < deadline,
            "timed out: status {:?}, last error {:?}",
            remote.status(),
            remote.last_error()
        );
        let _ = remote.take_events();
        thread::sleep(Duration::from_millis(20));
    }
}

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

/// ADR-0153 for the phone: a leaf-pinned client learns the CA on its first
/// connection; pinned, a different CA is refused by name and never attaches,
/// and the right one attaches.
#[test]
fn a_leaf_pinned_phone_learns_the_authority_and_a_swapped_one_is_refused() {
    let server = QuicServer::start_with(true);
    let leaf = phux_server::transport::tls::cert_fingerprint(&server.cert()).expect("leaf");
    let authority = phux_server::transport::tls::presented_authority(&server.cert())
        .expect("chain")
        .expect("issued by the workload CA");
    let endpoint = format!("quic://{}", server.addr);

    let learning = RemoteClient::new(endpoint.clone(), 80, 24, Some(leaf.clone()), None);
    learning.connect().expect("connect");
    learning.attach_session(SESSION.to_owned());
    wait_for(&learning, |remote| remote.status() == WireStatus::Attached);
    assert_eq!(learning.learned_authority(), Some(authority.clone()));
    learning.stop_connection();

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
    assert_eq!(
        swapped.learned_authority(),
        None,
        "a pinned client learns nothing"
    );
    swapped.stop_connection();

    let pinned = RemoteClient::new(endpoint, 80, 24, Some(leaf), None);
    pinned.set_authority_pin(Some(authority));
    pinned.connect().expect("connect");
    pinned.attach_session(SESSION.to_owned());
    wait_for(&pinned, |remote| remote.status() == WireStatus::Attached);
    pinned.stop_connection();
}

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
    let phone = RemoteClient::new(endpoint.clone(), 80, 24, Some(leaf.clone()), None);
    let chain = phone
        .enroll_device(ticket.clone(), Arc::clone(&key))
        .expect("the ticket enrolls the device key");
    assert!(!chain.contains("PRIVATE KEY"));
    assert_eq!(
        phone.learned_authority(),
        Some(phux_server::workload::ca_fingerprint(&paths.ca_cert).expect("ca")),
        "enrolling pins the authority that issued the device's certificate"
    );
    phone.connect().expect("connect");
    phone.attach_session(SESSION.to_owned());
    wait_for(&phone, |remote| remote.status() == WireStatus::Attached);
    phone.stop_connection();

    // A later launch: the stored chain and the keystore key.
    let relaunched = RemoteClient::new(endpoint.clone(), 80, 24, Some(leaf.clone()), None);
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
    let replay = thief
        .enroll_device(ticket, test_key())
        .expect_err("a consumed ticket enrolls nothing");
    assert!(
        replay.to_string().contains("enrollment refused"),
        "{replay}"
    );

    // Adversarial: the phone's (public) chain with another device's key.
    let impostor = RemoteClient::new(endpoint, 80, 24, Some(leaf), None);
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

/// Poll `take_publication` until `done` holds, keeping every event seen.
fn publish_until(
    remote: &RemoteClient,
    seen: &mut Vec<WireEvent>,
    done: impl Fn(&WirePublication) -> bool,
) -> WirePublication {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let publication = remote.take_publication();
        seen.extend(publication.events.iter().cloned());
        if done(&publication) {
            return publication;
        }
        assert!(
            Instant::now() < deadline,
            "timed out: {publication:?}, last error {:?}",
            remote.last_error()
        );
        thread::sleep(Duration::from_millis(20));
    }
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
