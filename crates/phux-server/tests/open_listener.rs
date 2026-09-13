//! `OPEN_LISTENER` over the wire (ADR-0120, `docs/spec/L1.md` §5.6).
//!
//! A Unix-socket client opens a QUIC listener for one attach. The listener
//! admits the token it was opened with and nothing else, the command itself is
//! refused when it arrives over QUIC, and the listener closes once nobody has
//! used it for its linger. Every case drives a real in-process server and real
//! QUIC dials on loopback.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(unused_unsafe, reason = "env::set_var is unsafe only on edition 2024")]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use phux_dial::{CertTrust, QuicDial};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet, ServerFeature};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, ErrorCode, FrameKind, ListenerTransport,
};
use phux_server_testkit::{
    SERVER_JOIN_DEADLINE, SOCKET_CONNECT_DEADLINE, await_command_result, encode_frame_vec,
    run_local, send_frame, spawn_server, wait_for_socket,
};
use tempfile::TempDir;
use tokio::net::UnixStream;

/// How long a dial plus HELLO may take before the listener is judged closed
/// or refusing. A loopback handshake takes milliseconds.
const DIAL_DEADLINE: Duration = Duration::from_secs(4);

/// The certificate every server in this binary presents.
struct Tls {
    _dir: TempDir,
    cert: PathBuf,
}

/// Point the server at one certificate for the whole binary. The variables
/// are process-global, so they are set once, before any server reads them.
fn tls() -> &'static Tls {
    static TLS: OnceLock<Tls> = OnceLock::new();
    TLS.get_or_init(|| {
        let dir = TempDir::new().expect("tempdir");
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        phux_server::transport::tls::ensure_self_signed(&cert, &key).expect("certificate");
        // SAFETY: runs once, inside `get_or_init`, before any server in this
        // process starts; every test reaches a server only through this call,
        // so nothing reads the environment while it is written.
        unsafe {
            std::env::set_var("PHUX_WS_TLS_CERT", &cert);
            std::env::set_var("PHUX_WS_TLS_KEY", &key);
        }
        Tls { _dir: dir, cert }
    })
}

fn hello() -> FrameKind {
    FrameKind::Hello {
        client_name: "open-listener-test".to_owned(),
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        client_caps: ClientCapabilities::new()
            .with_color_support(ColorSupport::TrueColor)
            .with_layers(LayerSet::all()),
    }
}

/// Send `OPEN_LISTENER` over `stream` and return the server's answer.
async fn open(
    stream: &mut UnixStream,
    request_id: u32,
    transport: ListenerTransport,
    port_range: Option<(u16, u16)>,
    linger_secs: u32,
) -> CommandResult {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command: Command::OpenListener {
                transport,
                port_range,
                linger_secs,
            },
        },
    )
    .await;
    await_command_result(stream, request_id).await
}

/// The fields of a successful `OPEN_LISTENER` reply.
struct Opened {
    port: u16,
    token: Vec<u8>,
    fingerprint: String,
    linger_secs: u64,
}

fn opened(result: CommandResult) -> Opened {
    let CommandResult::OkWith(CommandValue::Json(json)) = result else {
        panic!("OPEN_LISTENER failed: {result:?}");
    };
    let doc: serde_json::Value = serde_json::from_str(&json).expect("reply is JSON");
    assert_eq!(doc["schema_version"], 1);
    assert_eq!(doc["transport"], "quic");
    let token = doc["token"].as_str().expect("token");
    assert_eq!(token.len(), 64, "a 256-bit token, hex");
    Opened {
        port: u16::try_from(doc["port"].as_u64().expect("port")).expect("port fits u16"),
        token: hex::decode(token).expect("token is hex"),
        fingerprint: doc["cert_fingerprint"]
            .as_str()
            .expect("fingerprint")
            .to_owned(),
        linger_secs: doc["linger_secs"].as_u64().expect("linger"),
    }
}

/// A QUIC connection that completed HELLO through a listener.
struct Session {
    endpoint: quinn::Endpoint,
    conn: quinn::Connection,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
}

impl Session {
    /// Close at once, so the server sees the connection end now rather than
    /// at its idle timeout.
    async fn close(self) {
        self.conn.close(0u32.into(), b"done");
        let _ = tokio::time::timeout(Duration::from_secs(1), self.endpoint.wait_idle()).await;
    }
}

async fn read_frame(recv: &mut quinn::RecvStream) -> Result<FrameKind, String> {
    let mut header = [0u8; 4];
    recv.read_exact(&mut header)
        .await
        .map_err(|err| err.to_string())?;
    let len = u32::from_be_bytes(header) as usize;
    let mut framed = header.to_vec();
    framed.resize(4 + len, 0);
    recv.read_exact(&mut framed[4..])
        .await
        .map_err(|err| err.to_string())?;
    FrameKind::decode(&framed)
        .map(|(frame, _)| frame)
        .map_err(|err| format!("{err:?}"))
}

