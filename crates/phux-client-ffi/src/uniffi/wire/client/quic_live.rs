//! `RemoteClient` against a real server's loopback QUIC listener: the mobile
//! constructor's `quic://` string reaches ATTACHED through the shared runtime,
//! and an unpinned routable target fails closed before any socket opens.

#![allow(clippy::expect_used, reason = "test assertions")]
#![allow(clippy::panic, reason = "test assertions")]

use std::net::{SocketAddr, UdpSocket};
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
    _dir: tempfile::TempDir,
}

impl QuicServer {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let (cert, key) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
        phux_server::transport::tls::ensure_self_signed(&cert, &key).expect("cert");
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
            _dir: dir,
        }
    }
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
