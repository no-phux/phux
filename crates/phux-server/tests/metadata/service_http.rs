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
use tokio::net::UnixStream;

use phux_server_testkit::{SpawnedServer, command, recv_until, run_local};

use crate::common::{advertises, connect_as, full_caps};

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

async fn serve(body: impl AsyncFnOnce(UnixStream, FrameKind)) {
    let server = SpawnedServer::start(None, |_| {});
    let (stream, hello_ok) = connect_as(&server.socket, "service-http-test", full_caps()).await;
    body(stream, hello_ok).await;
    server.stop().await;
}

#[test]
fn the_bit_is_advertised_and_refusals_are_typed() {
    run_local(serve(async |mut stream, hello_ok| {
        assert!(advertises(&hello_ok, ServerFeatureExt::HostServiceHttp));

        let mut satellite = get("opencode", "/");
        if let Command::ServiceHttpOpen { request } = &mut satellite {
            request.host = Some("ghost".to_owned());
            request.headers.clear();
        }
        let read = |max_bytes| Command::ServiceHttpRead {
            exchange: 999,
            max_bytes,
        };
        let refusals = [
            ("unknown service", get("nginx", "/"), ErrorCode::NotFound),
            (
                "relative target",
                get("opencode", "api"),
                ErrorCode::InvalidCommand,
            ),
            (
                "unrouted satellite",
                satellite,
                ErrorCode::UnsupportedSatelliteRoute,
            ),
            ("unknown exchange", read(1), ErrorCode::NotFound),
            ("zero-byte read", read(0), ErrorCode::InvalidCommand),
        ];
        for ((label, refused, code), request_id) in refusals.into_iter().zip(1..) {
            match command(&mut stream, request_id, refused).await {
                CommandResult::Error { code: got, .. } => assert_eq!(got, code, "{label}"),
                other => panic!("{label}: expected {code:?}, got {other:?}"),
            }
        }
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
        phux_server_testkit::send_command(&mut stream, 5, held).await;
        let close = Command::ServiceHttpClose {
            exchange: events.exchange,
        };
        phux_server_testkit::send_command(&mut stream, 6, close).await;
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