/// Dial the listener on loopback with `token`, pinned to `fingerprint`, and
/// complete HELLO. `Err` when the listener refused the token or is gone.
async fn dial(opened: &Opened, token: &[u8]) -> Result<Session, String> {
    let dial = QuicDial {
        addr: SocketAddr::from(([127, 0, 0, 1], opened.port)),
        server_name: "localhost".to_owned(),
        token: Some(token.to_vec()),
        trust: CertTrust::Pinned(opened.fingerprint.clone()),
    };
    tokio::time::timeout(DIAL_DEADLINE, async {
        let (endpoint, conn, mut send, mut recv) = phux_dial::quic::dial(&dial)
            .await
            .map_err(|err| err.to_string())?;
        send.write_all(&encode_frame_vec(&hello()))
            .await
            .map_err(|err| err.to_string())?;
        match read_frame(&mut recv).await? {
            FrameKind::HelloOk { server_caps, .. } => {
                assert!(
                    server_caps.features.contains(ServerFeature::OpenListener),
                    "the server advertises OPEN_LISTENER on every transport"
                );
                Ok(Session {
                    endpoint,
                    conn,
                    send,
                    recv,
                })
            }
            other => Err(format!("expected HELLO_OK, got {other:?}")),
        }
    })
    .await
    .map_err(|_| "no HELLO_OK before the deadline".to_owned())?
}

/// Run `body` against a fresh server, then stop it.
fn with_server<F, Fut>(body: F)
where
    F: FnOnce(UnixStream) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    tls();
    let dir = TempDir::new().expect("tempdir");
    let socket = dir.path().join("s.sock");
    run_local(async move {
        let (shutdown, handle) = spawn_server(socket.clone(), Some("s"));
        let uds = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        body(uds).await;
        let _ = shutdown.send(());
        let _ = tokio::time::timeout(SERVER_JOIN_DEADLINE, handle).await;
    });
}

#[test]
fn a_listener_admits_its_own_token_and_nothing_else() {
    with_server(|mut uds| async move {
        let a = opened(open(&mut uds, 1, ListenerTransport::Quic, None, 0).await);
        let b = opened(open(&mut uds, 2, ListenerTransport::Quic, None, 0).await);
        assert_ne!(a.port, b.port, "each attach gets its own listener");
        assert_ne!(a.token, b.token, "and its own token");
        assert_eq!(
            a.fingerprint,
            phux_server::transport::tls::cert_fingerprint(&tls().cert).expect("fingerprint"),
            "the listener presents the server's persistent certificate"
        );
        assert_eq!(a.linger_secs, 120, "0 asks for the server default");

        dial(&a, &a.token)
            .await
            .expect("its own token is admitted")
            .close()
            .await;
        assert!(
            dial(&b, &a.token).await.is_err(),
            "another listener's token is refused"
        );
        assert!(
            dial(&a, &[0x5a; 32]).await.is_err(),
            "an unknown token is refused"
        );
    });
}

#[test]
fn open_listener_is_refused_over_quic() {
    with_server(|mut uds| async move {
        let a = opened(open(&mut uds, 1, ListenerTransport::Quic, None, 0).await);
        let mut session = dial(&a, &a.token).await.expect("admitted");
        let command = FrameKind::Command {
            request_id: 7,
            command: Command::OpenListener {
                transport: ListenerTransport::Quic,
                port_range: None,
                linger_secs: 0,
            },
        };
        session
            .send
            .write_all(&encode_frame_vec(&command))
            .await
            .expect("send");
        let result = loop {
            if let FrameKind::CommandResult {
                request_id: 7,
                result,
            } = read_frame(&mut session.recv).await.expect("reply")
            {
                break result;
            }
        };
        let CommandResult::Error { code, message } = result else {
            panic!("a remote peer opened a listener: {result:?}");
        };
        assert_eq!(code, ErrorCode::PermissionDenied);
        assert!(message.contains("local socket"), "{message}");
        session.close().await;
    });
}

#[test]
fn malformed_requests_are_refused_with_invalid_command() {
    with_server(|mut uds| async move {
        for (request_id, transport, range) in [
            (1, ListenerTransport::Unknown(7), None),
            (2, ListenerTransport::Quic, Some((9, 3))),
            (3, ListenerTransport::Quic, Some((0, 5))),
        ] {
            let result = open(&mut uds, request_id, transport, range, 0).await;
            assert!(
                matches!(
                    result,
                    CommandResult::Error {
                        code: ErrorCode::InvalidCommand,
                        ..
                    }
                ),
                "{transport:?} {range:?} must be refused, got {result:?}"
            );
        }
    });
}

#[test]
fn a_listener_lives_while_used_and_closes_after_its_linger() {
    with_server(|mut uds| async move {
        let a = opened(open(&mut uds, 1, ListenerTransport::Quic, None, 1).await);
        assert_eq!(a.linger_secs, 1);

        let held = dial(&a, &a.token).await.expect("admitted");
        tokio::time::sleep(Duration::from_secs(2)).await;
        let second = dial(&a, &a.token)
            .await
            .expect("a live connection holds the listener open past its linger");
        held.close().await;
        second.close().await;

        // One linger after the last connection leaves, with slack for the
        // server to observe the close.
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(
            dial(&a, &a.token).await.is_err(),
            "an unused listener closes after its linger"
        );
    });
}
