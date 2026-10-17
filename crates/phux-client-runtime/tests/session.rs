//! One-shot sessions against a real server: request/reply with the frames
//! interleaved ahead of each answer, metadata reads, spawn, corked batches,
//! QUIC per-Terminal streams, the WebSocket lane, and refusal
//! classification.

#![allow(clippy::expect_used, reason = "test assertions")]
#![allow(clippy::unwrap_used, reason = "test assertions")]
#![allow(clippy::panic, reason = "test assertions")]
#![allow(clippy::future_not_send, reason = "the server's actors are !Send")]

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use phux_client_runtime::Target;
use phux_client_runtime::session::{Dial, Session, SessionError, control_caps};
use phux_protocol::ids::ResourceId;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::RemoteListenerTransport;
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, Scope, SpawnResult, StateScope, ViewportInfo,
};
use phux_server::{ServerConfig, ServerEnv, ServerRuntime};
use phux_server_testkit::{Spawn, bound_listener_addr, run_local, spawn_server, wait_for_socket};
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout};

/// Idle budget for any single wait on a real server.
const STEP: Duration = Duration::from_secs(20);

const CLIENT: &str = "phux-client-runtime-session-e2e";

type ServerTask = JoinHandle<Result<(), phux_server::ServerError>>;

async fn connect_uds(socket: &Path) -> Session {
    wait_for_socket(socket, STEP).await;
    Session::connect(&Dial::uds(socket), CLIENT.to_owned(), control_caps())
        .await
        .expect("connect over the Unix socket")
}

async fn spawn_cat(session: &mut Session, request_id: u32) -> ResourceId {
    let reply = timeout(
        STEP,
        session.request_spawn(&Spawn::command(&["/bin/cat"]).frame(request_id)),
    )
    .await
    .expect("spawn answered")
    .expect("spawn round trip");
    match reply.into_result_ignoring_interleaved() {
        Ok(SpawnResult::Ok(id)) => id,
        other => panic!("spawn failed: {other:?}"),
    }
}

fn paste(terminal_id: &ResourceId, text: &str) -> FrameKind {
    FrameKind::InputPaste {
        terminal_id: terminal_id.clone(),
        event: PasteEvent {
            trust: PasteTrust::Trusted,
            data: text.as_bytes().to_vec(),
        },
    }
}

/// Read until `terminal`'s output contains `needle`; `STEP` is idle time.
async fn wait_for_output(session: &mut Session, terminal: &ResourceId, needle: &str) {
    let mut seen = Vec::new();
    loop {
        let frame = timeout(STEP, session.recv())
            .await
            .unwrap_or_else(|_| panic!("no {needle:?} in {:?}", String::from_utf8_lossy(&seen)))
            .expect("recv");
        if let FrameKind::ResourceOutput {
            terminal_id, bytes, ..
        } = frame
            && &terminal_id == terminal
        {
            seen.extend_from_slice(&bytes);
            if String::from_utf8_lossy(&seen).contains(needle) {
                return;
            }
        }
    }
}

fn is_bootstrap_of(frame: &FrameKind, terminal: &ResourceId) -> bool {
    matches!(frame, FrameKind::BootstrapBegin { terminal_id, .. } if terminal_id == terminal)
}

async fn stop(shutdown: oneshot::Sender<()>, server: ServerTask) {
    drop(shutdown);
    timeout(STEP, server)
        .await
        .expect("server stops")
        .expect("server task")
        .expect("server exit");
}

/// The pid, server identity, `GET_STATE`, spawn, the attach bootstrap the
/// server pushes ahead of its ack, and input over the control stream.
#[test]
fn a_unix_session_requests_spawns_attaches_and_types() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("s.sock");
        let (shutdown, server) = spawn_server(socket.clone(), Some("main"));
        let mut session = connect_uds(&socket).await;
        assert_eq!(
            session.peer_pid(),
            Some(i32::try_from(std::process::id()).unwrap()),
            "the in-process server is this process"
        );
        assert!(session.server_id().is_some_and(|id| !id.is_empty()));
        assert!(session.negotiated_bootstrap().is_some());
        assert!(!session.multistream_enabled());

        let state = session
            .request(
                1,
                Command::GetState {
                    scope: StateScope::Server,
                },
            )
            .await
            .expect("GET_STATE");
        let CommandResult::OkWith(CommandValue::State(snapshot)) = state.result() else {
            panic!("GET_STATE answered {:?}", state.result());
        };
        assert!(
            snapshot.sessions.iter().any(|s| s.name == "main"),
            "{snapshot:?}"
        );

        let cat = spawn_cat(&mut session, 2).await;
        // An observer could not type; this attach must.
        assert!(session.observer_role_policy().is_some());
        let attach = Command::attach_terminal(cat.clone(), None);
        let (ack, interleaved) = session
            .request(3, attach)
            .await
            .expect("ATTACH_RESOURCE")
            .into_parts();
        assert!(matches!(ack, CommandResult::Ok), "{ack:?}");
        assert!(
            interleaved.iter().any(|frame| is_bootstrap_of(frame, &cat)),
            "the bootstrap pushed ahead of the ack is kept: {interleaved:?}"
        );

        session
            .send(&paste(&cat, "over-uds\n"))
            .await
            .expect("type");
        wait_for_output(&mut session, &cat, "over-uds").await;
        session.shutdown().await;
        stop(shutdown, server).await;
    });
}

