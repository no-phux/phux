//! `FORWARD_*` over the real socket: the bit follows `[forward] tcp`,
//! bytes cross the dispatcher both ways, a parked read delays nothing, the
//! slot limit and the allow-list hold, and a dropped connection takes its
//! forwards with it.

#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` demands pub(crate) in a test-binary module"
)]

use std::time::Duration;

use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::forward::{ForwardChunk, ForwardOp, MAX_FORWARD_CHUNK_BYTES};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, ErrorCode, FrameKind, StateScope,
};
use phux_server::runtime::ServerConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixStream};

use phux_server_testkit::{SpawnedServer, await_command_result, command, run_local, send_frame};

use crate::common::{connect_as, full_caps};

const CHUNK: u32 = 256 * 1024;

const fn forward(op: ForwardOp) -> Command {
    Command::Forward { op }
}

fn code(result: &CommandResult) -> ErrorCode {
    match result {
        CommandResult::Error { code, .. } => *code,
        other => panic!("expected an error, got {other:?}"),
    }
}

async fn serve(
    configure: impl FnOnce(&mut ServerConfig),
    body: impl AsyncFnOnce(UnixStream, FrameKind),
) {
    let server = SpawnedServer::start(None, configure);
    let (stream, hello_ok) = connect_as(&server.socket, "forward-test", full_caps()).await;
    body(stream, hello_ok).await;
    server.stop().await;
}

fn advertised(hello_ok: &FrameKind) -> bool {
    let FrameKind::HelloOk { server_caps, .. } = hello_ok else {
        unreachable!()
    };
    server_caps
        .features_ext
        .contains(ServerFeatureExt::ForwardTcp)
}

async fn listener() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

async fn open(stream: &mut UnixStream, request_id: u32, addr: &str, port: u16) -> CommandResult {
    let addr = addr.to_owned();
    command(stream, request_id, forward(ForwardOp::Open { addr, port })).await
}

async fn open_ok(stream: &mut UnixStream, request_id: u32, port: u16) -> u64 {
    match open(stream, request_id, "127.0.0.1", port).await {
        CommandResult::OkWith(CommandValue::ForwardOpened(id)) => id,
        other => panic!("open: {other:?}"),
    }
}

const fn read(id: u64) -> Command {
    forward(ForwardOp::Read {
        id,
        max_bytes: CHUNK,
    })
}

fn chunk(result: CommandResult) -> ForwardChunk {
    match result {
        CommandResult::OkWith(CommandValue::ForwardChunk(chunk)) => chunk,
        other => panic!("read: {other:?}"),
    }
}

async fn send(stream: &mut UnixStream, request_id: u32, command: Command) {
    let frame = FrameKind::Command {
        request_id,
        command,
    };
    send_frame(stream, &frame).await;
}

#[test]
fn bytes_cross_the_socket_and_a_parked_read_delays_nothing() {
    run_local(serve(
        |_| {},
        async |mut stream, hello_ok| {
            assert!(advertised(&hello_ok));
            let (listener, port) = listener().await;
            let id = open_ok(&mut stream, 1, port).await;
            let (mut theirs, _) = listener.accept().await.unwrap();

            // The read parks: the target has sent nothing. A second read is
            // refused, and a write, an unrelated command, and the target's
            // answer all go through while it waits.
            send(&mut stream, 2, read(id)).await;
            let second = command(&mut stream, 3, read(id)).await;
            assert_eq!(code(&second), ErrorCode::ResourceExhausted);
            let started = std::time::Instant::now();
            let write = forward(ForwardOp::Write {
                id,
                bytes: b"ping".to_vec(),
                fin: false,
            });
            assert_eq!(command(&mut stream, 4, write).await, CommandResult::Ok);
            let state = Command::GetState {
                scope: StateScope::Server,
            };
            assert!(matches!(
                command(&mut stream, 5, state).await,
                CommandResult::OkWith(_)
            ));
            let mut ping = [0; 4];
            theirs.read_exact(&mut ping).await.unwrap();
            assert_eq!(&ping, b"ping");
            theirs.write_all(b"pong").await.unwrap();
            let parked = chunk(await_command_result(&mut stream, 2).await);
            assert_eq!(parked.bytes, b"pong");
            assert!(!parked.eof);
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "nothing waited"
            );

            // The largest chunk fits a frame each way.
            let big = vec![0xa5; MAX_FORWARD_CHUNK_BYTES];
            let write = forward(ForwardOp::Write {
                id,
                bytes: big.clone(),
                fin: true,
            });
            send(&mut stream, 6, write).await;
            let mut seen = Vec::new();
            theirs.read_to_end(&mut seen).await.unwrap();
            assert_eq!(seen, big);
            assert_eq!(
                await_command_result(&mut stream, 6).await,
                CommandResult::Ok
            );
            theirs.write_all(&big).await.unwrap();
            drop(theirs);
            let mut back = Vec::new();
            for request_id in 7.. {
                let next = chunk(command(&mut stream, request_id, read(id)).await);
                back.extend(next.bytes);
                if next.eof {
                    break;
                }
            }
            assert_eq!(back, big);

            let close = forward(ForwardOp::Close { id });
            assert_eq!(
                command(&mut stream, 100, close.clone()).await,
                CommandResult::Ok
            );
            assert_eq!(command(&mut stream, 101, close).await, CommandResult::Ok);
            let gone = command(&mut stream, 102, read(id)).await;
            assert_eq!(code(&gone), ErrorCode::NotFound);
        },
    ));
}

