//! The relay end to end with this crate's own client legs and a protocol
//! that names no application: a service's tunnel, consumers through it, and
//! each refusal a client can meet.

#![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use quictun::connect::{ConnectError, RelayAddr, Tunnel, dial_route};
use quictun::forward::{ServiceTls, serve_tunnel, service_tls_config};
use quictun::relay::{RelayConfig, RelayRuntime, TcpIngress, cert_fingerprint, mint_route_token};
use quictun::tunnel::{OVER_CAP_CODE, RECLAIMED_CODE, ROUTE_OFFLINE_CODE};
use quictun::{CidTag, HalfClose, Protocol};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
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
    /// The TCP ingress, when the test configured one.
    tcp: Option<SocketAddr>,
    _stop: oneshot::Sender<()>,
}

impl Relay {
    fn start() -> Self {
        Self::start_with(protocol(), |_| {})
    }

    fn start_with(protocol: Protocol, tune: impl FnOnce(&mut RelayConfig)) -> Self {
        Self::start_tuned(protocol, tune, |runtime| runtime)
    }

    fn start_tuned(
        protocol: Protocol,
        tune: impl FnOnce(&mut RelayConfig),
        tune_runtime: impl FnOnce(RelayRuntime) -> RelayRuntime,
    ) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let listen: SocketAddr = "127.0.0.1:0".parse().expect("addr");
        let mut config = RelayConfig::in_dir(listen, dir.path(), protocol);
        tune(&mut config);
        let tokens = config.tokens_path.clone();
        let cert = config.cert_path.clone();
        let bound = tune_runtime(RelayRuntime::new(config))
            .bind()
            .expect("bind");
        let addr = bound.local_addr();
        let tcp = bound.tcp_addr();
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
            tcp,
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

#[tokio::test]
async fn a_route_at_its_consumer_cap_refuses_the_next_consumer_until_one_leaves() {
    timeout(DEADLINE, async {
        let relay = Relay::start_with(protocol(), |config| config.max_conns_per_route = Some(1));
        let token = relay.enroll("echo");
        let tunnel = Tunnel::open(&relay.addr, &protocol(), &token)
            .await
            .expect("tunnel");
        let _service = serve_echo(tunnel);

        let (first_endpoint, first_conn, mut first_send, mut first_recv) =
            dial_route(&relay.addr, "echo", b"echo/1")
                .await
                .expect("first");
        first_send.write_all(b"one").await.expect("write");
        let mut echoed = [0u8; 3];
        first_recv.read_exact(&mut echoed).await.expect("read");
        assert_eq!(
            &echoed, b"ONE",
            "the first consumer holds the route's one unit"
        );

        let (_endpoint, conn, mut send, _recv) = dial_route(&relay.addr, "echo", b"echo/1")
            .await
            .expect("second");
        let _ = send.write_all(b"two").await;
        let refused = ConnectError::from(conn.closed().await);
        assert_eq!(refused.close_code(), Some(OVER_CAP_CODE), "{refused}");

        first_conn.close(0u32.into(), b"done");
        drop((first_send, first_recv));
        first_endpoint.wait_idle().await;
        // The relay returns the unit once it sees the close.
        while try_round_trip(&relay.addr, b"three").await.as_deref() != Some(&b"THREE"[..]) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("deadline");
}

/// One consumer round trip, or `None` when the relay refused it.
async fn try_round_trip(relay: &RelayAddr, message: &[u8]) -> Option<Vec<u8>> {
    let (_endpoint, _conn, mut send, mut recv) = dial_route(relay, "echo", b"echo/1").await.ok()?;
    send.write_all(message).await.ok()?;
    let mut echoed = vec![0u8; message.len()];
    recv.read_exact(&mut echoed).await.ok()?;
    Some(echoed)
}

#[tokio::test]
async fn with_propagated_half_close_a_consumer_reads_the_reply_after_its_fin() {
    timeout(DEADLINE, async {
        let mut tcp_like = protocol();
        tcp_like.half_close = HalfClose::Propagate;
        let relay = Relay::start_with(tcp_like.clone(), |_| {});
        let token = relay.enroll("echo");
        let tunnel = Tunnel::open(&relay.addr, &tcp_like, &token)
            .await
            .expect("tunnel");
        // A request/response service: read to the FIN, then answer.
        tokio::spawn(async move {
            let (mut send, mut recv) = tunnel.accept().await.expect("consumer");
            let request = recv.read_to_end(1024).await.expect("request");
            let reply = format!("got {} bytes", request.len());
            send.write_all(reply.as_bytes()).await.expect("reply");
            send.finish().expect("finish");
            let _ = send.stopped().await;
        });

        let (_endpoint, _conn, mut send, mut recv) = dial_route(&relay.addr, "echo", b"echo/1")
            .await
            .expect("consumer");
        send.write_all(b"hello").await.expect("write");
        send.finish().expect("half-close");
        let reply = recv.read_to_end(1024).await.expect("reply after the FIN");
        assert_eq!(reply, b"got 5 bytes");
    })
    .await
    .expect("deadline");
}

/// A multiplexed consumer carries many streams on one connection, each
/// bridged on its own; once it has none for the idle period the relay closes
/// it, so an idle client gives its connection slot back.
#[tokio::test]
async fn a_multiplexed_consumer_is_bridged_per_stream_and_reaped_when_idle() {
    timeout(DEADLINE, async {
        let mut multiplexed = protocol();
        multiplexed.consumer_streams = 8;
        let relay = Relay::start_tuned(
            multiplexed.clone(),
            |_| {},
            |runtime| runtime.with_multiplexed_idle(Duration::from_millis(300)),
        );
        let token = relay.enroll("echo");
        let tunnel = Tunnel::open(&relay.addr, &multiplexed, &token)
            .await
            .expect("tunnel");
        let _service = serve_echo(tunnel);

        let (_endpoint, conn) =
            quictun::connect::dial_route_connection(&relay.addr, "echo", b"echo/1")
                .await
                .expect("consumer");
        for message in [&b"one"[..], b"two", b"three"] {
            let (mut send, mut recv) = conn.open_bi().await.expect("stream");
            send.write_all(message).await.expect("write");
            let mut echoed = vec![0u8; message.len()];
            recv.read_exact(&mut echoed).await.expect("read");
            assert_eq!(echoed, message.to_ascii_uppercase(), "each stream bridged");
            send.finish().expect("finish");
        }
        let closed = ConnectError::from(conn.closed().await);
        assert_eq!(closed.close_code(), Some(0), "closed as idle: {closed}");
    })
    .await
    .expect("deadline");
}

/// TCP clients that connect and send nothing hold only handshake permits,
/// never connection slots, so they cannot lock out tunnels and consumers.
#[tokio::test]
async fn stalled_tcp_clients_take_no_connection_slot() {
    timeout(DEADLINE, async {
        let relay = Relay::start_with(Protocol::quictun(), |config| {
            // One slot for the tunnel, one for the consumer.
            config.max_conns = 2;
            config.tcp_ingress = Some(TcpIngress {
                listen: "127.0.0.1:0".parse().expect("addr"),
                domain: None,
            });
        });
        let ingress = relay.tcp.expect("tcp ingress");
        let mut stalled = Vec::new();
        for _ in 0..3 {
            stalled.push(tokio::net::TcpStream::connect(ingress).await.expect("tcp"));
        }
        let token = relay.enroll("echo");
        let tunnel = Tunnel::open(&relay.addr, &Protocol::quictun(), &token)
            .await
            .expect("tunnel");
        let _service = serve_echo(tunnel);
        assert_eq!(
            round_trip(&relay.addr, "echo", b"quictun/1", b"still served").await,
            b"STILL SERVED"
        );
        drop(stalled);
    })
    .await
    .expect("deadline");
}

/// A local TCP service that reads each connection to its end, then answers
/// with how much it got.
async fn counting_target() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = Vec::new();
                let _ = stream.read_to_end(&mut request).await;
                let reply = format!("got {} bytes", request.len());
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    addr
}

#[tokio::test]
async fn tls_clients_without_quic_reach_a_route_through_the_tcp_ingress() {
    timeout(DEADLINE, async {
        let relay = Relay::start_with(Protocol::quictun(), |config| {
            config.tcp_ingress = Some(TcpIngress {
                listen: "127.0.0.1:0".parse().expect("addr"),
                domain: Some("relay.test".to_owned()),
            });
        });
        let token = relay.enroll("echo");
        let tunnel = Tunnel::open(&relay.addr, &Protocol::quictun(), &token)
            .await
            .expect("tunnel");
        let dir = tempfile::tempdir().expect("tempdir");
        let (cert, key) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
        quictun::cert::ensure_self_signed(&cert, &key).expect("service cert");
        let service_pin = cert_fingerprint(&cert).expect("service pin");
        let tls = ServiceTls::Terminate(Arc::new(
            service_tls_config(&cert, &key, Vec::new()).expect("service tls"),
        ));
        let target = counting_target().await;
        tokio::spawn(async move { serve_tunnel(&tunnel, target, &tls).await });

        let ingress = relay.tcp.expect("tcp ingress");
        let connector = tokio_rustls::TlsConnector::from(Arc::new(
            quictun::pin::stream_client_config(&service_pin).expect("client tls"),
        ));
        let tcp = tokio::net::TcpStream::connect(ingress).await.expect("tcp");
        let name = rustls::pki_types::ServerName::try_from("echo.relay.test").expect("name");
        let mut session = connector
            .connect(name, tcp)
            .await
            .expect("TLS with the service");
        session.write_all(b"hello over tcp").await.expect("write");
        session.shutdown().await.expect("half-close");
        let mut reply = Vec::new();
        session.read_to_end(&mut reply).await.expect("reply");
        assert_eq!(reply, b"got 14 bytes");

        // A name outside the domain, or an unenrolled route, gets nothing.
        for name in ["nobody.relay.test", "echo.elsewhere.test"] {
            let tcp = tokio::net::TcpStream::connect(ingress).await.expect("tcp");
            let name = rustls::pki_types::ServerName::try_from(name).expect("name");
            assert!(connector.connect(name, tcp).await.is_err(), "refused");
        }
    })
    .await
    .expect("deadline");
}