/// Metadata writes read back through every read shape, and a subscribed
/// change notice is returned ahead of the read it preceded.
#[test]
fn a_unix_session_reads_metadata_with_the_notices_ahead_of_each_answer() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("m.sock");
        let (shutdown, server) = spawn_server(socket.clone(), Some("main"));
        let mut session = connect_uds(&socket).await;
        let key = "test.session/v1".to_owned();
        session
            .send(&FrameKind::SubscribeMetadata {
                scope: Scope::Global,
                key: key.clone(),
            })
            .await
            .expect("subscribe");
        session
            .send(&FrameKind::SetMetadata {
                request_id: 10,
                scope: Scope::Global,
                key: key.clone(),
                value: b"v1".to_vec(),
            })
            .await
            .expect("set");

        let (value, interleaved) = session
            .request_metadata(11, Scope::Global, key.clone())
            .await
            .expect("GET_METADATA")
            .into_parts();
        assert_eq!(value.expect("answered"), Some(b"v1".to_vec()));
        assert!(
            interleaved.iter().any(|frame| matches!(
                frame,
                FrameKind::MetadataChanged { key: changed, .. } if *changed == key
            )),
            "the change notice is kept: {interleaved:?}"
        );

        let keys = session
            .request_metadata_keys(12, Scope::Global)
            .await
            .expect("LIST_METADATA")
            .into_result_ignoring_interleaved()
            .expect("answered");
        assert!(keys.contains(&key), "{keys:?}");

        let reads = vec![
            (Scope::Global, key.clone()),
            (Scope::Global, "test.session/unset".to_owned()),
        ];
        let answers = session
            .request_metadata_many(20, reads)
            .await
            .expect("pipelined reads")
            .into_result_ignoring_interleaved();
        assert_eq!(answers.len(), 2);
        assert_eq!(answers[0].as_ref().expect("set"), &Some(b"v1".to_vec()));
        assert_eq!(answers[1].as_ref().expect("unset"), &None);
        session.shutdown().await;
        stop(shutdown, server).await;
    });
}

/// A corked batch reaches the server as one write and every request in it
/// is answered, in order.
#[test]
fn a_corked_batch_of_requests_is_answered_in_order() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("c.sock");
        let (shutdown, server) = spawn_server(socket.clone(), Some("main"));
        let mut session = connect_uds(&socket).await;
        session.cork();
        for nonce in 1..=3 {
            session
                .send(&FrameKind::Ping { nonce })
                .await
                .expect("corked");
        }
        session.uncork().await.expect("ship the batch");
        let mut pongs = Vec::new();
        while pongs.len() < 3 {
            if let FrameKind::Pong { nonce } = timeout(STEP, session.recv())
                .await
                .expect("answered")
                .expect("recv")
            {
                pongs.push(nonce);
            }
        }
        assert_eq!(pongs, vec![1, 2, 3]);
        assert!(session.try_recv().expect("nothing pending").is_none());
        session.shutdown().await;
        stop(shutdown, server).await;
    });
}

