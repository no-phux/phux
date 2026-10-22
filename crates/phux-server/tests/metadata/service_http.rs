//! `SERVICE_HTTP_*` over the real socket: the bit is advertised, refusals
//! are typed, and (ignored, needs the host's `OpenCode`) a live exchange with
//! the `OpenCode` background service.

#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` demands pub(crate) in a test-binary module"
)]

use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::service_http::{ServiceHttpChunk, ServiceHttpHead, ServiceHttpRequest};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode, FrameKind};
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{
    command, join_after_shutdown, recv_until, run_local, send_frame, spawn_server,
};

use crate::common::{connect_as, full_caps};

fn get(service: &str, target: &str) -> Command {
    Command::ServiceHttpOpen {
        request: Box::new(ServiceHttpRequest {
            host: None,
            service: service.to_owned(),
            method: "GET".to_owned(),
            target: target.to_owned(),
            headers: vec![("accept".to_owned(), "application/json".to_owned())],
            body: Vec::new(),
        }),
    }
}

fn code(result: &CommandResult) -> ErrorCode {
    match result {
        CommandResult::Error { code, .. } => *code,
        other => panic!("expected an error, got {other:?}"),
    }
}

async fn serve(body: impl AsyncFnOnce(UnixStream, FrameKind)) {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("phux.sock");
    let (shutdown, server) = spawn_server(socket.clone(), None);
    let (stream, hello_ok) = connect_as(&socket, "service-http-test", full_caps()).await;
    body(stream, hello_ok).await;
    join_after_shutdown(shutdown, server).await;
}

#[test]
fn the_bit_is_advertised_and_refusals_are_typed() {
    run_local(serve(async |mut stream, hello_ok| {
        let FrameKind::HelloOk { server_caps, .. } = hello_ok else {
            unreachable!()
        };
        assert!(
            server_caps
                .features_ext
                .contains(ServerFeatureExt::HostServiceHttp)
        );

        let unknown = command(&mut stream, 1, get("nginx", "/")).await;
        assert_eq!(code(&unknown), ErrorCode::NotFound);
        let bad_target = command(&mut stream, 2, get("opencode", "api")).await;
        assert_eq!(code(&bad_target), ErrorCode::InvalidCommand);
        let satellite = Command::ServiceHttpOpen {
            request: Box::new(ServiceHttpRequest {
                host: Some("ghost".to_owned()),
                service: "opencode".to_owned(),
                method: "GET".to_owned(),
                target: "/".to_owned(),
                headers: Vec::new(),
                body: Vec::new(),
            }),
        };
        let unrouted = command(&mut stream, 3, satellite).await;
        assert_eq!(code(&unrouted), ErrorCode::UnsupportedSatelliteRoute);

        let read = Command::ServiceHttpRead {
            exchange: 999,
            max_bytes: 1,
        };
        assert_eq!(
            code(&command(&mut stream, 4, read).await),
            ErrorCode::NotFound
        );
        let empty = Command::ServiceHttpRead {
            exchange: 999,
            max_bytes: 0,
        };
        assert_eq!(
            code(&command(&mut stream, 5, empty).await),
            ErrorCode::InvalidCommand
        );
        let close = Command::ServiceHttpClose { exchange: 999 };
        assert_eq!(command(&mut stream, 6, close).await, CommandResult::Ok);
    }));
}

async fn open(stream: &mut UnixStream, request_id: u32, target: &str) -> ServiceHttpHead {
    match command(stream, request_id, get("opencode", target)).await {
        CommandResult::OkWith(CommandValue::ServiceHttpHead(head)) => head,
        other => panic!("open {target}: {other:?}"),
    }
}

async fn read(stream: &mut UnixStream, request_id: u32, exchange: u64) -> ServiceHttpChunk {
    let read = Command::ServiceHttpRead {
        exchange,
        max_bytes: 64 * 1024,
    };
    match command(stream, request_id, read).await {
        CommandResult::OkWith(CommandValue::ServiceHttpChunk(chunk)) => chunk,
        other => panic!("read {exchange}: {other:?}"),
    }
}

async fn body(stream: &mut UnixStream, first_request: u32, exchange: u64) -> serde_json::Value {
    let mut bytes = Vec::new();
    for request_id in first_request.. {
        let chunk = read(stream, request_id, exchange).await;
        bytes.extend(chunk.bytes);
        if chunk.end {
            break;
        }
    }
    serde_json::from_slice(&bytes).unwrap()
}

/// The host's real `OpenCode` service, through a private server: `GET
/// /api/info` and `GET /api/session` answer JSON, and a read held on the
/// event stream neither blocks the next command nor outlives its close.
#[test]
#[ignore = "needs the host's OpenCode (`opencode service status`)"]
fn live_opencode_answers_through_the_connection() {
    run_local(serve(async |mut stream, _| {
        let info = open(&mut stream, 1, "/api/info").await;
        assert_eq!(info.status, 200);
        let info = body(&mut stream, 100, info.exchange).await;
        assert!(info["version"].is_string(), "{info}");

        let sessions = open(&mut stream, 2, "/api/session").await;
        assert_eq!(sessions.status, 200);
        let sessions = body(&mut stream, 200, sessions.exchange).await;
        assert!(sessions.is_array() || sessions.is_object(), "{sessions}");

        let events = open(&mut stream, 3, "/api/event").await;
        assert_eq!(events.status, 200);
        let first = read(&mut stream, 4, events.exchange).await;
        assert!(!first.end, "the event stream stays open");

        // A read now waits for the next event; the close is answered at
        // once and ends that read.
        let held = Command::ServiceHttpRead {
            exchange: events.exchange,
            max_bytes: 1024,
        };
        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 5,
                command: held,
            },
        )
        .await;
        let close = Command::ServiceHttpClose {
            exchange: events.exchange,
        };
        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 6,
                command: close,
            },
        )
        .await;
        let mut answered = Vec::new();
        while answered.len() < 2 {
            let (request_id, result) = recv_until(&mut stream, |_, frame| match frame {
                FrameKind::CommandResult { request_id, result } => Some((request_id, result)),
                _ => None,
            })
            .await;
            answered.push(request_id);
            if request_id == 6 {
                assert_eq!(result, CommandResult::Ok);
            }
        }
        assert!(
            answered.contains(&5) && answered.contains(&6),
            "{answered:?}"
        );
    }));
}
