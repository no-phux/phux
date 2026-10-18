//! The relay end to end with this crate's own client legs and a protocol
//! that names no application: a service's tunnel, consumers through it, and
//! each refusal a client can meet.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

use std::net::SocketAddr;
use std::time::Duration;

use quictun::connect::{ConnectError, RelayAddr, Tunnel, dial_route};
use quictun::relay::{RelayConfig, RelayRuntime, cert_fingerprint, mint_route_token};
use quictun::tunnel::{RECLAIMED_CODE, ROUTE_OFFLINE_CODE};
use quictun::{CidTag, Protocol};
use tokio::sync::oneshot;
use tokio::time::timeout;

const DEADLINE: Duration = Duration::from_secs(10);

fn protocol() -> Protocol {
    let mut protocol = Protocol::new(b"echo-tunnel/1", b"echo/1", CidTag(*b"ecoT"));
    protocol.consumer_alpns.push(b"echo/2".to_vec());
    protocol
}

/// A serving relay, its state directory, and how clients reach it.
struct Relay {
    _dir: tempfile::TempDir,
    tokens: std::path::PathBuf,
    addr: RelayAddr,
    _stop: oneshot::Sender<()>,
}

impl Relay {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let listen: SocketAddr = "127.0.0.1:0".parse().expect("addr");
        let config = RelayConfig::in_dir(listen, dir.path(), protocol());
        let tokens = config.tokens_path.clone();
        let cert = config.cert_path.clone();
        let bound = RelayRuntime::new(config).bind().expect("bind");
        let addr = bound.local_addr();
        let (stop, stopped) = oneshot::channel::<()>();
        tokio::spawn(bound.serve(async move {
            let _ = stopped.await;
        }));
        Self {
            addr: RelayAddr {
                addr,
                pin: cert_fingerprint(&cert).expect("pin"),
            },
            tokens,
            _dir: dir,
            _stop: stop,
        }
    }

    fn enroll(&self, route: &str) -> Vec<u8> {
        hex::decode(mint_route_token(&self.tokens, route).expect("mint")).expect("hex")
    }
}

/// Echo every bridged consumer's bytes back, upper-cased, until it closes.
fn serve_echo(tunnel: Tunnel) -> tokio::task::JoinHandle<ConnectError> {
    tokio::spawn(async move {
        loop {
            let (mut send, mut recv) = match tunnel.accept().await {
                Ok(streams) => streams,
                Err(err) => return err,
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                while let Ok(Some(n)) = recv.read(&mut buf).await {
                    if send
                        .write_all(&buf[..n].to_ascii_uppercase())
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                let _ = send.finish();
            });
        }
    })
}

async fn round_trip(relay: &RelayAddr, route: &str, alpn: &[u8], message: &[u8]) -> Vec<u8> {
    let (_endpoint, _conn, mut send, mut recv) =
        dial_route(relay, route, alpn).await.expect("consumer dial");
    send.write_all(message).await.expect("write");
    // No half-close first: the relay ends a bridge when either direction
    // finishes, so the reply is read before anything is closed.
    let mut echoed = vec![0u8; message.len()];
    recv.read_exact(&mut echoed).await.expect("read");
    echoed
}

#[tokio::test]
async fn consumers_reach_a_service_through_its_tunnel_by_route() {
    timeout(DEADLINE, async {
        let relay = Relay::start();
        let token = relay.enroll("echo");
        let tunnel = Tunnel::open(&relay.addr, &protocol(), &token)
            .await
            .expect("tunnel");
        let _service = serve_echo(tunnel);

        // Every consumer ALPN is admitted, and concurrent consumers each get
        // their own stream over the one tunnel.
        let (first, second) = tokio::join!(
            round_trip(&relay.addr, "echo", b"echo/1", b"first"),
            round_trip(&relay.addr, "echo", b"echo/2", b"second"),
        );
        assert_eq!(first, b"FIRST");
        assert_eq!(second, b"SECOND");
    })
    .await
    .expect("deadline");
}

#[tokio::test]
async fn a_tunnel_with_an_unknown_token_is_refused() {
    timeout(DEADLINE, async {
        let relay = Relay::start();
        relay.enroll("echo");
        let refused = match Tunnel::open(&relay.addr, &protocol(), &[7u8; 32]).await {
            Ok(tunnel) => tunnel
                .accept()
                .await
                .expect_err("refused after the preamble"),
            Err(err) => err,
        };
        assert!(refused.is_auth_refused(), "{refused}");
    })
    .await
    .expect("deadline");
}

#[tokio::test]
async fn consumers_of_unknown_or_offline_routes_are_refused() {
    timeout(DEADLINE, async {
        let relay = Relay::start();
        relay.enroll("echo");

        // Not enrolled: the relay declines at TLS, so the handshake fails.
        let unknown = dial_route(&relay.addr, "nobody", b"echo/1").await;
        assert!(unknown.is_err(), "an unknown route fails the handshake");

        // Enrolled, no tunnel: closed with ROUTE_OFFLINE once the stream opens.
        let (_endpoint, conn, mut send, _recv) = dial_route(&relay.addr, "echo", b"echo/1")
            .await
            .expect("handshake for an enrolled route");
        let _ = send.write_all(b"hello").await;
        let closed = ConnectError::from(conn.closed().await);
        assert_eq!(closed.close_code(), Some(ROUTE_OFFLINE_CODE), "{closed}");

        // An ALPN outside the protocol never completes a handshake.
        assert!(dial_route(&relay.addr, "echo", b"other/1").await.is_err());
    })
    .await
    .expect("deadline");
}

#[tokio::test]
async fn a_newer_tunnel_for_the_route_reclaims_it() {
    timeout(DEADLINE, async {
        let relay = Relay::start();
        let token = relay.enroll("echo");
        let old = Tunnel::open(&relay.addr, &protocol(), &token)
            .await
            .expect("first tunnel");
        let old_service = serve_echo(old);
        // A bridged consumer proves the relay claimed the route for it.
        assert_eq!(
            round_trip(&relay.addr, "echo", b"echo/1", b"old").await,
            b"OLD"
        );

        let new = Tunnel::open(&relay.addr, &protocol(), &token)
            .await
            .expect("second tunnel");
        let reclaimed = old_service.await.expect("service task");
        assert_eq!(reclaimed.close_code(), Some(RECLAIMED_CODE), "{reclaimed}");

        let _service = serve_echo(new);
        assert_eq!(
            round_trip(&relay.addr, "echo", b"echo/1", b"new").await,
            b"NEW"
        );
    })
    .await
    .expect("deadline");
}