/// A server on `socket` that also listens on loopback QUIC; returns the
/// address the listener bound. `tokens` names a bearer store the listener
/// then requires, as an enrolled host's does.
async fn quic_server(
    tmp: &TempDir,
    socket: &Path,
    tokens: Option<&Path>,
) -> (oneshot::Sender<()>, ServerTask, SocketAddr) {
    let cert = tmp.path().join("cert.pem");
    let key = tmp.path().join("key.pem");
    phux_server::transport::tls::ensure_self_signed(&cert, &key).expect("self-signed cert");
    let config = ServerConfig {
        socket_path: socket.to_path_buf(),
        pre_seeded_session: Some("main".to_owned()),
        seed_with_pty: false,
        seed_command: None,
        // Only what this test sets: never the shell's `PHUX_*`.
        env: ServerEnv {
            tls_cert: Some(cert),
            tls_key: Some(key),
            ws_secure: tokens.is_some(),
            ws_tokens: tokens.map(Path::to_path_buf),
            ..ServerEnv::default()
        },
        ..ServerConfig::with_default_socket()
    };
    let (shutdown, stopped) = oneshot::channel::<()>();
    let server = tokio::task::spawn_local(async move {
        ServerRuntime::new(config)
            .listen_quic(SocketAddr::from(([127, 0, 0, 1], 0)))
            .run_async(async move {
                let _ = stopped.await;
            })
            .await
    });
    let addr = bound_listener_addr(socket, RemoteListenerTransport::Quic).await;
    (shutdown, server, addr)
}

/// QUIC multistream: a Terminal has no route until it is bound, its input
/// rides its own stream once bound, its content comes back on it, and an
/// unbind takes the route away again. The viewport barrier settles.
#[test]
fn a_quic_session_binds_routes_and_unbinds_terminal_streams() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("q.sock");
        let (shutdown, server, addr) = quic_server(&tmp, &socket, None).await;
        let target = Target::quic(addr.to_string());
        let mut session = Session::connect_target(&target, CLIENT.to_owned(), control_caps())
            .await
            .expect("connect over QUIC");
        assert!(session.multistream_enabled(), "QUIC_STREAMS negotiated");
        assert_eq!(session.peer_pid(), None);

        let cat = spawn_cat(&mut session, 1).await;
        assert!(!session.can_route_terminal(&cat), "unbound");
        let refused = session.send(&paste(&cat, "early\n")).await;
        assert!(
            matches!(refused, Err(SessionError::Protocol(_))),
            "{refused:?}"
        );

        let attach = Command::attach_terminal(cat.clone(), None);
        let ack = session
            .request(2, attach)
            .await
            .expect("ATTACH_RESOURCE")
            .into_result_ignoring_interleaved();
        assert!(matches!(ack, CommandResult::Ok), "{ack:?}");
        session.bind_terminal(&cat).await.expect("bind");
        session
            .bind_terminal(&cat)
            .await
            .expect("a second bind is a no-op");
        assert!(session.can_route_terminal(&cat));

        session
            .send(&FrameKind::ViewportResize {
                viewport: ViewportInfo::new(100, 30),
            })
            .await
            .expect("the viewport barrier settles");
        session
            .send(&paste(&cat, "over-quic\n"))
            .await
            .expect("type");
        wait_for_output(&mut session, &cat, "over-quic").await;

        session.unbind_terminal(&cat);
        assert!(!session.can_route_terminal(&cat), "unbound again");
        session.bind_terminal(&cat).await.expect("rebind");
        session.unbind_all_terminals();
        assert!(!session.can_route_terminal(&cat));
        timeout(STEP, session.shutdown())
            .await
            .expect("QUIC close drains");
        stop(shutdown, server).await;
    });
}

/// The WebSocket lane: plan a loopback target, negotiate, and round-trip.
#[test]
fn a_websocket_session_round_trips() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("w.sock");
        let config = ServerConfig {
            socket_path: socket.clone(),
            pre_seeded_session: Some("main".to_owned()),
            seed_with_pty: false,
            seed_command: None,
            ..ServerConfig::with_default_socket()
        };
        let (shutdown, stopped) = oneshot::channel::<()>();
        let server = tokio::task::spawn_local(async move {
            ServerRuntime::new(config)
                .listen_ws(SocketAddr::from(([127, 0, 0, 1], 0)))
                .run_async(async move {
                    let _ = stopped.await;
                })
                .await
        });
        let addr = bound_listener_addr(&socket, RemoteListenerTransport::Wss).await;
        let target = Target::ws(format!("ws://{addr}/phux"));
        let mut session = Session::connect_target(&target, CLIENT.to_owned(), control_caps())
            .await
            .expect("connect over WebSocket");
        assert!(!session.multistream_enabled());
        session.cork();
        session
            .send(&FrameKind::Ping { nonce: 77 })
            .await
            .expect("ping");
        session.uncork().await.expect("a no-op off a Unix socket");
        let deadline = Instant::now() + STEP;
        loop {
            assert!(Instant::now() < deadline, "no PONG over WebSocket");
            if matches!(
                session.recv().await.expect("recv"),
                FrameKind::Pong { nonce: 77 }
            ) {
                break;
            }
        }
        session.shutdown().await;
        stop(shutdown, server).await;
    });
}

