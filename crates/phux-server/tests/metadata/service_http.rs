//! `SERVICE_HTTP_*` over the real socket: the bit is advertised and
//! refusals are typed.

#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` demands pub(crate) in a test-binary module"
)]

use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::service_http::ServiceHttpRequest;
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind};
use tokio::net::UnixStream;

use phux_server_testkit::{SpawnedServer, command, run_local};

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
            (
                "unrouted satellite",
                satellite,
                ErrorCode::UnsupportedSatelliteRoute,
            ),
            ("zero-byte read", read(0), ErrorCode::InvalidCommand),
        ];
        for ((label, refused, code), request_id) in refusals.into_iter().zip(1..) {
            match command(&mut stream, request_id, refused).await {
                CommandResult::Error { code: got, .. } => assert_eq!(got, code, "{label}"),
                other => panic!("{label}: expected {code:?}, got {other:?}"),
            }
        }
        let close = Command::ServiceHttpClose { exchange: 999 };
        assert_eq!(command(&mut stream, 3, close).await, CommandResult::Ok);
    }));
}