#[test]
fn tcp_false_hides_the_bit_and_refuses_the_commands() {
    run_local(serve(
        |cfg| cfg.forward.tcp = false,
        async |mut stream, hello_ok| {
            assert!(!advertised(&hello_ok));
            let (_listener, port) = listener().await;
            let refused = open(&mut stream, 1, "127.0.0.1", port).await;
            assert_eq!(code(&refused), ErrorCode::Unsupported);
            let refused = command(&mut stream, 2, read(1)).await;
            assert_eq!(code(&refused), ErrorCode::Unsupported);
        },
    ));
}

#[test]
fn only_loopback_and_the_listed_target_are_dialed() {
    // Not one of the three loopback spellings, so it needs the allow-list;
    // it still reaches this machine, which keeps the test offline.
    const MAPPED: &str = "::ffff:127.0.0.1";
    run_local(async {
        let (listed, listed_port) = listener().await;
        let (_unlisted, unlisted_port) = listener().await;
        serve(
            |cfg| cfg.forward.allow = vec![format!("[{MAPPED}]:{listed_port}")],
            async |mut stream, _| {
                for (request_id, addr) in [(1, "example.com"), (2, "127.0.0.2"), (3, "LOCALHOST")] {
                    let refused = open(&mut stream, request_id, addr, 80).await;
                    assert_eq!(code(&refused), ErrorCode::PermissionDenied, "{addr}");
                }
                let CommandResult::Error { message, .. } =
                    open(&mut stream, 4, "example.com", 80).await
                else {
                    unreachable!()
                };
                assert_eq!(
                    message,
                    "example.com:80 is not on this server's loopback: phux forwards only to \
                     localhost, 127.0.0.1, or ::1 unless the server's `[forward] allow` lists \
                     the target"
                );

                let other_port = open(&mut stream, 5, MAPPED, unlisted_port).await;
                assert_eq!(code(&other_port), ErrorCode::PermissionDenied);
                let opened = open(&mut stream, 6, MAPPED, listed_port).await;
                assert!(
                    matches!(
                        opened,
                        CommandResult::OkWith(CommandValue::ForwardOpened(_))
                    ),
                    "{opened:?}"
                );
                listed.accept().await.unwrap();
            },
        )
        .await;
    });
}

#[test]
fn the_thirty_third_forward_is_refused() {
    run_local(serve(
        |_| {},
        async |mut stream, _| {
            let (_listener, port) = listener().await;
            let mut ids = Vec::new();
            for request_id in 1..=32 {
                ids.push(open_ok(&mut stream, request_id, port).await);
            }
            let over = open(&mut stream, 33, "127.0.0.1", port).await;
            assert_eq!(code(&over), ErrorCode::ResourceExhausted);
            let close = forward(ForwardOp::Close { id: ids[0] });
            assert_eq!(command(&mut stream, 34, close).await, CommandResult::Ok);
            open_ok(&mut stream, 35, port).await;
        },
    ));
}

#[test]
fn a_dropped_connection_closes_the_forwards_it_opened() {
    run_local(serve(
        |_| {},
        async |mut stream, _| {
            let (listener, port) = listener().await;
            open_ok(&mut stream, 1, port).await;
            open_ok(&mut stream, 2, port).await;
            let (mut first, _) = listener.accept().await.unwrap();
            let (mut second, _) = listener.accept().await.unwrap();
            // One with a read parked on it, one idle.
            send(&mut stream, 3, read(1)).await;
            drop(stream);
            for theirs in [&mut first, &mut second] {
                let mut byte = [0; 1];
                let ended = tokio::time::timeout(Duration::from_secs(10), theirs.read(&mut byte));
                assert_eq!(ended.await.unwrap().unwrap(), 0, "the target saw the end");
            }
        },
    ));
}