/// A QUIC listener refusing the bearer token closes the connection with its
/// auth code after the stream opened. The read that sees it is a refusal the
/// caller stops on and words as one, not a lost connection it redials.
#[test]
fn a_refused_quic_token_is_a_fatal_refusal() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("t.sock");
        let tokens = tmp.path().join("tokens");
        phux_server::auth::mint_token(&tokens).expect("a credential the wrong token is not");
        let (shutdown, server, addr) = quic_server(&tmp, &socket, Some(&tokens)).await;
        let mut target = Target::quic(addr.to_string());
        target.token = Some("ab".repeat(32));
        let refused = Session::connect_target(&target, CLIENT.to_owned(), control_caps()).await;
        let error = refused.expect_err("an unknown token");
        assert!(error.is_fatal_refusal(), "{error}");
        assert!(!error.to_string().contains("connection lost"), "{error}");
        stop(shutdown, server).await;
    });
}

/// The reconnecting runtime, attached over QUIC, redials after its token is
/// revoked: the refusal ends the session with a "re-pair" message instead of
/// walking the backoff ladder forever.
#[test]
fn a_revoked_quic_token_ends_an_attached_runtime() {
    use phux_client_runtime::control::{ControlOptions, Status};
    use phux_client_runtime::reconnect::Ladder;
    use phux_client_runtime::{Client, ClientOptions, ConnectOptions, Runtime};
    use phux_protocol::wire::frame::AttachTarget;

    async fn wait_for(client: &Client, what: &str, done: impl Fn(Status) -> bool) {
        let deadline = Instant::now() + STEP;
        while !done(client.status()) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {:?} {:?}",
                client.status(),
                client.last_error()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("r.sock");
        let tokens = tmp.path().join("tokens");
        let minted = phux_server::auth::mint_token(&tokens).expect("a credential");
        let (shutdown, server, addr) = quic_server(&tmp, &socket, Some(&tokens)).await;
        let mut target = Target::quic(addr.to_string());
        target.name = "mini".to_owned();
        target.token = Some(minted.secret().to_owned());
        let options = ClientOptions {
            control: ControlOptions {
                client_name: CLIENT.to_owned(),
                attach: Some(AttachTarget::ByName("main".to_owned())),
                viewport: (40, 8),
                ..ControlOptions::default()
            },
            connect: ConnectOptions {
                ladder: Ladder::AGENT_VERB,
                ..ConnectOptions::default()
            },
        };
        let client = Runtime::connect(target, options).expect("connect");
        wait_for(&client, "the attach", |status| status == Status::Attached).await;

        phux_server::auth::revoke_credential(&tokens, &minted.id).expect("revoke");
        client.resync();
        wait_for(&client, "the refusal", Status::is_terminal).await;
        assert_eq!(client.status(), Status::Failed);
        let error = client.last_error().expect("a refusal message");
        assert!(error.contains("re-pair"), "{error}");
        client.close();
        drop(client);
        stop(shutdown, server).await;
    });
}

/// 401 on the upgrade, as a token gate refusing a token answers it.
async fn refuse_with_401() -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::task::spawn_local(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut request = Vec::new();
            let mut buf = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => request.extend_from_slice(&buf[..read]),
                }
            }
            let _ = stream
                .write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
        }
    });
    port
}

/// Each failure lands in the variant a caller reacts to.
#[test]
fn refusals_are_classified_for_the_caller() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let missing = Session::connect(
            &Dial::uds(&tmp.path().join("absent.sock")),
            CLIENT.to_owned(),
            control_caps(),
        )
        .await;
        assert!(
            matches!(&missing, Err(SessionError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound),
            "{missing:?}"
        );

        let port = refuse_with_401().await;
        let gate = Target::ws(format!("ws://127.0.0.1:{port}"));
        let refused = Session::connect_target(&gate, CLIENT.to_owned(), control_caps()).await;
        let error = refused.expect_err("401");
        assert!(error.is_fatal_refusal(), "a 401 is not retried: {error}");

        let unpinned = Target::quic("192.0.2.1:8788");
        let plan = Session::connect_target(&unpinned, CLIENT.to_owned(), control_caps()).await;
        assert!(
            matches!(&plan, Err(SessionError::Connect(detail)) if detail.contains("certificate pin")),
            "{plan:?}"
        );
    });
}
